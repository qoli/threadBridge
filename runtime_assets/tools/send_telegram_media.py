#!/usr/bin/env python3
import argparse
import json
import sys
from pathlib import Path
from typing import Optional


def fail(message: str) -> None:
    raise SystemExit(message)


def ensure_object(value, label: str) -> dict:
    if not isinstance(value, dict):
        fail(f"Invalid {label}: expected an object.")
    return value


def ensure_non_empty_string(value, label: str) -> str:
    if not isinstance(value, str) or not value.strip():
        fail(f"Invalid {label}.")
    return value.strip()


def optional_non_empty_string(value, label: str) -> Optional[str]:
    if value is None:
        return None
    if not isinstance(value, str):
        fail(f"Invalid {label}.")
    trimmed = value.strip()
    return trimmed or None


def normalize_surface(value, label: str) -> str:
    if value is None:
        return "content"
    surface = ensure_non_empty_string(value, label)
    allowed = {"content", "status", "draft", "control", "edit"}
    if surface not in allowed:
        fail(f"Unsupported {label}: {surface}")
    return surface


def normalize_workspace_file_path(workspace_dir: Path, value, label: str) -> str:
    raw = ensure_non_empty_string(value, label)
    candidate = Path(raw)
    if not candidate.is_absolute():
        candidate = workspace_dir / candidate
    try:
        resolved = candidate.resolve(strict=True)
    except FileNotFoundError:
        fail(f"Missing {label}: {candidate}")
    workspace_root = workspace_dir.resolve()
    try:
        relative = resolved.relative_to(workspace_root)
    except ValueError:
        fail(f"{label} must stay inside the current workspace: {resolved}")
    if not resolved.is_file():
        fail(f"{label} must point to a file: {resolved}")
    return relative.as_posix()


def parse_item(workspace_dir: Path, value, index: int) -> dict:
    item = ensure_object(value, f"request.items[{index}]")
    item_type = ensure_non_empty_string(item.get("type"), f"request.items[{index}].type")
    if item_type == "text":
        return {
            "type": "text",
            "text": ensure_non_empty_string(item.get("text"), f"request.items[{index}].text"),
            "surface": normalize_surface(item.get("surface"), f"request.items[{index}].surface"),
        }
    if item_type in {"photo", "document"}:
        normalized = {
            "type": item_type,
            "path": normalize_workspace_file_path(
                workspace_dir,
                item.get("path"),
                f"request.items[{index}].path",
            ),
            "surface": normalize_surface(item.get("surface"), f"request.items[{index}].surface"),
        }
        caption = optional_non_empty_string(item.get("caption"), f"request.items[{index}].caption")
        if caption is not None:
            normalized["caption"] = caption
        return normalized
    fail(f"Unsupported request.items[{index}].type: {item_type}")


def load_existing_outbox(path: Path) -> list[dict]:
    if not path.exists():
        return []
    existing = ensure_object(json.loads(path.read_text(encoding="utf-8")), "telegram outbox")
    items = existing.get("items")
    if not isinstance(items, list):
        fail("Invalid telegram outbox.")
    return [ensure_object(item, "telegram outbox item") for item in items]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--config-env")
    parser.parse_args()

    workspace_dir = Path.cwd()
    runtime_dir = workspace_dir / ".threadbridge"
    tool_requests_dir = runtime_dir / "tool_requests"
    tool_results_dir = runtime_dir / "tool_results"
    tool_requests_dir.mkdir(parents=True, exist_ok=True)
    tool_results_dir.mkdir(parents=True, exist_ok=True)

    request_path = tool_requests_dir / "send_telegram_media.request.json"
    outbox_path = tool_results_dir / "telegram_outbox.json"
    result_path = tool_results_dir / "send_telegram_media.result.json"

    if not request_path.exists():
        fail(f"Missing {request_path}.")

    request = ensure_object(json.loads(request_path.read_text(encoding="utf-8")), "request")
    items = request.get("items")
    if not isinstance(items, list) or not items:
        fail("request.items must be a non-empty array.")

    normalized_items = [
        parse_item(workspace_dir, value, index) for index, value in enumerate(items)
    ]

    outbox_items = load_existing_outbox(outbox_path)
    outbox_items.extend(normalized_items)
    outbox = {"items": outbox_items}
    outbox_path.write_text(
        json.dumps(outbox, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )

    result = {
        "outbox_path": outbox_path.relative_to(workspace_dir).as_posix(),
        "queued_count": len(normalized_items),
        "total_pending_count": len(outbox_items),
    }
    result_path.write_text(
        json.dumps(result, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    print(json.dumps({"status": "ok", **result}, ensure_ascii=False))


if __name__ == "__main__":
    try:
        main()
    except SystemExit:
        raise
    except Exception as error:
        print(str(error), file=sys.stderr)
        raise
