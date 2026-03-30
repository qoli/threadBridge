#!/usr/bin/env python3
"""Extract a centered macOS app-icon tile from a light-background reference image.

The output is a prebuilt 1024x1024 tile image intended to be used directly as the
final app-icon source, without any additional rounded-mask pass.
"""

from __future__ import annotations

import argparse
import json
import math
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

try:
    from PIL import Image
except ImportError as exc:  # pragma: no cover - runtime dependency check
    raise SystemExit(
        "error: Pillow is required for extract_macos_icon_tile.py; install it with `python3 -m pip install Pillow`"
    ) from exc


DEFAULT_INPUT = Path("icon/candidates/p2_brand_loop_r1_reference.png")
DEFAULT_OUTPUT = Path("icon/p2-brand-loop-r1-tile-1024.png")
DEFAULT_THRESHOLD = 40.0
DEFAULT_PADDING = 8
OUTPUT_SIZE = 1024


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Extract the centered tile from a macOS app-icon reference screenshot."
    )
    parser.add_argument(
        "input_image",
        nargs="?",
        default=str(DEFAULT_INPUT),
        help=f"Reference image path (default: {DEFAULT_INPUT})",
    )
    parser.add_argument(
        "output_image",
        nargs="?",
        default=str(DEFAULT_OUTPUT),
        help=f"Output tile PNG path (default: {DEFAULT_OUTPUT})",
    )
    parser.add_argument(
        "--threshold",
        type=float,
        default=DEFAULT_THRESHOLD,
        help=f"Background-distance threshold for tile detection (default: {DEFAULT_THRESHOLD})",
    )
    parser.add_argument(
        "--padding",
        type=int,
        default=DEFAULT_PADDING,
        help=f"Extra pixels of padding around the detected tile crop (default: {DEFAULT_PADDING})",
    )
    parser.add_argument(
        "--background-samples",
        type=int,
        default=8,
        help="Reserved for future tuning; current detector samples 8 edge points",
    )
    return parser.parse_args()


def ensure_command(name: str) -> None:
    if shutil.which(name) is None:
        raise SystemExit(f"error: required command not found: {name}")


def average_background(img: Image.Image) -> tuple[float, float, float]:
    width, height = img.size
    coords = [
        (0, 0),
        (width - 1, 0),
        (0, height - 1),
        (width - 1, height - 1),
        (width // 2, 0),
        (width // 2, height - 1),
        (0, height // 2),
        (width - 1, height // 2),
    ]
    samples = [img.getpixel(coord) for coord in coords]
    return tuple(sum(pixel[i] for pixel in samples) / len(samples) for i in range(3))


def color_distance(pixel: tuple[int, int, int], background: tuple[float, float, float]) -> float:
    return math.sqrt(sum((pixel[i] - background[i]) ** 2 for i in range(3)))


def detect_tile_bbox(img: Image.Image, threshold: float) -> tuple[int, int, int, int]:
    width, height = img.size
    background = average_background(img)
    min_x, min_y = width, height
    max_x, max_y = -1, -1

    for y in range(height):
        for x in range(width):
            if color_distance(img.getpixel((x, y)), background) > threshold:
                if x < min_x:
                    min_x = x
                if y < min_y:
                    min_y = y
                if x > max_x:
                    max_x = x
                if y > max_y:
                    max_y = y

    if max_x < min_x or max_y < min_y:
        raise SystemExit("error: failed to detect tile bounds; try lowering --threshold")

    return min_x, min_y, max_x, max_y


def square_crop_box(
    bbox: tuple[int, int, int, int], width: int, height: int, padding: int
) -> tuple[int, int, int, int]:
    min_x, min_y, max_x, max_y = bbox
    crop_width = max_x - min_x + 1
    crop_height = max_y - min_y + 1
    side = max(crop_width, crop_height) + padding * 2

    center_x = (min_x + max_x) / 2
    center_y = (min_y + max_y) / 2

    left = round(center_x - side / 2)
    top = round(center_y - side / 2)
    right = left + side
    bottom = top + side

    if left < 0:
        right -= left
        left = 0
    if top < 0:
        bottom -= top
        top = 0
    if right > width:
        shift = right - width
        left -= shift
        right = width
    if bottom > height:
        shift = bottom - height
        top -= shift
        bottom = height

    if left < 0 or top < 0:
        raise SystemExit("error: failed to construct a valid square crop box")

    return left, top, right, bottom


def run_ffmpeg_scale(input_path: Path, output_path: Path) -> None:
    cmd = [
        "ffmpeg",
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-i",
        str(input_path),
        "-vf",
        f"scale={OUTPUT_SIZE}:{OUTPUT_SIZE}:flags=lanczos",
        str(output_path),
    ]
    subprocess.run(cmd, check=True)


def main() -> None:
    args = parse_args()
    ensure_command("ffmpeg")

    input_path = Path(args.input_image)
    output_path = Path(args.output_image)
    metadata_path = output_path.with_suffix(output_path.suffix + ".meta.json")

    if not input_path.is_file():
        raise SystemExit(f"error: input image not found: {input_path}")

    img = Image.open(input_path).convert("RGB")
    bbox = detect_tile_bbox(img, args.threshold)
    crop_box = square_crop_box(bbox, *img.size, padding=args.padding)
    cropped = img.crop(crop_box)

    output_path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp_dir:
        crop_path = Path(tmp_dir) / "tile-crop.png"
        cropped.save(crop_path)
        run_ffmpeg_scale(crop_path, output_path)

    metadata = {
        "input_image": input_path.as_posix(),
        "output_image": output_path.as_posix(),
        "output_size": [OUTPUT_SIZE, OUTPUT_SIZE],
        "threshold": args.threshold,
        "padding": args.padding,
        "detected_bbox": list(bbox),
        "square_crop_box": list(crop_box),
    }
    metadata_path.write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(metadata))


if __name__ == "__main__":
    main()
