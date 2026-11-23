# Audix

App de escritorio para **convertir vídeos a audio** y **descargar vídeos o playlists de
YouTube**. Para Windows y macOS, sin instalación: descomprimes y haces doble clic.

- **Convertir** — arrastra archivos o carpetas enteras (busca vídeos también en las
  subcarpetas) y saca el audio en mp3, m4a, opus, ogg, flac o wav. El resultado va junto
  al vídeo original o a la carpeta que elijas.
- **Descargar** — pega un enlace de un vídeo, una playlist o un canal. Solo el audio, o el
  vídeo con la resolución que quieras. Las playlists se guardan en su propia carpeta.
  Además de YouTube funcionan los cientos de webs que soporta yt-dlp.

## Descargas

En la pestaña [Releases](../../releases) hay cuatro archivos por versión:

| Archivo | Qué es |
|---|---|
| `…-windows-x64-portable.zip` | Descomprimir y abrir `Audix.exe`. Trae ffmpeg y yt-dlp dentro. |
| `…-windows-x64-instalador.exe` | Instalador normal. Al primer arranque ofrece descargar ffmpeg y yt-dlp. |
| `…-macos-universal-portable.zip` | Descomprimir y abrir `Audix.app`. Trae ffmpeg y yt-dlp dentro. Intel y Apple Silicon. |
| `…-macos-universal-instalador.dmg` | DMG normal. Al primer arranque ofrece descargar ffmpeg y yt-dlp. |

La app no está firmada, así que la primera vez el sistema avisa:

- **Windows**: SmartScreen muestra "Windows protegió su PC" → *Más información* →
  *Ejecutar de todas formas*.
- **macOS**: clic derecho sobre `Audix.app` → *Abrir* → *Abrir*. Después se abre con doble
  clic como cualquier otra.

Firmar la app quitaría estos avisos, pero cuesta 99 €/año de cuenta de Apple Developer y
un certificado de firma para Windows.

## Cómo funciona por dentro

El trabajo pesado lo hacen dos programas probados, que Audix llama por debajo:

- [**ffmpeg**](https://ffmpeg.org) extrae y recodifica el audio.
- [**yt-dlp**](https://github.com/yt-dlp/yt-dlp) descarga de YouTube y compañía.

La versión portable los lleva en `bin/`, al lado del ejecutable. La instalable sale sin
ellos y los descarga de sus repos oficiales a la carpeta de datos del usuario cuando
pulsas *Descargar herramientas*. Si ya los tienes en el PATH, usa esos.

Mantener yt-dlp al día importa: YouTube cambia a menudo y las versiones viejas dejan de
funcionar. Basta con borrar `yt-dlp` de la carpeta de herramientas y volver a pulsar
*Descargar herramientas*, o actualizar el que tengas en el sistema.

## Desarrollo

Hecho con [Tauri 2](https://tauri.app): backend en Rust, interfaz en HTML, CSS y
JavaScript sin framework ni paso de compilación. En Windows y macOS usa el motor web que
el sistema ya trae (WebView2 y WKWebView), por eso el ejecutable ronda los 7 MB y no hay
nada que instalar.

```
src/                interfaz (index.html, styles.css, app.js)
src-tauri/src/      main.rs (comandos y ejecución) y tools.rs (ffmpeg y yt-dlp)
scripts/            descarga de herramientas y versionado
.github/workflows/  tests y compilación de las releases
```

Necesitas [Rust](https://rustup.rs) y el CLI de Tauri (`npm i -g @tauri-apps/cli@^2` o
`cargo install tauri-cli --version "^2"`). En Linux, además, `webkit2gtk` y `gtk3` para
poder probar en local.

```bash
cd src-tauri && cargo test          # incluye una conversión real con ffmpeg
cargo tauri dev                     # ventana en modo desarrollo
cargo tauri build                   # compilar para el sistema actual
```

Para ver la interfaz en un navegador sin compilar nada, sirve `src/` por HTTP con un
`window.__TAURI__` simulado.

### Publicar una versión

```bash
git tag v0.1.0 && git push origin v0.1.0
```

GitHub Actions compila en runners de Windows y macOS, empaqueta los cuatro archivos y crea
la release. La versión de la etiqueta se escribe sola en `tauri.conf.json` y `Cargo.toml`.

## Lo que no hace

- **Spotify** no entra. Su DRM impide descargar sus audios y lo que hacen otras
  herramientas es leer la playlist y bajar cada canción de YouTube; se puede añadir, pero
  de momento está fuera.
- **Linux** no está entre los objetivos. El código compila y funciona, pero no se publican
  binarios porque Tauri necesita `webkit2gtk` instalado en el sistema.
- Un solo trabajo a la vez: la cola de conversión va archivo por archivo.

## Licencia

MIT.
