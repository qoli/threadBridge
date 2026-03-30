#!/usr/bin/env python3
import argparse
import csv
import json
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

try:
    from PIL import Image, ImageOps, ImageDraw
except ImportError as exc:  # pragma: no cover
    raise SystemExit(f"Pillow is required: {exc}")


DEFAULT_CSV_PATH = "icon/exploration/qwen-image-2512/fluent-office/prompts.csv"
DEFAULT_OUTPUT_DIR = "tmp/qwen-image-2512-icon-exploration"
DEFAULT_QWEN_CLI = "/Volumes/Data/Github/qwen.image.swift/.build/xcode/Build/Products/Release/QwenImageCLI"
DEFAULT_MODEL_PATH = "/Volumes/Data/qwen-image/Qwen-Image-2512"
DEFAULT_LIGHTNING_LORA = (
    "/Volumes/Data/qwen-image/Qwen-Image-2512-Lightning/"
    "Qwen-Image-2512-Lightning-4steps-V1.0-bf16.safetensors"
)
FIELDNAMES = [
    "prompt_id",
    "series",
    "round",
    "label",
    "status",
    "profile",
    "steps",
    "guidance",
    "true_cfg_scale",
    "width",
    "height",
    "negative_prompt",
    "prompt_text",
    "notes",
]


def fail(message: str) -> None:
    raise SystemExit(message)


def utc_stamp() -> str:
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run Qwen-Image-2512 icon exploration prompts from a CSV registry."
    )
    parser.add_argument("--csv-path", default=DEFAULT_CSV_PATH)
    parser.add_argument("--prompt-id")
    parser.add_argument("--series")
    parser.add_argument("--list-prompts", action="store_true")
    parser.add_argument("--output-dir", default=DEFAULT_OUTPUT_DIR)
    parser.add_argument("--qwen-cli", default=DEFAULT_QWEN_CLI)
    parser.add_argument("--model-path", default=DEFAULT_MODEL_PATH)
    parser.add_argument("--lightning-lora", default=DEFAULT_LIGHTNING_LORA)
    parser.add_argument(
        "--seeds",
        default="41,42,43",
        help="Comma-separated seeds to run for each selected prompt.",
    )
    parser.add_argument(
        "--gpu-cache-limit",
        default="16gb",
        help="Forwarded to QwenImageCLI to reduce peak cache retention.",
    )
    parser.add_argument(
        "--clear-cache-between-stages",
        action="store_true",
        default=True,
        help="Forwarded to QwenImageCLI.",
    )
    return parser.parse_args()


def load_rows(csv_path: Path) -> list[dict[str, str]]:
    if not csv_path.exists():
        fail(f"prompt CSV does not exist: {csv_path}")
    with csv_path.open("r", encoding="utf-8", newline="") as handle:
        reader = csv.DictReader(handle)
        if reader.fieldnames != FIELDNAMES:
            fail(
                "prompt CSV has unexpected columns: "
                f"expected {FIELDNAMES}, got {reader.fieldnames}"
            )
        rows: list[dict[str, str]] = []
        for raw in reader:
            row = {field: (raw.get(field) or "").strip() for field in FIELDNAMES}
            if not row["prompt_id"]:
                fail("prompt CSV contains a row with an empty prompt_id")
            rows.append(row)
    if not rows:
        fail(f"prompt CSV is empty: {csv_path}")
    return rows


def list_prompts(rows: list[dict[str, str]]) -> None:
    print("prompt_id\tseries\tround\tstatus\tprofile\tlabel")
    for row in rows:
        print(
            "\t".join(
                [
                    row["prompt_id"],
                    row["series"],
                    row["round"] or "-",
                    row["status"],
                    row["profile"],
                    row["label"],
                ]
            )
        )


def selected_rows(
    rows: list[dict[str, str]], prompt_id: str | None, series: str | None
) -> list[dict[str, str]]:
    active_rows = [row for row in rows if row["status"] == "active"]
    if prompt_id:
        matches = [row for row in active_rows if row["prompt_id"] == prompt_id]
        if not matches:
            fail(f"active prompt_id `{prompt_id}` not found")
        return matches
    if series:
        matches = [row for row in active_rows if row["series"] == series]
        if not matches:
            fail(f"active series `{series}` not found")
        return matches
    fail("missing selection; use --prompt-id, --series, or --list-prompts")


def parse_seeds(seed_arg: str) -> list[int]:
    values: list[int] = []
    for raw in seed_arg.split(","):
        raw = raw.strip()
        if not raw:
            continue
        try:
            values.append(int(raw))
        except ValueError as exc:
            fail(f"invalid seed `{raw}`: {exc}")
    if not values:
        fail("no valid seeds were provided")
    return values


def write_json(path: Path, payload: object) -> None:
    path.write_text(json.dumps(payload, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def write_prompt_row_csv(path: Path, row: dict[str, str]) -> None:
    with path.open("w", encoding="utf-8", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=FIELDNAMES)
        writer.writeheader()
        writer.writerow({field: row.get(field, "") for field in FIELDNAMES})


def build_command(
    args: argparse.Namespace,
    row: dict[str, str],
    seed: int,
    output_path: Path,
) -> list[str]:
    command = [
        args.qwen_cli,
        "--model",
        args.model_path,
        "--prompt",
        row["prompt_text"],
        "--steps",
        row["steps"],
        "--guidance",
        row["guidance"],
        "--true-cfg-scale",
        row["true_cfg_scale"],
        "--width",
        row["width"],
        "--height",
        row["height"],
        "--seed",
        str(seed),
        "--gpu-cache-limit",
        args.gpu_cache_limit,
        "--output",
        output_path.as_posix(),
    ]
    if args.clear_cache_between_stages:
        command.append("--clear-cache-between-stages")
    negative_prompt = row["negative_prompt"]
    if negative_prompt:
        command.extend(["--negative-prompt", negative_prompt])
    if row["profile"] == "lightning":
        command.extend(["--lora", args.lightning_lora])
    elif row["profile"] != "base":
        fail(f"unsupported profile `{row['profile']}` for `{row['prompt_id']}`")
    return command


def run_one(
    args: argparse.Namespace,
    row: dict[str, str],
    seed: int,
    prompt_run_dir: Path,
) -> dict[str, str | int]:
    seed_dir = prompt_run_dir / f"seed_{seed}"
    seed_dir.mkdir(parents=True, exist_ok=True)
    image_path = seed_dir / "image.png"
    stdout_path = seed_dir / "stdout.log"
    stderr_path = seed_dir / "stderr.log"
    command = build_command(args, row, seed, image_path)
    write_json(seed_dir / "command.json", {"command": command})
    completed = subprocess.run(command, text=True, capture_output=True)
    stdout_path.write_text(completed.stdout, encoding="utf-8")
    stderr_path.write_text(completed.stderr, encoding="utf-8")
    if completed.returncode != 0:
        fail(
            f"QwenImageCLI failed for `{row['prompt_id']}` seed `{seed}`: "
            f"see {stderr_path.as_posix()}"
        )
    if not image_path.exists():
        fail(
            f"QwenImageCLI reported success for `{row['prompt_id']}` seed `{seed}` "
            f"but did not write {image_path.as_posix()}"
        )
    return {
        "seed": seed,
        "image_path": image_path.as_posix(),
        "stdout_path": stdout_path.as_posix(),
        "stderr_path": stderr_path.as_posix(),
        "command_path": (seed_dir / "command.json").as_posix(),
    }


def make_contact_sheet(image_paths: list[Path], output_path: Path) -> None:
    if not image_paths:
        return
    cell_width = 680
    cell_height = 720
    cols = 2
    rows = (len(image_paths) + cols - 1) // cols
    sheet = Image.new("RGBA", (cell_width * cols, cell_height * rows), (255, 255, 255, 255))
    for index, path in enumerate(image_paths):
        image = Image.open(path).convert("RGBA")
        canvas = Image.new("RGBA", (cell_width, cell_height), (245, 245, 245, 255))
        fitted = ImageOps.contain(image, (640, 640))
        canvas.alpha_composite(fitted, ((cell_width - fitted.width) // 2, 20))
        ImageDraw.Draw(canvas).text((20, 670), path.parent.name, fill=(20, 20, 20, 255))
        sheet.alpha_composite(canvas, ((index % cols) * cell_width, (index // cols) * cell_height))
    output_path.parent.mkdir(parents=True, exist_ok=True)
    sheet.save(output_path)


def main() -> None:
    args = parse_args()
    csv_path = Path(args.csv_path)
    rows = load_rows(csv_path)

    if args.list_prompts:
        list_prompts(rows)
        return

    prompt_rows = selected_rows(rows, args.prompt_id, args.series)
    seeds = parse_seeds(args.seeds)
    qwen_cli = Path(args.qwen_cli)
    if not qwen_cli.exists():
        fail(f"QwenImageCLI does not exist: {qwen_cli}")
    if not Path(args.model_path).exists():
        fail(f"Qwen model path does not exist: {args.model_path}")
    if any(row["profile"] == "lightning" for row in prompt_rows) and not Path(args.lightning_lora).exists():
        fail(f"Lightning LoRA does not exist: {args.lightning_lora}")

    output_root = Path(args.output_dir)
    run_root = output_root / utc_stamp()
    run_root.mkdir(parents=True, exist_ok=True)
    results: list[dict[str, object]] = []

    for row in prompt_rows:
        prompt_run_dir = run_root / row["prompt_id"]
        prompt_run_dir.mkdir(parents=True, exist_ok=True)
        (prompt_run_dir / "prompt.txt").write_text(row["prompt_text"] + "\n", encoding="utf-8")
        write_json(prompt_run_dir / "prompt_row.json", row)
        write_prompt_row_csv(prompt_run_dir / "prompt_row.csv", row)

        seed_results = []
        for seed in seeds:
            seed_results.append(run_one(args, row, seed, prompt_run_dir))

        image_paths = [Path(item["image_path"]) for item in seed_results]
        make_contact_sheet(image_paths, prompt_run_dir / "contact_sheet.png")
        prompt_result = {
            "prompt_id": row["prompt_id"],
            "series": row["series"],
            "round": row["round"],
            "label": row["label"],
            "profile": row["profile"],
            "run_dir": prompt_run_dir.as_posix(),
            "contact_sheet_path": (prompt_run_dir / "contact_sheet.png").as_posix(),
            "seed_runs": seed_results,
        }
        write_json(prompt_run_dir / "index.json", prompt_result)
        results.append(prompt_result)

    run_index = {
        "status": "ok",
        "csv_path": csv_path.as_posix(),
        "run_dir": run_root.as_posix(),
        "prompt_count": len(results),
        "seeds": seeds,
        "results": results,
    }
    write_json(run_root / "index.json", run_index)
    print(json.dumps(run_index, ensure_ascii=False))


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("interrupted", file=sys.stderr)
        raise SystemExit(130)
