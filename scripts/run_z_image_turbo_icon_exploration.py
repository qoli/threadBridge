#!/usr/bin/env python3
import argparse
import base64
import csv
import json
import sys
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path


DEFAULT_API_BASE = "http://localhost:11434"
DEFAULT_CSV_PATH = "icon/exploration/z-image-turbo/prompts.csv"
DEFAULT_MODEL = "x/z-image-turbo:latest"
DEFAULT_SIZE = "1024x1024"
PROMPT_FIELDNAMES = [
    "prompt_id",
    "series",
    "round",
    "label",
    "parent_prompt_id",
    "status",
    "prompt_text",
    "notes",
]


def fail(message: str) -> None:
    raise SystemExit(message)


def utc_stamp() -> str:
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run a single app-icon exploration prompt from the CSV registry."
    )
    parser.add_argument(
        "--csv-path",
        default=DEFAULT_CSV_PATH,
        help="CSV registry that stores all prompts.",
    )
    parser.add_argument(
        "--prompt-id",
        help="Prompt identifier to execute. Required unless --list-prompts is used.",
    )
    parser.add_argument(
        "--list-prompts",
        action="store_true",
        help="List prompts from the CSV registry and exit.",
    )
    parser.add_argument(
        "--output-dir",
        default="tmp/z-image-turbo-icon-exploration",
        help="Directory where run artifacts are written.",
    )
    parser.add_argument(
        "--api-base",
        default=DEFAULT_API_BASE,
        help="Base URL for the Ollama server.",
    )
    parser.add_argument(
        "--model",
        default=DEFAULT_MODEL,
        help="Image model name exposed by Ollama.",
    )
    parser.add_argument(
        "--size",
        default=DEFAULT_SIZE,
        help='Output size, for example "1024x1024".',
    )
    parser.add_argument(
        "--n",
        type=int,
        default=1,
        help="Number of images to request.",
    )
    parser.add_argument(
        "--timeout",
        type=int,
        default=600,
        help="HTTP timeout in seconds.",
    )
    return parser.parse_args()


def load_prompt_rows(csv_path: Path) -> list[dict[str, str]]:
    if not csv_path.exists():
        fail(f"prompt CSV does not exist: {csv_path}")
    with csv_path.open("r", encoding="utf-8", newline="") as handle:
        reader = csv.DictReader(handle)
        if reader.fieldnames != PROMPT_FIELDNAMES:
            fail(
                "prompt CSV has unexpected columns: "
                f"expected {PROMPT_FIELDNAMES}, got {reader.fieldnames}"
            )
        rows = []
        for raw_row in reader:
            row = {field: (raw_row.get(field) or "").strip() for field in PROMPT_FIELDNAMES}
            if not row["prompt_id"]:
                fail("prompt CSV contains a row with an empty prompt_id")
            rows.append(row)
    if not rows:
        fail(f"prompt CSV is empty: {csv_path}")
    return rows


def list_prompts(rows: list[dict[str, str]]) -> None:
    print("prompt_id\tseries\tround\tstatus\tlabel")
    for row in rows:
        print(
            "\t".join(
                [
                    row["prompt_id"],
                    row["series"],
                    row["round"] or "-",
                    row["status"],
                    row["label"],
                ]
            )
        )


def resolve_prompt_row(rows: list[dict[str, str]], prompt_id: str) -> dict[str, str]:
    for row in rows:
        if row["prompt_id"] == prompt_id:
            if row["status"] != "active":
                fail(f"prompt_id `{prompt_id}` is not active")
            if not row["prompt_text"]:
                fail(f"prompt_id `{prompt_id}` has an empty prompt_text")
            return row
    fail(f"prompt_id `{prompt_id}` was not found in the CSV registry")


def request_payload(model: str, prompt: str, size: str, n: int) -> dict:
    return {
        "model": model,
        "prompt": prompt,
        "size": size,
        "response_format": "b64_json",
        "n": n,
    }


def post_json(url: str, payload: dict, timeout: int) -> str:
    request = urllib.request.Request(
        url,
        data=json.dumps(payload).encode("utf-8"),
        headers={
            "Content-Type": "application/json",
            "Authorization": "Bearer ollama",
        },
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return response.read().decode("utf-8")


def decode_images(response_json: dict) -> list[bytes]:
    data = response_json.get("data")
    if not isinstance(data, list):
        raise ValueError("response.data is missing")
    images: list[bytes] = []
    for item in data:
        if not isinstance(item, dict):
            continue
        encoded = item.get("b64_json")
        if isinstance(encoded, str) and encoded:
            images.append(base64.b64decode(encoded))
    if not images:
        raise ValueError("response did not contain any b64_json images")
    return images


def write_json(path: Path, payload: object) -> None:
    path.write_text(json.dumps(payload, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def write_prompt_row_csv(path: Path, row: dict[str, str]) -> None:
    with path.open("w", encoding="utf-8", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=PROMPT_FIELDNAMES)
        writer.writeheader()
        writer.writerow({field: row.get(field, "") for field in PROMPT_FIELDNAMES})


def main() -> None:
    args = parse_args()
    csv_path = Path(args.csv_path)
    rows = load_prompt_rows(csv_path)

    if args.list_prompts:
        list_prompts(rows)
        return

    if not args.prompt_id:
        fail("missing --prompt-id; use --list-prompts to inspect available ids")

    row = resolve_prompt_row(rows, args.prompt_id)
    output_root = Path(args.output_dir)
    run_root = output_root / utc_stamp()
    prompt_run_dir = run_root / row["prompt_id"]
    prompt_run_dir.mkdir(parents=True, exist_ok=True)

    prompt_text = row["prompt_text"]
    request_json = request_payload(args.model, prompt_text, args.size, args.n)
    request_path = prompt_run_dir / "request.json"
    response_path = prompt_run_dir / "response.json"
    prompt_text_path = prompt_run_dir / "prompt.txt"
    prompt_row_json_path = prompt_run_dir / "prompt_row.json"
    prompt_row_csv_path = prompt_run_dir / "prompt_row.csv"

    prompt_text_path.write_text(prompt_text + "\n", encoding="utf-8")
    write_json(prompt_row_json_path, row)
    write_prompt_row_csv(prompt_row_csv_path, row)
    write_json(request_path, request_json)

    endpoint = args.api_base.rstrip("/") + "/v1/images/generations"

    try:
        response_text = post_json(endpoint, request_json, args.timeout)
        response_path.write_text(response_text, encoding="utf-8")
        response_json = json.loads(response_text)
        images = decode_images(response_json)
    except urllib.error.HTTPError as error:
        body = error.read().decode("utf-8", errors="replace")
        response_path.write_text(body, encoding="utf-8")
        fail(f"Ollama image request failed with HTTP {error.code}: {body}")

    image_paths: list[str] = []
    for index, image_bytes in enumerate(images, start=1):
        image_path = prompt_run_dir / f"{index:04d}.png"
        image_path.write_bytes(image_bytes)
        image_paths.append(image_path.as_posix())

    result = {
        "run_dir": run_root.as_posix(),
        "prompt_id": row["prompt_id"],
        "series": row["series"],
        "round": row["round"],
        "label": row["label"],
        "model": args.model,
        "csv_path": csv_path.as_posix(),
        "prompt_row_csv_path": prompt_row_csv_path.as_posix(),
        "prompt_row_json_path": prompt_row_json_path.as_posix(),
        "prompt_text_path": prompt_text_path.as_posix(),
        "request_path": request_path.as_posix(),
        "response_path": response_path.as_posix(),
        "image_count": len(image_paths),
        "image_paths": image_paths,
    }
    write_json(run_root / "index.json", result)
    print(json.dumps({"status": "ok", **result}, ensure_ascii=False))


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("interrupted", file=sys.stderr)
        raise SystemExit(130)
