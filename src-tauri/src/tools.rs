//! Localización y descarga de las herramientas externas (ffmpeg y yt-dlp).
//!
//! No viajan dentro de la app: se descargan de sus repos oficiales a la carpeta de
//! datos del usuario la primera vez que se abre. Si el sistema ya las tiene, se usan
//! esas. Empaquetarlas congelaría la copia de yt-dlp en la fecha de compilación, y
//! YouTube deja de servir a las versiones viejas a los pocos meses.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use tauri::{AppHandle, Manager};

use crate::{log, progress};

/// Versión fijada de los binarios estáticos de ffmpeg. Se sube a mano tras comprobar
/// que la nueva sigue trayendo libmp3lame, aac, libopus, libvorbis, flac y pcm_s16le.
const FFMPEG_TAG: &str = "b6.1.1";

pub const EXE: &str = if cfg!(windows) { ".exe" } else { "" };

/// Carpeta donde se guardan las herramientas descargadas por la app.
pub fn bin_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("bin")
}

/// Busca una herramienta: primero la que ha descargado la app, que es la que
/// mantiene al día, y si no hay, la del PATH del sistema.
///
/// Quien no pueda descargar (un cortafuegos de empresa) puede dejar el binario a
/// mano en la carpeta que la ventana enseña al arrancar, que es `bin_dir`.
pub fn find(app: &AppHandle, name: &str) -> Option<PathBuf> {
    find_in(&[bin_dir(app)], name)
}

fn find_in(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    let file = format!("{name}{EXE}");
    dirs.iter()
        .map(|d| d.join(&file))
        .find(|p| p.is_file())
        .or_else(|| which(&file))
}

fn which(file: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(file))
        .find(|p| p.is_file())
}

/// Binarios oficiales a descargar según el sistema: (nombre final, URL).
fn sources() -> Vec<(&'static str, String)> {
    let ff = |arch: &str| {
        format!("https://github.com/eugeneware/ffmpeg-static/releases/download/{FFMPEG_TAG}/ffmpeg-{arch}")
    };
    let yt =
        |name: &str| format!("https://github.com/yt-dlp/yt-dlp/releases/latest/download/{name}");
    if cfg!(windows) {
        vec![
            ("ffmpeg.exe", ff("win32-x64")),
            ("yt-dlp.exe", yt("yt-dlp.exe")),
        ]
    } else if cfg!(target_os = "macos") {
        let arch = if cfg!(target_arch = "aarch64") {
            "darwin-arm64"
        } else {
            "darwin-x64"
        };
        vec![("ffmpeg", ff(arch)), ("yt-dlp", yt("yt-dlp_macos"))]
    } else {
        let arch = if cfg!(target_arch = "aarch64") {
            "linux-arm64"
        } else {
            "linux-x64"
        };
        vec![("ffmpeg", ff(arch)), ("yt-dlp", yt("yt-dlp_linux"))]
    }
}

/// Versión instalada de una herramienta, para poder enseñarla en la ventana.
pub fn version(bin: &Path) -> Option<String> {
    let stem = bin.file_stem()?.to_string_lossy().to_lowercase();
    // ffmpeg rechaza «--version» (sale con error y sin stdout); quiere un guion.
    let flag = if stem.starts_with("ffmpeg") || stem.starts_with("ffprobe") {
        "-version"
    } else {
        "--version"
    };
    let mut cmd = Command::new(bin);
    cmd.arg(flag);
    crate::hide_console(&mut cmd);
    let out = cmd.output().ok()?;
    let first = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()?
        .trim()
        .to_string();
    // yt-dlp imprime sólo el número; ffmpeg, «ffmpeg version 6.1.1 Copyright…».
    Some(match first.strip_prefix("ffmpeg version ") {
        Some(rest) => rest.split_whitespace().next().unwrap_or(rest).to_string(),
        None => first,
    })
}

/// Baja la última versión de yt-dlp aunque ya hubiera una, y a la carpeta propia
/// de la app, que tiene prioridad sobre el PATH.
///
/// No es un capricho: YouTube cambia su extracción cada pocas semanas y las
/// versiones viejas empiezan a dar «HTTP Error 403: Forbidden». Si el sistema
/// trae un yt-dlp antiguo, la app tiene que poder pasar por encima.
pub fn update_ytdlp(app: &AppHandle) -> Result<(), String> {
    let dir = bin_dir(app);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("no se pudo crear {}: {e}", dir.display()))?;
    let (file, url) = sources()
        .into_iter()
        .find(|(name, _)| name.starts_with("yt-dlp"))
        .ok_or("no hay descarga de yt-dlp para este sistema")?;
    log(
        app,
        format!("Descargando la última versión de yt-dlp de {url}"),
    );
    fetch(app, &url, &dir.join(file), "yt-dlp")?;
    let nueva = version(&dir.join(file)).unwrap_or_else(|| "?".into());
    log(
        app,
        format!("✔ yt-dlp {nueva} instalado en {}", dir.display()),
    );
    Ok(())
}

/// Descarga las herramientas que falten. Se llama desde un hilo aparte.
pub fn install(app: &AppHandle) -> Result<(), String> {
    let dir = bin_dir(app);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("no se pudo crear {}: {e}", dir.display()))?;
    for (file, url) in sources() {
        let name = file.trim_end_matches(".exe");
        if find(app, name).is_some() {
            log(app, format!("{name} ya disponible, no se descarga"));
            continue;
        }
        log(app, format!("Descargando {name} de {url}"));
        fetch(app, &url, &dir.join(file), name)?;
        log(app, format!("✔ {name} listo"));
    }
    log(app, format!("Herramientas en {}", dir.display()));
    Ok(())
}

/// Descarga a un fichero `.part` y renombra al terminar, para no dejar
/// binarios a medias si se corta la conexión.
fn fetch(app: &AppHandle, url: &str, dst: &Path, name: &str) -> Result<(), String> {
    let resp = ureq::get(url)
        .call()
        .map_err(|e| format!("no se pudo descargar {name}: {e}"))?;
    let total: u64 = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let tmp = dst.with_extension("part");
    let mut out = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    let mut reader = resp.into_body().into_reader();
    let mut buf = vec![0u8; 1 << 16];
    let mut done: u64 = 0;
    let mut next_report: u64 = 0;
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        if done >= next_report {
            next_report = done + (1 << 21); // informa cada 2 MB
            let frac = if total > 0 {
                done as f64 / total as f64
            } else {
                0.0
            };
            let mb = |b: u64| b as f64 / 1_048_576.0;
            let txt = if total > 0 {
                format!("{name}: {:.0} de {:.0} MB", mb(done), mb(total))
            } else {
                format!("{name}: {:.0} MB", mb(done))
            };
            progress(app, frac, txt);
        }
    }
    drop(out);
    std::fs::rename(&tmp, dst).map_err(|e| e.to_string())?;
    set_executable(dst);
    Ok(())
}

#[cfg(unix)]
fn set_executable(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755));
}

#[cfg(not(unix))]
fn set_executable(_p: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// La carpeta propia de la app tiene que ganarle al PATH: es donde cae la
    /// versión nueva al pulsar «Actualizar», y si no gana el botón no sirve de nada.
    #[test]
    fn la_carpeta_de_la_app_manda_sobre_el_path() {
        let base = std::env::temp_dir().join("audix-test-find");
        let _ = std::fs::remove_dir_all(&base);
        let bin = base.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let file = format!("ffmpeg{EXE}");
        std::fs::write(bin.join(&file), b"falso").unwrap();

        // Aunque el sistema tenga ffmpeg en el PATH, gana el de la carpeta de la app.
        assert!(
            which(&file).is_some(),
            "este equipo debería tener ffmpeg en el PATH"
        );
        assert_eq!(
            find_in(std::slice::from_ref(&bin), "ffmpeg"),
            Some(bin.join(&file))
        );
        // Y si no está en ninguna de las carpetas propias, se cae al PATH.
        assert_eq!(
            find_in(&[base.join("no-existe")], "ffmpeg"),
            which(&file),
            "sin copia propia debe usar la del sistema"
        );
        assert!(find_in(std::slice::from_ref(&bin), "programa-que-no-existe-xyz").is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// La versión se enseña en la ventana y es el dato con el que se diagnostica
    /// un 403 de YouTube, así que tiene que salir limpia, sin el «ffmpeg version».
    #[test]
    fn lee_la_version_de_las_herramientas() {
        for name in ["ffmpeg", "yt-dlp"] {
            let Some(bin) = which(&format!("{name}{EXE}")) else {
                eprintln!("sin {name} en el PATH, se omite");
                continue;
            };
            let v = version(&bin).unwrap_or_default();
            assert!(!v.is_empty(), "{name} no devolvió versión");
            assert!(!v.contains(' '), "{name}: versión con ruido: {v:?}");
            // Valen «2026.10.26», «n9.0.1» o «7.0.2-static»: lo que no vale es
            // que se cuele la frase entera de ffmpeg.
            assert!(
                v.len() < 32 && v.chars().any(|c| c.is_ascii_digit()),
                "{name}: versión con pinta rara: {v:?}"
            );
        }
    }

    #[test]
    fn hay_descargas_para_este_sistema() {
        let s = sources();
        assert_eq!(s.len(), 2, "hacen falta ffmpeg y yt-dlp");
        assert!(s
            .iter()
            .all(|(_, url)| url.starts_with("https://github.com/")));
        assert!(s.iter().any(|(name, _)| name.starts_with("ffmpeg")));
        assert!(s.iter().any(|(name, _)| name.starts_with("yt-dlp")));
    }
}
