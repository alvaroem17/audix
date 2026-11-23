//! Localización y descarga de las herramientas externas (ffmpeg y yt-dlp).
//!
//! La app no las lleva compiladas dentro: en la versión portable viajan en `bin/`
//! junto al ejecutable, y en la instalada se descargan de sus repos oficiales a la
//! carpeta de datos del usuario. Si el sistema ya las tiene, se usan esas.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

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

/// Busca una herramienta por orden: descargada por la app, junto al ejecutable
/// (versión portable), dentro del paquete .app de macOS, y por último el PATH.
pub fn find(app: &AppHandle, name: &str) -> Option<PathBuf> {
    let file = format!("{name}{EXE}");
    let mut dirs = vec![bin_dir(app)];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            dirs.push(d.join("bin"));
            dirs.push(d.to_path_buf());
            dirs.push(d.join("../Resources/bin"));
        }
    }
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
    let yt = |name: &str| {
        format!("https://github.com/yt-dlp/yt-dlp/releases/latest/download/{name}")
    };
    if cfg!(windows) {
        vec![
            ("ffmpeg.exe", ff("win32-x64")),
            ("yt-dlp.exe", yt("yt-dlp.exe")),
        ]
    } else if cfg!(target_os = "macos") {
        let arch = if cfg!(target_arch = "aarch64") { "darwin-arm64" } else { "darwin-x64" };
        vec![("ffmpeg", ff(arch)), ("yt-dlp", yt("yt-dlp_macos"))]
    } else {
        let arch = if cfg!(target_arch = "aarch64") { "linux-arm64" } else { "linux-x64" };
        vec![("ffmpeg", ff(arch)), ("yt-dlp", yt("yt-dlp_linux"))]
    }
}

/// Descarga las herramientas que falten. Se llama desde un hilo aparte.
pub fn install(app: &AppHandle) -> Result<(), String> {
    let dir = bin_dir(app);
    std::fs::create_dir_all(&dir).map_err(|e| format!("no se pudo crear {}: {e}", dir.display()))?;
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
            let frac = if total > 0 { done as f64 / total as f64 } else { 0.0 };
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
