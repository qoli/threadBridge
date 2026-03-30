#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEFAULT_SYMBOL_SOURCE="$ROOT_DIR/icon/round-04-no-tile-dark-bg-v1.png"
DEFAULT_FINAL_TILE_SOURCE="$ROOT_DIR/icon/p2-brand-loop-r1-tile-1024.png"
APP_ICON_DIR="$ROOT_DIR/rust/static/app_icon"
ICONSET_DIR="$APP_ICON_DIR/threadBridge.iconset"
ICNS_PATH="$APP_ICON_DIR/threadBridge.icns"
MASK_SCRIPT="$ROOT_DIR/scripts/apply_macos_icon_mask.swift"
ZOOM_SCALE_PERCENT=150
SOURCE_MODE="symbol"
SOURCE_IMAGE=""

usage() {
  cat <<EOF
usage: $(basename "$0") [--source-mode symbol|final-tile] [source-image]

Build the macOS app iconset and .icns.

Modes:
  symbol      Source is a symbol-only image; apply the existing zoom + rounded-mask pipeline.
  final-tile  Source is already a final 1024x1024 tile image; skip zoom and rounded-mask.

Defaults:
  symbol      $DEFAULT_SYMBOL_SOURCE
  final-tile  $DEFAULT_FINAL_TILE_SOURCE
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --source-mode)
      if [[ $# -lt 2 ]]; then
        echo "error: --source-mode requires a value" >&2
        usage >&2
        exit 1
      fi
      SOURCE_MODE="$2"
      shift 2
      ;;
    --source-mode=*)
      SOURCE_MODE="${1#*=}"
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    -*)
      echo "error: unknown option: $1" >&2
      usage >&2
      exit 1
      ;;
    *)
      if [[ -n "$SOURCE_IMAGE" ]]; then
        echo "error: multiple source images provided" >&2
        usage >&2
        exit 1
      fi
      SOURCE_IMAGE="$1"
      shift
      ;;
  esac
done

case "$SOURCE_MODE" in
  symbol)
    SOURCE_IMAGE="${SOURCE_IMAGE:-$DEFAULT_SYMBOL_SOURCE}"
    ;;
  final-tile)
    SOURCE_IMAGE="${SOURCE_IMAGE:-$DEFAULT_FINAL_TILE_SOURCE}"
    ;;
  *)
    echo "error: unsupported --source-mode: $SOURCE_MODE" >&2
    usage >&2
    exit 1
    ;;
esac

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: build_macos_app_icon.sh requires macOS" >&2
  exit 1
fi

for cmd in iconutil sips; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "error: required command not found: $cmd" >&2
    exit 1
  fi
done

if [[ ! -f "$SOURCE_IMAGE" ]]; then
  echo "error: source image not found: $SOURCE_IMAGE" >&2
  exit 1
fi

if [[ "$SOURCE_MODE" == "symbol" && ! -f "$MASK_SCRIPT" ]]; then
  echo "error: mask script not found: $MASK_SCRIPT" >&2
  exit 1
fi

tmp_dir="$(mktemp -d)"
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

read -r width height < <(
  sips -g pixelWidth -g pixelHeight "$SOURCE_IMAGE" 2>/dev/null \
    | awk '/pixelWidth/ { w=$2 } /pixelHeight/ { h=$2 } END { print w, h }'
)

if [[ -z "${width:-}" || -z "${height:-}" ]]; then
  echo "error: failed to read source image dimensions: $SOURCE_IMAGE" >&2
  exit 1
fi

normalized_png="$tmp_dir/threadBridge-1024.png"
zoomed_png="$tmp_dir/threadBridge-zoomed.png"
framed_png="$tmp_dir/threadBridge-framed.png"
masked_png="$tmp_dir/threadBridge-masked.png"
final_png="$masked_png"

if [[ "$width" -eq 1024 && "$height" -eq 1024 ]]; then
  cp "$SOURCE_IMAGE" "$normalized_png"
elif [[ "$width" -eq "$height" ]]; then
  echo "normalizing square source from ${width}x${height} to 1024x1024"
  sips -z 1024 1024 "$SOURCE_IMAGE" --out "$normalized_png" >/dev/null
else
  side="$width"
  if (( height < width )); then
    side="$height"
  fi
  cropped_png="$tmp_dir/threadBridge-cropped.png"
  echo "cropping non-square source from ${width}x${height} to ${side}x${side}, then scaling to 1024x1024"
  sips -c "$side" "$side" "$SOURCE_IMAGE" --out "$cropped_png" >/dev/null
  sips -z 1024 1024 "$cropped_png" --out "$normalized_png" >/dev/null
fi

if [[ "$SOURCE_MODE" == "symbol" ]]; then
  zoom_size=$((1024 * ZOOM_SCALE_PERCENT / 100))
  sips -z "$zoom_size" "$zoom_size" "$normalized_png" --out "$zoomed_png" >/dev/null
  sips -c 1024 1024 "$zoomed_png" --out "$framed_png" >/dev/null
  swift "$MASK_SCRIPT" "$framed_png" "$masked_png"
else
  final_png="$normalized_png"
fi

mkdir -p "$APP_ICON_DIR"
rm -rf "$ICONSET_DIR"
mkdir -p "$ICONSET_DIR"

render_icon() {
  local size="$1"
  local filename="$2"
  sips -z "$size" "$size" "$final_png" --out "$ICONSET_DIR/$filename" >/dev/null
}

render_icon 16 "icon_16x16.png"
render_icon 32 "icon_16x16@2x.png"
render_icon 32 "icon_32x32.png"
render_icon 64 "icon_32x32@2x.png"
render_icon 128 "icon_128x128.png"
render_icon 256 "icon_128x128@2x.png"
render_icon 256 "icon_256x256.png"
render_icon 512 "icon_256x256@2x.png"
render_icon 512 "icon_512x512.png"
render_icon 1024 "icon_512x512@2x.png"

iconutil -c icns "$ICONSET_DIR" -o "$ICNS_PATH"

echo "source mode: $SOURCE_MODE"
echo "source image: $SOURCE_IMAGE"
echo "generated iconset: $ICONSET_DIR"
echo "generated icns: $ICNS_PATH"
