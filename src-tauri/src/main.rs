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
struct ToolStatus {
    ffmpeg: bool,
    ytdlp: bool,
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
    let Ok(rd) = std::fs::read_dir(dir) else { return };
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
    p.file_name().unwrap_or_default().to_string_lossy().into_owned()
}

/// Ruta del audio resultante. Si coincide con el origen (convertir un mp3 a mp3
/// en su propia carpeta) se le añade un sufijo para no machacarlo.
fn out_path(src: &Path, fmt: &str, outdir: Option<&Path>) -> PathBuf {
    let stem = src.file_stem().unwrap_or_default().to_string_lossy().into_owned();
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
fn hide_console(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
}

#[cfg(not(windows))]
fn hide_console(_cmd: &mut Command) {}

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
    for line in rx {
        match pct(&line) {
            Some(p) => progress(
                app,
                ((index as f64 + p / 100.0) / total as f64).min(1.0),
                format!("{}/{} · {}", index + 1, total, clip(&line, 90)),
            ),
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

fn run_jobs(app: AppHandle, jobs: Vec<(String, Vec<String>)>) {
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
        let msg = if cancelled {
            "Cancelado".to_string()
        } else if fail > 0 {
            format!("{ok} completado(s), {fail} con error")
        } else {
            format!("{ok} completado(s)")
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
            dir: p.parent().unwrap_or(Path::new("")).to_string_lossy().into_owned(),
            path: p.to_string_lossy().into_owned(),
        })
        .collect()
}

#[tauri::command]
fn tool_status(app: AppHandle) -> ToolStatus {
    ToolStatus {
        ffmpeg: tools::find(&app, "ffmpeg").is_some(),
        ytdlp: tools::find(&app, "yt-dlp").is_some(),
        dir: tools::bin_dir(&app).to_string_lossy().into_owned(),
    }
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
        let mut argv: Vec<String> = vec![
            ffmpeg.clone(),
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
        if lossy {
            argv.push("-b:a".into());
            argv.push(bitrate.clone());
        }
        argv.push(dst.to_string_lossy().into_owned());
        jobs.push((format!("{} → {}", file_name(&src), file_name(&dst)), argv));
    }
    run_jobs(app, jobs);
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

    let mut argv: Vec<String> = vec![
        ytdlp,
        "--newline".into(),
        "--ignore-errors".into(),
        "--embed-metadata".into(),
        "--ffmpeg-location".into(),
        ffmpeg,
        "-P".into(),
        dir.to_string_lossy().into_owned(),
        // los vídeos de una playlist van a su propia subcarpeta
        "-o".into(),
        "%(playlist_title&{}/|)s%(title)s [%(id)s].%(ext)s".into(),
    ];
    if audio {
        codec_of(&format).ok_or("Formato desconocido")?;
        argv.extend([
            "-x".into(),
            "--audio-format".into(),
            format,
            "--audio-quality".into(),
            bitrate,
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
    argv.push(url.clone());
    run_jobs(app, vec![(format!("Descargando {}", clip(&url, 70)), argv)]);
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
    fn reconoce_videos_sin_importar_mayusculas() {
        assert!(is_video(Path::new("a.MP4")));
        assert!(is_video(Path::new("a.mkv")));
        assert!(!is_video(Path::new("a.txt")));
        assert!(!is_video(Path::new("sin_extension")));
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
        let found = expand(&[p.clone(), p, dir.join("a.mp4").to_string_lossy().into_owned()]);
        assert_eq!(found.len(), 2, "recursivo, sin el .txt y sin duplicados");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
