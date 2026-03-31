#!/usr/bin/env python3
"""Rank 1024x1024 icon candidates in ./tmp with a serial Qwen scoring pass."""

from __future__ import annotations

import argparse
import base64
import csv
import json
import mimetypes
import os
import re
import sys
import urllib.error
import urllib.request
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

try:
    from PIL import Image, ImageDraw
except ImportError as exc:  # pragma: no cover - runtime dependency check
    raise SystemExit(
        "error: Pillow is required for select_top_icons_qwen.py; install it with `python3 -m pip install Pillow`"
    ) from exc


DEFAULT_INPUT_ROOT = Path("tmp")
DEFAULT_OUTPUT_ROOT = Path("tmp/qwen-icon-ranking")
DEFAULT_API_BASE = "http://ronnie-mac-studio.local:8001/v1"
DEFAULT_MODEL = "Qwen3.5-35B-A3B-8bit"
DEFAULT_TOP_K = 9
DEFAULT_TIMEOUT = 180
DEFAULT_RETRY_COUNT = 1
DEFAULT_INITIAL_MAX_TOKENS = 131072
DEFAULT_RETRY_MAX_TOKENS = 131072
TARGET_SIZE = (1024, 1024)
IMAGE_EXTENSIONS = {".png", ".jpg", ".jpeg", ".webp"}
METRIC_WEIGHTS = {
    "legibility": 0.30,
    "polish": 0.25,
    "composition": 0.20,
    "distinctiveness": 0.15,
    "icon_fit": 0.10,
}
CSV_FIELDNAMES = [
    "rank",
    "path",
    "width",
    "height",
    "model",
    "legibility",
    "polish",
    "composition",
    "distinctiveness",
    "icon_fit",
    "weighted_score",
    "summary",
    "status",
    "error",
]


@dataclass(frozen=True)
class Candidate:
    path: Path
    width: int
    height: int


@dataclass
class EvaluationResult:
    path: str
    width: int
    height: int
    model: str
    status: str
    raw_response: str
    summary: str
    metrics: dict[str, int]
    weighted_score: float | None
    error: str
    attempt_count: int


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Score 1024x1024 icon candidates in ./tmp with Qwen and select the top results."
    )
    parser.add_argument(
        "--input-root",
        default=str(DEFAULT_INPUT_ROOT),
        help=f"Root directory to scan recursively (default: {DEFAULT_INPUT_ROOT})",
    )
    parser.add_argument(
        "--output-root",
        default=str(DEFAULT_OUTPUT_ROOT),
        help=f"Directory where ranking artifacts are written (default: {DEFAULT_OUTPUT_ROOT})",
    )
    parser.add_argument(
        "--api-base",
        default=DEFAULT_API_BASE,
        help=f"OpenAI-compatible API base URL (default: {DEFAULT_API_BASE})",
    )
    parser.add_argument(
        "--model",
        default=DEFAULT_MODEL,
        help=f"Model name exposed by the API (default: {DEFAULT_MODEL})",
    )
    parser.add_argument(
        "--api-key",
        default=os.environ.get("QWEN_API_KEY") or os.environ.get("OPENAI_API_KEY"),
        help="API key. Defaults to QWEN_API_KEY or OPENAI_API_KEY from the environment.",
    )
    parser.add_argument(
        "--top-k",
        type=int,
        default=DEFAULT_TOP_K,
        help=f"How many icons to keep in the final ranking (default: {DEFAULT_TOP_K})",
    )
    parser.add_argument(
        "--limit",
        type=int,
        help="Optional cap after filtering, useful for quick validation runs.",
    )
    parser.add_argument(
        "--timeout",
        type=int,
        default=DEFAULT_TIMEOUT,
        help=f"Per-request timeout in seconds (default: {DEFAULT_TIMEOUT})",
    )
    parser.add_argument(
        "--retry-count",
        type=int,
        default=DEFAULT_RETRY_COUNT,
        help=f"How many retry attempts to make after the first parse failure (default: {DEFAULT_RETRY_COUNT})",
    )
    parser.add_argument(
        "--initial-max-tokens",
        type=int,
        default=DEFAULT_INITIAL_MAX_TOKENS,
        help=f"Max tokens for the first attempt (default: {DEFAULT_INITIAL_MAX_TOKENS})",
    )
    parser.add_argument(
        "--retry-max-tokens",
        type=int,
        default=DEFAULT_RETRY_MAX_TOKENS,
        help=f"Max tokens for retry attempts (default: {DEFAULT_RETRY_MAX_TOKENS})",
    )
    return parser.parse_args()


def fail(message: str) -> None:
    raise SystemExit(f"error: {message}")


def utc_stamp() -> str:
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def ensure_api_key(value: str | None) -> str:
    if value:
        return value
    fail("missing API key; pass --api-key or set QWEN_API_KEY / OPENAI_API_KEY")


def build_output_dir(root: Path) -> Path:
    output_dir = root / utc_stamp()
    output_dir.mkdir(parents=True, exist_ok=True)
    return output_dir


def scan_candidates(root: Path) -> list[Candidate]:
    if not root.is_dir():
        fail(f"input root does not exist or is not a directory: {root}")

    candidates: list[Candidate] = []
    for path in sorted(root.rglob("*")):
        if not path.is_file() or path.suffix.lower() not in IMAGE_EXTENSIONS:
            continue
        try:
            with Image.open(path) as image:
                width, height = image.size
        except OSError:
            continue
        if (width, height) != TARGET_SIZE:
            continue
        candidates.append(Candidate(path=path, width=width, height=height))
    return candidates


def make_system_prompt(attempt_index: int) -> str:
    if attempt_index == 0:
        return (
            "You are scoring one app icon for visual quality only. "
            "Use these criteria: legibility at small sizes, polish/material finish, "
            "composition balance, distinctiveness, and icon fit. "
            "After any reasoning, include exactly these labeled lines with integer values 0-100:\n"
            "legibility: <int>\n"
            "polish: <int>\n"
            "composition: <int>\n"
            "distinctiveness: <int>\n"
            "icon_fit: <int>\n"
            "summary: <one short sentence>"
        )
    return (
        "Return a short response. End with exactly these six lines and nothing after them:\n"
        "legibility: <int>\n"
        "polish: <int>\n"
        "composition: <int>\n"
        "distinctiveness: <int>\n"
        "icon_fit: <int>\n"
        "summary: <one short sentence>\n"
        "All scores must be integers from 0 to 100."
    )


def make_user_prompt() -> str:
    return (
        "Evaluate this single 1024x1024 app icon candidate for visual quality. "
        "Do not score brand fit. Focus only on the visual execution as an icon."
    )


def mime_type_for(path: Path) -> str:
    mime_type, _ = mimetypes.guess_type(path.name)
    return mime_type or "application/octet-stream"


def encode_data_url(path: Path) -> str:
    return f"data:{mime_type_for(path)};base64,{base64.b64encode(path.read_bytes()).decode('ascii')}"


def request_payload(model: str, image_path: Path, attempt_index: int, max_tokens: int) -> dict[str, Any]:
    return {
        "model": model,
        "messages": [
            {"role": "system", "content": make_system_prompt(attempt_index)},
            {
                "role": "user",
                "content": [
                    {"type": "text", "text": make_user_prompt()},
                    {"type": "image_url", "image_url": {"url": encode_data_url(image_path)}},
                ],
            },
        ],
        "temperature": 0,
        "max_tokens": max_tokens,
    }


def post_chat_completion(
    api_base: str,
    api_key: str,
    payload: dict[str, Any],
    timeout: int,
) -> dict[str, Any]:
    url = api_base.rstrip("/") + "/chat/completions"
    request = urllib.request.Request(
        url,
        data=json.dumps(payload).encode("utf-8"),
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {api_key}",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return json.loads(response.read().decode("utf-8"))
    except urllib.error.HTTPError as exc:
        body = exc.read().decode("utf-8", errors="replace")
        raise RuntimeError(f"HTTP {exc.code}: {body}") from exc
    except urllib.error.URLError as exc:
        raise RuntimeError(f"network error: {exc}") from exc


def extract_text_content(payload: dict[str, Any]) -> str:
    choices = payload.get("choices")
    if not isinstance(choices, list) or not choices:
        raise RuntimeError("response did not contain choices")
    first = choices[0]
    if not isinstance(first, dict):
        raise RuntimeError("response choice is not an object")
    message = first.get("message")
    if not isinstance(message, dict):
        raise RuntimeError("response choice did not contain a message")
    content = message.get("content")
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts: list[str] = []
        for item in content:
            if not isinstance(item, dict):
                continue
            if item.get("type") == "text" and isinstance(item.get("text"), str):
                parts.append(item["text"])
        if parts:
            return "\n".join(parts)
    raise RuntimeError("response message did not contain text content")


def parse_metrics(raw_response: str) -> tuple[dict[str, int], str]:
    metrics: dict[str, int] = {}
    for metric in METRIC_WEIGHTS:
        patterns = [
            rf"(?im)^\s*(?:[-*]\s*)?(?:\*\*)?{re.escape(metric)}(?:\*\*)?\s*[:=]\s*([0-9]{{1,3}})\b",
            rf"(?im)\b{re.escape(metric)}\b\s*[:=]\s*([0-9]{{1,3}})\b",
        ]
        matches: list[str] = []
        for pattern in patterns:
            matches.extend(re.findall(pattern, raw_response))
        if not matches:
            raise ValueError(f"missing metric: {metric}")
        value = int(matches[-1])
        if value < 0 or value > 100:
            raise ValueError(f"metric out of range: {metric}={value}")
        metrics[metric] = value

    summary_matches = re.findall(
        r"(?im)^\s*(?:[-*]\s*)?(?:\*\*)?summary(?:\*\*)?\s*[:=]\s*(.+)$", raw_response
    )
    summary = summary_matches[-1].strip() if summary_matches else ""
    if not summary:
        nonempty_lines = [line.strip() for line in raw_response.splitlines() if line.strip()]
        if nonempty_lines:
            summary = nonempty_lines[-1][:180]
    return metrics, summary


def compute_weighted_score(metrics: dict[str, int]) -> float:
    total = 0.0
    for metric, weight in METRIC_WEIGHTS.items():
        total += metrics[metric] * weight
    return round(total, 2)


def evaluate_candidate(
    candidate: Candidate,
    *,
    api_base: str,
    api_key: str,
    model: str,
    timeout: int,
    retry_count: int,
    initial_max_tokens: int,
    retry_max_tokens: int,
) -> EvaluationResult:
    max_attempts = retry_count + 1
    last_error = ""
    last_raw_response = ""

    for attempt_index in range(max_attempts):
        max_tokens = initial_max_tokens if attempt_index == 0 else retry_max_tokens
        try:
            payload = request_payload(model, candidate.path, attempt_index, max_tokens)
            response = post_chat_completion(api_base, api_key, payload, timeout)
            raw_response = extract_text_content(response)
            metrics, summary = parse_metrics(raw_response)
            return EvaluationResult(
                path=candidate.path.as_posix(),
                width=candidate.width,
                height=candidate.height,
                model=model,
                status="ok",
                raw_response=raw_response,
                summary=summary,
                metrics=metrics,
                weighted_score=compute_weighted_score(metrics),
                error="",
                attempt_count=attempt_index + 1,
            )
        except Exception as exc:  # pragma: no cover - exercised in live runs
            last_error = str(exc)
            last_raw_response = raw_response if "raw_response" in locals() else ""

    return EvaluationResult(
        path=candidate.path.as_posix(),
        width=candidate.width,
        height=candidate.height,
        model=model,
        status="failed",
        raw_response=last_raw_response,
        summary="",
        metrics={},
        weighted_score=None,
        error=last_error,
        attempt_count=max_attempts,
    )


def write_json(path: Path, payload: object) -> None:
    path.write_text(json.dumps(payload, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def append_jsonl(path: Path, payload: object) -> None:
    with path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(payload, ensure_ascii=False) + "\n")


def result_to_json(result: EvaluationResult) -> dict[str, Any]:
    payload: dict[str, Any] = {
        "path": result.path,
        "width": result.width,
        "height": result.height,
        "model": result.model,
        "status": result.status,
        "summary": result.summary,
        "weighted_score": result.weighted_score,
        "raw_response": result.raw_response,
        "error": result.error,
        "attempt_count": result.attempt_count,
    }
    payload.update(result.metrics)
    return payload


def write_csv(path: Path, rows: list[dict[str, Any]]) -> None:
    with path.open("w", encoding="utf-8", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=CSV_FIELDNAMES)
        writer.writeheader()
        writer.writerows(rows)


def display_label(path_str: str, max_length: int = 52) -> str:
    path = Path(path_str)
    parts = path.parts[-4:] if len(path.parts) >= 4 else path.parts
    label = "/".join(parts)
    if len(label) <= max_length:
        return label
    return "..." + label[-(max_length - 3) :]


def build_contact_sheet(results: list[EvaluationResult], output_path: Path) -> None:
    if not results:
        return

    columns = 3
    rows = min(3, (len(results) + columns - 1) // columns)
    padding = 32
    tile_size = 420
    label_height = 96
    width = columns * tile_size + (columns + 1) * padding
    height = rows * (tile_size + label_height) + (rows + 1) * padding

    sheet = Image.new("RGBA", (width, height), (15, 21, 29, 255))
    draw = ImageDraw.Draw(sheet)
    font = None

    for index, result in enumerate(results):
        row = index // columns
        column = index % columns
        x = padding + column * (tile_size + padding)
        y = padding + row * (tile_size + label_height + padding)

        with Image.open(result.path) as image:
            tile = image.convert("RGBA").resize((tile_size, tile_size), Image.LANCZOS)
        cell_bg = Image.new("RGBA", (tile_size, tile_size), (27, 34, 44, 255))
        composed = Image.alpha_composite(cell_bg, tile)
        sheet.paste(composed, (x, y))

        label_top = y + tile_size + 10
        label_text = [
            f"#{index + 1}  {result.weighted_score:.2f}",
            display_label(result.path),
        ]
        for line_index, line in enumerate(label_text):
            draw.text(
                (x, label_top + line_index * 28),
                line,
                fill=(240, 244, 248, 255),
                font=font,
            )

    output_path.parent.mkdir(parents=True, exist_ok=True)
    sheet.convert("RGB").save(output_path)


def build_csv_rows(results: list[EvaluationResult]) -> list[dict[str, Any]]:
    ranked_successes = [
        result
        for result in sorted(
            results,
            key=lambda item: (
                item.weighted_score is None,
                -(item.weighted_score or -1.0),
                item.path,
            ),
        )
    ]

    rows: list[dict[str, Any]] = []
    rank = 1
    for result in ranked_successes:
        row = {
            "rank": rank if result.weighted_score is not None else "",
            "path": result.path,
            "width": result.width,
            "height": result.height,
            "model": result.model,
            "legibility": result.metrics.get("legibility", ""),
            "polish": result.metrics.get("polish", ""),
            "composition": result.metrics.get("composition", ""),
            "distinctiveness": result.metrics.get("distinctiveness", ""),
            "icon_fit": result.metrics.get("icon_fit", ""),
            "weighted_score": f"{result.weighted_score:.2f}" if result.weighted_score is not None else "",
            "summary": result.summary,
            "status": result.status,
            "error": result.error,
        }
        if result.weighted_score is not None:
            rank += 1
        rows.append(row)
    return rows


def print_summary(results: list[EvaluationResult], top_k: int) -> None:
    successes = [result for result in results if result.weighted_score is not None]
    failures = [result for result in results if result.weighted_score is None]
    print(f"processed={len(results)} ok={len(successes)} failed={len(failures)}")
    for index, result in enumerate(
        sorted(successes, key=lambda item: (-float(item.weighted_score or 0.0), item.path))[:top_k],
        start=1,
    ):
        print(f"{index}. {result.weighted_score:.2f} {result.path}")


def main() -> None:
    args = parse_args()
    api_key = ensure_api_key(args.api_key)
    input_root = Path(args.input_root)
    output_root = Path(args.output_root)
    output_dir = build_output_dir(output_root)
    scores_jsonl_path = output_dir / "scores.jsonl"

    candidates = scan_candidates(input_root)
    if args.limit is not None:
        candidates = candidates[: args.limit]
    if not candidates:
        fail(f"no {TARGET_SIZE[0]}x{TARGET_SIZE[1]} image candidates found under {input_root}")

    write_json(
        output_dir / "candidates.json",
        {
            "input_root": input_root.as_posix(),
            "target_size": list(TARGET_SIZE),
            "count": len(candidates),
            "paths": [candidate.path.as_posix() for candidate in candidates],
        },
    )

    results: list[EvaluationResult] = []
    for index, candidate in enumerate(candidates, start=1):
        print(f"[{index}/{len(candidates)}] {candidate.path.as_posix()}", file=sys.stderr)
        result = evaluate_candidate(
            candidate,
            api_base=args.api_base,
            api_key=api_key,
            model=args.model,
            timeout=args.timeout,
            retry_count=args.retry_count,
            initial_max_tokens=args.initial_max_tokens,
            retry_max_tokens=args.retry_max_tokens,
        )
        results.append(result)
        append_jsonl(scores_jsonl_path, result_to_json(result))

    successes = sorted(
        [result for result in results if result.weighted_score is not None],
        key=lambda item: (-float(item.weighted_score or 0.0), item.path),
    )
    top_results = successes[: args.top_k]

    write_json(output_dir / "top9.json", [result_to_json(result) for result in top_results])
    write_csv(output_dir / "scores.csv", build_csv_rows(results))
    build_contact_sheet(top_results, output_dir / "top9_contact_sheet.png")

    metadata = {
        "input_root": input_root.as_posix(),
        "output_dir": output_dir.as_posix(),
        "target_size": list(TARGET_SIZE),
        "candidate_count": len(candidates),
        "success_count": len(successes),
        "failure_count": len(results) - len(successes),
        "top_k": args.top_k,
        "model": args.model,
    }
    write_json(output_dir / "run_summary.json", metadata)
    print_summary(results, args.top_k)


if __name__ == "__main__":
    main()
