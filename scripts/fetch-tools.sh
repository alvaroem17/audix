#!/usr/bin/env bash
# Descarga ffmpeg y yt-dlp estáticos oficiales a src-tauri/bin/.
# Ese directorio es lo que viaja dentro de la versión portable; la instalable
# sale sin él y los descarga ella misma al primer arranque.
#
#   ./scripts/fetch-tools.sh windows|macos|linux
set -euo pipefail

# Fijada a mano: al subirla hay que comprobar que sigue trayendo los códecs de abajo.
FFMPEG_TAG="b6.1.1"
FF="https://github.com/eugeneware/ffmpeg-static/releases/download/$FFMPEG_TAG"
YT="https://github.com/yt-dlp/yt-dlp/releases/latest/download"

cd "$(dirname "$0")/.."
OUT="src-tauri/bin"
mkdir -p "$OUT"
get() { curl -fsSL --retry 3 -o "$1" "$2"; }

case "${1:-}" in
  windows)
    get "$OUT/ffmpeg.exe" "$FF/ffmpeg-win32-x64"
    get "$OUT/yt-dlp.exe" "$YT/yt-dlp.exe"
    FFBIN="$OUT/ffmpeg.exe"
    ;;
  macos)
    # Un único binario universal para Intel y Apple Silicon.
    get /tmp/ffmpeg-arm64 "$FF/ffmpeg-darwin-arm64"
    get /tmp/ffmpeg-x64 "$FF/ffmpeg-darwin-x64"
    lipo -create /tmp/ffmpeg-arm64 /tmp/ffmpeg-x64 -output "$OUT/ffmpeg"
    get "$OUT/yt-dlp" "$YT/yt-dlp_macos"
    FFBIN="$OUT/ffmpeg"
    ;;
  linux)
    get "$OUT/ffmpeg" "$FF/ffmpeg-linux-x64"
    get "$OUT/yt-dlp" "$YT/yt-dlp_linux"
    FFBIN="$OUT/ffmpeg"
    ;;
  *)
    echo "uso: $0 windows|macos|linux" >&2
    exit 2
    ;;
esac

chmod +x "$OUT"/*

# Si la build de ffmpeg no trae estos códecs, la app falla al convertir.
echo "Comprobando códecs de $FFBIN"
encoders=$("$FFBIN" -hide_banner -encoders)
for c in libmp3lame aac libopus libvorbis flac pcm_s16le; do
  echo "$encoders" | grep -qw "$c" || { echo "FALTA el códec $c en esta build de ffmpeg" >&2; exit 1; }
done
echo "Herramientas listas en $OUT:"
ls -lh "$OUT"
