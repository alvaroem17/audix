// Sin consola detrás de la ventana en Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod tools;

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

/// Extensiones que se consideran vídeo al recorrer una carpeta.
const VIDEO_EXT: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "webm", "flv", "wmv", "m4v", "mpg", "mpeg", "ts", "m2ts", "mts",
    "3gp", "ogv", "vob", "divx", "rmvb",
];

/// Plantilla de nombres de yt-dlp. Los vídeos de una playlist van a una subcarpeta
/// con el nombre de ésta y los sueltos a la raíz del destino.
///
/// El `|.` del final es imprescindible: con el valor por defecto vacío yt-dlp
/// construía una ruta absoluta («/titulo.mp4») y se saltaba la carpeta elegida.
/// Tampoco sirve `%(playlist_title&{}/|)s`, porque sanea la barra a «⧸» y deja
/// el nombre de la playlist pegado al del vídeo en vez de crear la carpeta.
const OUT_TEMPLATE: &str = "%(playlist_title|.)s/%(title)s [%(id)s].%(ext)s";

/// Formato de salida -> (códec de ffmpeg, si admite bitrate).
fn codec_of(fmt: &str) -> Option<(&'static str, bool)> {
    Some(match fmt {
        "mp3" => ("libmp3lame", true),
        "m4a" => ("aac", true),
        "opus" => ("libopus", true),
        "ogg" => ("libvorbis", true),
        "flac" => ("flac", false),
        "wav" => ("pcm_s16le", false),
        _ => return None,
    })
}

// ------------------------------------------------------------------ estado

/// Un único trabajo a la vez: el proceso en curso y las banderas para pararlo.
#[derive(Default)]
struct Jobs {
    child: Mutex<Option<Child>>,
    cancel: AtomicBool,
    busy: AtomicBool,
}

#[derive(Serialize, Clone)]
struct Progress {
    value: f64,
    status: String,
}

#[derive(Serialize, Clone)]
pub struct Entry {
    path: String,
    name: String,
    dir: String,
}

#[derive(Serialize, Clone)]
struct Tool {
    found: bool,
    path: String,
    version: String,
}

#[derive(Serialize, Clone)]
struct ToolStatus {
    ffmpeg: Tool,
    ytdlp: Tool,
    dir: String,
}

pub fn log(app: &AppHandle, line: impl Into<String>) {
    let _ = app.emit("log", line.into());
}

pub fn progress(app: &AppHandle, value: f64, status: impl Into<String>) {
    let _ = app.emit(
        "progress",
        Progress {
            value,
            status: status.into(),
        },
    );
}

fn set_busy(app: &AppHandle, busy: bool) {
    app.state::<Jobs>().busy.store(busy, Ordering::SeqCst);
    let _ = app.emit("busy", busy);
}

// ------------------------------------------------------------- utilidades

fn is_video(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| VIDEO_EXT.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(&p, out);
        } else if is_video(&p) {
            out.push(p);
        }
    }
}

/// Expande carpetas a los vídeos que contienen (recursivo) y quita duplicados.
fn expand(paths: &[String]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in paths.iter().map(PathBuf::from) {
        if p.is_dir() {
            walk(&p, &mut out);
        } else if p.is_file() {
            out.push(p);
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    out
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// Ruta del audio resultante. Si coincide con el origen (convertir un mp3 a mp3
/// en su propia carpeta) se le añade un sufijo para no machacarlo.
fn out_path(src: &Path, fmt: &str, outdir: Option<&Path>) -> PathBuf {
    let stem = src
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let dir = outdir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| src.parent().unwrap_or(Path::new(".")).to_path_buf());
    let dst = dir.join(format!("{stem}.{fmt}"));
    if dst == src {
        dir.join(format!("{stem} (audio).{fmt}"))
    } else {
        dst
    }
}

/// Posición dentro de una playlist: «[download] Downloading item 3 of 4» -> (3, 4).
/// Sin esto la barra se reiniciaba de 0 a 100 % con cada vídeo de la lista.
fn playlist_item(line: &str) -> Option<(usize, usize)> {
    let rest = line.strip_prefix("[download] Downloading item ")?;
    let mut campos = rest.split_whitespace();
    let n: usize = campos.next()?.parse().ok()?;
    if campos.next()? != "of" {
        return None;
    }
    let total: usize = campos.next()?.parse().ok()?;
    (total > 0 && n >= 1 && n <= total).then_some((n, total))
}

/// Porcentaje de una línea de progreso de yt-dlp: "[download]  42.3% of 10MiB".
fn pct(line: &str) -> Option<f64> {
    if !line.starts_with("[download]") {
        return None;
    }
    let tok = line.split_whitespace().find(|t| t.ends_with('%'))?;
    tok.trim_end_matches('%').parse().ok()
}

fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// En Windows, los procesos hijos abrirían una consola negra en cada archivo.
#[cfg(windows)]
pub fn hide_console(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
}

#[cfg(not(windows))]
pub fn hide_console(_cmd: &mut Command) {}

// ------------------------------------------------- construcción de comandos

/// Comando de ffmpeg para extraer el audio. `bitrate` sólo para formatos con pérdida.
fn ffmpeg_argv(
    ffmpeg: &str,
    src: &Path,
    dst: &Path,
    codec: &str,
    bitrate: Option<&str>,
) -> Vec<String> {
    let mut argv: Vec<String> = vec![
        ffmpeg.into(),
        "-hide_banner".into(),
        "-nostdin".into(),
        "-nostats".into(),
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        "-i".into(),
        src.to_string_lossy().into_owned(),
        "-vn".into(),
        "-c:a".into(),
        codec.into(),
    ];
    if let Some(b) = bitrate {
        argv.push("-b:a".into());
        argv.push(b.into());
    }
    argv.push(dst.to_string_lossy().into_owned());
    argv
}

/// Comando de yt-dlp. Los vídeos que pertenecen a una playlist van a una
/// subcarpeta con el nombre de ésta; los sueltos, directos al destino.
#[allow(clippy::too_many_arguments)]
fn ytdlp_argv(
    ytdlp: &str,
    ffmpeg: &str,
    url: &str,
    dir: &Path,
    audio: bool,
    format: &str,
    bitrate: &str,
    height: &str,
) -> Vec<String> {
    let mut argv: Vec<String> = vec![
        ytdlp.into(),
        "--newline".into(),
        "--ignore-errors".into(),
        "--embed-metadata".into(),
        "--ffmpeg-location".into(),
        ffmpeg.into(),
        "-P".into(),
        dir.to_string_lossy().into_owned(),
        "-o".into(),
        OUT_TEMPLATE.into(),
    ];
    if audio {
        argv.extend([
            "-x".into(),
            "--audio-format".into(),
            format.into(),
            "--audio-quality".into(),
            bitrate.into(),
        ]);
    } else {
        let sel = if height == "mejor" {
            "bv*+ba/b".to_string()
        } else {
            format!("bv*[height<={height}]+ba/b[height<={height}]")
        };
        argv.extend([
            "-f".into(),
            sel,
            "--merge-output-format".into(),
            "mp4".into(),
        ]);
    }
    argv.push(url.into());
    argv
}

// --------------------------------------------------------- ejecución

fn pump<R: Read + Send + 'static>(src: Option<R>, tx: mpsc::Sender<String>) {
    if let Some(src) = src {
        std::thread::spawn(move || {
            for line in BufReader::new(src).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
    }
}

/// Lanza un comando y va mandando su salida a la interfaz. Devuelve el código de salida.
fn run_one(app: &AppHandle, argv: &[String], index: usize, total: usize) -> Result<i32, String> {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_console(&mut cmd);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("no se pudo ejecutar «{}»: {e}", argv[0]))?;
    let (tx, rx) = mpsc::channel();
    pump(child.stdout.take(), tx.clone());
    pump(child.stderr.take(), tx);

    let state = app.state::<Jobs>();
    *state.child.lock().unwrap() = Some(child);

    // ffmpeg no informa de porcentaje: ahí la barra avanza por archivos.
    let mut item: Option<(usize, usize)> = None;
    for line in rx {
        if let Some(pos) = playlist_item(&line) {
            item = Some(pos);
        }
        match pct(&line) {
            Some(p) => {
                // Dentro de una playlist, el porcentaje es el del vídeo actual.
                let avance = match item {
                    Some((n, m)) => (n as f64 - 1.0 + p / 100.0) / m as f64,
                    None => p / 100.0,
                };
                let dentro = match item {
                    Some((n, m)) => format!("{n} de {m} · "),
                    None => String::new(),
                };
                progress(
                    app,
                    ((index as f64 + avance) / total as f64).min(1.0),
                    format!("{}/{} · {dentro}{}", index + 1, total, clip(&line, 80)),
                );
            }
            None => log(app, line),
        }
    }

    let child = state.child.lock().unwrap().take();
    match child {
        Some(mut c) => c
            .wait()
            .map(|s| s.code().unwrap_or(-1))
            .map_err(|e| e.to_string()),
        None => Ok(-1),
    }
}

/// Qué contar al terminar. Una descarga es un solo trabajo aunque baje una
/// playlist entera, así que decir «1 completado» engañaría.
#[derive(Clone, Copy)]
enum Resumen {
    Archivos,
    Descarga,
}

fn run_jobs(app: AppHandle, jobs: Vec<(String, Vec<String>)>, resumen: Resumen) {
    std::thread::spawn(move || {
        app.state::<Jobs>().cancel.store(false, Ordering::SeqCst);
        set_busy(&app, true);
        let total = jobs.len();
        let (mut ok, mut fail) = (0usize, 0usize);

        for (i, (label, argv)) in jobs.iter().enumerate() {
            if app.state::<Jobs>().cancel.load(Ordering::SeqCst) {
                break;
            }
            log(&app, format!("\n▶ {label}"));
            progress(
                &app,
                i as f64 / total as f64,
                format!("{}/{} · {label}", i + 1, total),
            );
            match run_one(&app, argv, i, total) {
                _ if app.state::<Jobs>().cancel.load(Ordering::SeqCst) => {
                    log(&app, "⏹ Cancelado");
                    break;
                }
                Ok(0) => {
                    ok += 1;
                    log(&app, "✔ Hecho");
                }
                Ok(code) => {
                    fail += 1;
                    log(&app, format!("✖ Error (código {code})"));
                }
                Err(e) => {
                    fail += 1;
                    log(&app, format!("✖ {e}"));
                }
            }
        }

        let cancelled = app.state::<Jobs>().cancel.load(Ordering::SeqCst);
        let msg = match (cancelled, resumen, fail) {
            (true, _, _) => "Cancelado".to_string(),
            (_, Resumen::Archivos, 0) => format!("{ok} archivo(s) convertido(s)"),
            (_, Resumen::Archivos, _) => format!("{ok} convertido(s), {fail} con error"),
            (_, Resumen::Descarga, 0) => "Descarga terminada".to_string(),
            (_, Resumen::Descarga, _) => "La descarga terminó con errores".to_string(),
        };
        progress(&app, if cancelled { 0.0 } else { 1.0 }, msg);
        set_busy(&app, false);
    });
}

// ------------------------------------------------------------- comandos

#[tauri::command]
fn scan(paths: Vec<String>) -> Vec<Entry> {
    expand(&paths)
        .into_iter()
        .map(|p| Entry {
            name: file_name(&p),
            dir: p
                .parent()
                .unwrap_or(Path::new(""))
                .to_string_lossy()
                .into_owned(),
            path: p.to_string_lossy().into_owned(),
        })
        .collect()
}

fn tool_info(app: &AppHandle, name: &str) -> Tool {
    match tools::find(app, name) {
        Some(p) => Tool {
            found: true,
            version: tools::version(&p).unwrap_or_default(),
            path: p.to_string_lossy().into_owned(),
        },
        None => Tool {
            found: false,
            path: String::new(),
            version: String::new(),
        },
    }
}

#[tauri::command]
fn tool_status(app: AppHandle) -> ToolStatus {
    ToolStatus {
        ffmpeg: tool_info(&app, "ffmpeg"),
        ytdlp: tool_info(&app, "yt-dlp"),
        dir: tools::bin_dir(&app).to_string_lossy().into_owned(),
    }
}

/// Fuerza la descarga de la última versión de yt-dlp. Es la respuesta a los
/// errores 403 de YouTube, que casi siempre significan «yt-dlp desactualizado».
#[tauri::command]
fn update_ytdlp(app: AppHandle) -> Result<(), String> {
    if app.state::<Jobs>().busy.load(Ordering::SeqCst) {
        return Err("Ya hay un trabajo en curso".into());
    }
    std::thread::spawn(move || {
        set_busy(&app, true);
        match tools::update_ytdlp(&app) {
            Ok(()) => progress(&app, 1.0, "yt-dlp actualizado"),
            Err(e) => {
                log(&app, format!("✖ {e}"));
                progress(&app, 0.0, "No se pudo actualizar yt-dlp");
            }
        }
        let _ = app.emit("tools", tool_status(app.clone()));
        set_busy(&app, false);
    });
    Ok(())
}

#[tauri::command]
fn install_tools(app: AppHandle) -> Result<(), String> {
    if app.state::<Jobs>().busy.load(Ordering::SeqCst) {
        return Err("Ya hay un trabajo en curso".into());
    }
    std::thread::spawn(move || {
        set_busy(&app, true);
        match tools::install(&app) {
            Ok(()) => progress(&app, 1.0, "Herramientas listas"),
            Err(e) => {
                log(&app, format!("✖ {e}"));
                progress(&app, 0.0, "No se pudieron descargar");
            }
        }
        let _ = app.emit("tools", tool_status(app.clone()));
        set_busy(&app, false);
    });
    Ok(())
}

#[tauri::command]
fn default_out(app: AppHandle) -> String {
    app.path()
        .download_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("Audix")
        .to_string_lossy()
        .into_owned()
}

#[tauri::command]
fn convert(
    app: AppHandle,
    files: Vec<String>,
    format: String,
    bitrate: String,
    outdir: String,
) -> Result<(), String> {
    if app.state::<Jobs>().busy.load(Ordering::SeqCst) {
        return Err("Ya hay un trabajo en curso".into());
    }
    if files.is_empty() {
        return Err("Añade algún vídeo primero".into());
    }
    let (codec, lossy) = codec_of(&format).ok_or("Formato desconocido")?;
    let ffmpeg = tools::find(&app, "ffmpeg")
        .ok_or("Falta ffmpeg. Pulsa «Descargar herramientas».")?
        .to_string_lossy()
        .into_owned();

    let out = if outdir.is_empty() {
        None
    } else {
        let d = PathBuf::from(&outdir);
        std::fs::create_dir_all(&d).map_err(|e| format!("no se pudo crear {outdir}: {e}"))?;
        Some(d)
    };

    let mut jobs = Vec::new();
    for f in &files {
        let src = PathBuf::from(f);
        let dst = out_path(&src, &format, out.as_deref());
        let argv = ffmpeg_argv(
            &ffmpeg,
            &src,
            &dst,
            codec,
            lossy.then_some(bitrate.as_str()),
        );
        jobs.push((format!("{} → {}", file_name(&src), file_name(&dst)), argv));
    }
    run_jobs(app, jobs, Resumen::Archivos);
    Ok(())
}

#[tauri::command]
fn download(
    app: AppHandle,
    url: String,
    audio: bool,
    format: String,
    bitrate: String,
    height: String,
    outdir: String,
) -> Result<(), String> {
    if app.state::<Jobs>().busy.load(Ordering::SeqCst) {
        return Err("Ya hay un trabajo en curso".into());
    }
    let url = url.trim().to_string();
    if !url.starts_with("http") {
        return Err("Pega una URL válida".into());
    }
    let ytdlp = tools::find(&app, "yt-dlp")
        .ok_or("Falta yt-dlp. Pulsa «Descargar herramientas».")?
        .to_string_lossy()
        .into_owned();
    let ffmpeg = tools::find(&app, "ffmpeg")
        .ok_or("Falta ffmpeg. Pulsa «Descargar herramientas».")?
        .to_string_lossy()
        .into_owned();

    let dir = PathBuf::from(&outdir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("no se pudo crear {outdir}: {e}"))?;

    if audio {
        codec_of(&format).ok_or("Formato desconocido")?;
    }
    let argv = ytdlp_argv(
        &ytdlp, &ffmpeg, &url, &dir, audio, &format, &bitrate, &height,
    );
    run_jobs(
        app,
        vec![(format!("Descargando {}", clip(&url, 70)), argv)],
        Resumen::Descarga,
    );
    Ok(())
}

#[tauri::command]
fn cancel(state: State<Jobs>) {
    state.cancel.store(true, Ordering::SeqCst);
    if let Some(c) = state.child.lock().unwrap().as_mut() {
        let _ = c.kill();
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(Jobs::default())
        .invoke_handler(tauri::generate_handler![
            scan,
            tool_status,
            install_tools,
            update_ytdlp,
            default_out,
            convert,
            download,
            cancel
        ])
        .run(tauri::generate_context!())
        .expect("error al arrancar Audix");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatos_conocidos() {
        assert_eq!(codec_of("mp3"), Some(("libmp3lame", true)));
        assert_eq!(codec_of("flac"), Some(("flac", false)));
        assert_eq!(codec_of("xyz"), None);
    }

    #[test]
    fn destino_no_machaca_el_origen() {
        let src = Path::new("/v/a.mkv");
        assert_eq!(out_path(src, "mp3", None), PathBuf::from("/v/a.mp3"));
        assert_eq!(
            out_path(src, "mp3", Some(Path::new("/out"))),
            PathBuf::from("/out/a.mp3")
        );
        assert_eq!(
            out_path(Path::new("/v/a.mp3"), "mp3", None),
            PathBuf::from("/v/a (audio).mp3")
        );
    }

    #[test]
    fn porcentaje_solo_en_lineas_de_descarga() {
        assert_eq!(pct("[download]  42.3% of 10MiB"), Some(42.3));
        assert_eq!(pct("[download] 100% of 1MiB"), Some(100.0));
        assert_eq!(pct("un 50% de cosas"), None);
        assert_eq!(pct("[download] Destination: x.mp4"), None);
    }

    #[test]
    fn posicion_dentro_de_la_playlist() {
        assert_eq!(
            playlist_item("[download] Downloading item 3 of 4"),
            Some((3, 4))
        );
        assert_eq!(
            playlist_item("[download] Downloading item 1 of 1"),
            Some((1, 1))
        );
        assert_eq!(playlist_item("[download]  42.3% of 10MiB"), None);
        assert_eq!(playlist_item("[download] Downloading item 5 of 4"), None);
        assert_eq!(playlist_item("otra cosa"), None);
    }

    #[test]
    fn reconoce_videos_sin_importar_mayusculas() {
        assert!(is_video(Path::new("a.MP4")));
        assert!(is_video(Path::new("a.mkv")));
        assert!(!is_video(Path::new("a.txt")));
        assert!(!is_video(Path::new("sin_extension")));
    }

    #[test]
    fn comando_de_ffmpeg_segun_el_formato() {
        let src = Path::new("/v/a.mkv");
        let dst = Path::new("/v/a.mp3");
        let con = ffmpeg_argv("ffmpeg", src, dst, "libmp3lame", Some("192k"));
        assert_eq!(con.first().unwrap(), "ffmpeg");
        assert_eq!(con.last().unwrap(), "/v/a.mp3");
        assert!(con.windows(2).any(|w| w == ["-b:a", "192k"]));
        assert!(con.windows(2).any(|w| w == ["-c:a", "libmp3lame"]));
        assert!(con.contains(&"-vn".to_string()), "no debe copiar el vídeo");

        let sin = ffmpeg_argv("ffmpeg", src, Path::new("/v/a.flac"), "flac", None);
        assert!(!sin.contains(&"-b:a".to_string()), "flac no lleva bitrate");
    }

    #[test]
    fn comando_de_ytdlp_audio_y_video() {
        let dir = Path::new("/out");
        let a = ytdlp_argv(
            "yt-dlp",
            "ffmpeg",
            "https://x/y",
            dir,
            true,
            "mp3",
            "320k",
            "1080",
        );
        assert!(a.contains(&"-x".to_string()));
        assert!(a.windows(2).any(|w| w == ["--audio-format", "mp3"]));
        assert!(a.windows(2).any(|w| w == ["--audio-quality", "320k"]));
        assert_eq!(a.last().unwrap(), "https://x/y");

        let v = ytdlp_argv(
            "yt-dlp",
            "ffmpeg",
            "https://x/y",
            dir,
            false,
            "mp3",
            "320k",
            "720",
        );
        assert!(!v.contains(&"-x".to_string()));
        let sel = &v[v.iter().position(|x| x == "-f").unwrap() + 1];
        assert!(sel.contains("height<=720"), "limita la resolución: {sel}");

        let best = ytdlp_argv(
            "yt-dlp",
            "ffmpeg",
            "https://x/y",
            dir,
            false,
            "mp3",
            "320k",
            "mejor",
        );
        assert_eq!(
            best[best.iter().position(|x| x == "-f").unwrap() + 1],
            "bv*+ba/b"
        );
    }

    /// Prueba de verdad: genera un vídeo con ffmpeg y lo convierte a mp3 con el
    /// comando que arma la app. Se salta si el sistema no tiene ffmpeg.
    #[test]
    fn conversion_real_con_ffmpeg() {
        let Some(ffmpeg) = which("ffmpeg") else {
            eprintln!("sin ffmpeg en el PATH, test omitido");
            return;
        };
        let dir = std::env::temp_dir().join("audix-test-convert");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("muestra.mp4");
        let ok = Command::new(&ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=d=1:s=160x120",
                "-f",
                "lavfi",
                "-i",
                "sine=d=1",
            ])
            .arg(&src)
            .status()
            .unwrap();
        assert!(ok.success(), "no se pudo crear el vídeo de prueba");

        let dst = out_path(&src, "mp3", None);
        let argv = ffmpeg_argv(&ffmpeg, &src, &dst, "libmp3lame", Some("192k"));
        let status = Command::new(&argv[0]).args(&argv[1..]).status().unwrap();
        assert!(status.success(), "ffmpeg falló: {argv:?}");
        assert!(dst.is_file(), "no se creó {}", dst.display());
        assert!(std::fs::metadata(&dst).unwrap().len() > 1000, "mp3 vacío");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Comprueba contra el yt-dlp real (sin tocar la red, con un info.json falso)
    /// que la plantilla mete las playlists en su carpeta y deja los vídeos sueltos
    /// dentro del destino en lugar de en la raíz del disco.
    #[test]
    fn plantilla_de_ytdlp_respeta_el_destino() {
        let Some(ytdlp) = which("yt-dlp") else {
            eprintln!("sin yt-dlp en el PATH, test omitido");
            return;
        };
        let dir = std::env::temp_dir().join("audix-test-tmpl");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let base = r#""id":"abc","title":"Tema","ext":"mp4","_type":"video","extractor":"t","extractor_key":"T","webpage_url":"https://x/1","url":"https://x/1.mp4""#;

        let filename = |json: &str, name: &str| {
            let f = dir.join(name);
            std::fs::write(&f, json).unwrap();
            let out = Command::new(&ytdlp)
                .args(["--quiet", "--skip-download", "--load-info-json"])
                .arg(&f)
                .arg("-P")
                .arg(&dir)
                .args(["-o", OUT_TEMPLATE, "--print", "filename"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };

        let suelto = filename(&format!("{{{base}}}"), "solo.json");
        let lista = filename(
            &format!(r#"{{{base},"playlist_title":"Mi Lista"}}"#),
            "lista.json",
        );

        assert!(
            Path::new(&suelto).starts_with(&dir),
            "un vídeo suelto debe quedarse en el destino, no en {suelto}"
        );
        assert_eq!(
            Path::new(&lista).parent().unwrap().file_name().unwrap(),
            "Mi Lista",
            "una playlist debe ir a su propia carpeta: {lista}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// El caso de uso principal de la pestaña Convertir: soltar una carpeta con
    /// subcarpetas y que salgan todos los audios. Usa ffmpeg de verdad.
    #[test]
    fn convierte_una_carpeta_entera_con_subcarpetas() {
        let Some(ffmpeg) = which("ffmpeg") else {
            eprintln!("sin ffmpeg en el PATH, test omitido");
            return;
        };
        let raiz = std::env::temp_dir().join("audix-test-carpeta");
        let _ = std::fs::remove_dir_all(&raiz);
        std::fs::create_dir_all(raiz.join("boda/discursos")).unwrap();

        let crear = |ruta: &Path| {
            let ok = Command::new(&ffmpeg)
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc=d=1:s=128x96",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=d=1",
                ])
                .arg(ruta)
                .status()
                .unwrap();
            assert!(ok.success(), "no se pudo crear {}", ruta.display());
        };
        crear(&raiz.join("boda/ceremonia.mp4"));
        crear(&raiz.join("boda/discursos/padrino.mkv"));
        crear(&raiz.join("boda/discursos/madrina.webm"));
        std::fs::write(raiz.join("boda/notas.txt"), "no es un vídeo").unwrap();

        let encontrados = expand(&[raiz.join("boda").to_string_lossy().into_owned()]);
        assert_eq!(
            encontrados.len(),
            3,
            "debe bajar a las subcarpetas y saltarse el .txt"
        );

        let destino = raiz.join("salida");
        std::fs::create_dir_all(&destino).unwrap();
        for src in &encontrados {
            let dst = out_path(src, "mp3", Some(&destino));
            let argv = ffmpeg_argv(&ffmpeg, src, &dst, "libmp3lame", Some("128k"));
            let st = Command::new(&argv[0]).args(&argv[1..]).status().unwrap();
            assert!(st.success(), "falló al convertir {}", src.display());
            assert!(dst.is_file() && dst.metadata().unwrap().len() > 1000);
        }
        let salidas = std::fs::read_dir(&destino).unwrap().count();
        assert_eq!(salidas, 3, "un mp3 por vídeo, sin pisarse entre carpetas");
        let _ = std::fs::remove_dir_all(&raiz);
    }

    fn which(bin: &str) -> Option<String> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|d| d.join(bin))
            .find(|p| p.is_file())
            .map(|p| p.to_string_lossy().into_owned())
    }

    #[test]
    fn expand_quita_duplicados_y_lo_que_no_existe() {
        assert!(expand(&["/no/existe".into()]).is_empty());
        let dir = std::env::temp_dir().join("audix-test-expand");
        let _ = std::fs::create_dir_all(dir.join("sub"));
        std::fs::write(dir.join("a.mp4"), b"x").unwrap();
        std::fs::write(dir.join("sub/b.mkv"), b"x").unwrap();
        std::fs::write(dir.join("c.txt"), b"x").unwrap();
        let p = dir.to_string_lossy().into_owned();
        let found = expand(&[
            p.clone(),
            p,
            dir.join("a.mp4").to_string_lossy().into_owned(),
        ]);
        assert_eq!(found.len(), 2, "recursivo, sin el .txt y sin duplicados");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
