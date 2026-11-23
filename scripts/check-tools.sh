#!/usr/bin/env bash
# Comprueba que las descargas que hace la app siguen vivas y sirven.
#
# La app no empaqueta ffmpeg ni yt-dlp: se los baja al primer arranque de los
# repos oficiales. Si una de esas URLs se cae o la build de ffmpeg pierde un
# códec, la app deja de funcionar para todo el que la instale desde cero y no
# nos enteraríamos. Por eso esto corre antes de publicar una versión.
#
#   ./scripts/check-tools.sh windows|macos|linux
set -euo pipefail
cd "$(dirname "$0")/.."

# La etiqueta sale del código, para que no se desincronicen.
FFMPEG_TAG=$(sed -n 's/.*FFMPEG_TAG: &str = "\([^"]*\)".*/\1/p' src-tauri/src/tools.rs)
[ -n "$FFMPEG_TAG" ] || { echo "no se pudo leer FFMPEG_TAG de src-tauri/src/tools.rs" >&2; exit 1; }
echo "ffmpeg fijado en el código: $FFMPEG_TAG"

FF="https://github.com/eugeneware/ffmpeg-static/releases/download/$FFMPEG_TAG"
YT="https://github.com/yt-dlp/yt-dlp/releases/latest/download"
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT
get() { curl -fsSL --retry 3 -o "$1" "$2"; }

case "${1:-}" in
  windows) get "$OUT/ffmpeg.exe" "$FF/ffmpeg-win32-x64"; get "$OUT/yt-dlp.exe" "$YT/yt-dlp.exe" ;;
  macos)
    arch=$([ "$(uname -m)" = "arm64" ] && echo darwin-arm64 || echo darwin-x64)
    get "$OUT/ffmpeg" "$FF/ffmpeg-$arch"; get "$OUT/yt-dlp" "$YT/yt-dlp_macos" ;;
  linux)   get "$OUT/ffmpeg" "$FF/ffmpeg-linux-x64"; get "$OUT/yt-dlp" "$YT/yt-dlp_linux" ;;
  *) echo "uso: $0 windows|macos|linux" >&2; exit 2 ;;
esac
chmod +x "$OUT"/*

FFBIN=$(ls "$OUT"/ffmpeg*)
YTBIN=$(ls "$OUT"/yt-dlp*)

# Sin estos códecs la app no puede producir alguno de sus formatos de salida.
encoders=$("$FFBIN" -hide_banner -encoders)
for c in libmp3lame aac libopus libvorbis flac pcm_s16le; do
  echo "$encoders" | grep -qw "$c" || { echo "FALTA el códec $c en esta build de ffmpeg" >&2; exit 1; }
done
echo "ffmpeg $("$FFBIN" -version | head -1 | cut -d' ' -f3): los 6 códecs presentes"
echo "yt-dlp $("$YTBIN" --version)"
echo "Las descargas de la app funcionan."
