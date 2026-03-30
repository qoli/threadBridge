#!/usr/bin/env python3
"""Convert a flat keyed background color into alpha."""

from __future__ import annotations

import argparse
from pathlib import Path

from PIL import Image


def parse_rgb(value: str) -> tuple[int, int, int]:
    parts = [part.strip() for part in value.split(",")]
    if len(parts) != 3:
        raise argparse.ArgumentTypeError("key color must be R,G,B")
    try:
        rgb = tuple(int(part) for part in parts)
    except ValueError as exc:
        raise argparse.ArgumentTypeError("key color must be R,G,B") from exc
    if any(channel < 0 or channel > 255 for channel in rgb):
        raise argparse.ArgumentTypeError("RGB channels must be between 0 and 255")
    return rgb


def alpha_for_distance(distance: float, threshold: float, feather: float) -> int:
    if distance <= threshold:
        return 0
    if feather <= 0 or distance >= threshold + feather:
        return 255
    normalized = (distance - threshold) / feather
    return round(255 * normalized)


def main() -> None:
    parser = argparse.ArgumentParser(description="Turn a keyed background color into transparency.")
    parser.add_argument("input_image", help="Input PNG path")
    parser.add_argument("output_image", help="Output PNG path")
    parser.add_argument(
        "--key-color",
        type=parse_rgb,
        required=True,
        help="Background color to key out, as R,G,B",
    )
    parser.add_argument(
        "--threshold",
        type=float,
        default=55.0,
        help="Distance fully removed as background (default: 55)",
    )
    parser.add_argument(
        "--feather",
        type=float,
        default=45.0,
        help="Soft transition after threshold (default: 45)",
    )
    args = parser.parse_args()

    input_path = Path(args.input_image)
    output_path = Path(args.output_image)
    output_path.parent.mkdir(parents=True, exist_ok=True)

    image = Image.open(input_path).convert("RGBA")
    pixels = image.load()
    key_r, key_g, key_b = args.key_color

    for y in range(image.height):
        for x in range(image.width):
            r, g, b, a = pixels[x, y]
            distance = ((r - key_r) ** 2 + (g - key_g) ** 2 + (b - key_b) ** 2) ** 0.5
            new_alpha = alpha_for_distance(distance, args.threshold, args.feather)
            pixels[x, y] = (r, g, b, min(a, new_alpha))

    image.save(output_path)
    print(
        f"wrote {output_path} with key_color={args.key_color} "
        f"threshold={args.threshold} feather={args.feather}"
    )


if __name__ == "__main__":
    main()
