#!/usr/bin/env python3

import argparse
import json
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


STATUS_SCHEMA_VERSION = 1
STATUS_DIR = Path(".threadbridge/state/codex-sync")
CURRENT_FILE = STATUS_DIR / "current.json"
EVENTS_FILE = STATUS_DIR / "events.jsonl"


def now_iso() -> str:
    return (
        datetime.now(timezone.utc)
        .isoformat(timespec="milliseconds")
        .replace("+00:00", "Z")
    )


def workspace_root(path: str | None) -> Path:
    return Path(path or ".").resolve()


def ensure_surface(workspace: Path) -> None:
    status_dir = workspace / STATUS_DIR
    status_dir.mkdir(parents=True, exist_ok=True)
    current_path = workspace / CURRENT_FILE
    if not current_path.exists():
        write_current(workspace, idle_status(workspace))
    events_path = workspace / EVENTS_FILE
    if not events_path.exists():
        events_path.write_text("", encoding="utf-8")


def idle_status(workspace: Path) -> dict[str, Any]:
    return {
        "schema_version": STATUS_SCHEMA_VERSION,
        "workspace_cwd": str(workspace.resolve()),
        "source": None,
        "phase": "idle",
        "shell_pid": None,
        "client": None,
        "session_id": None,
        "turn_id": None,
        "summary": None,
        "updated_at": now_iso(),
    }


def read_current(workspace: Path) -> dict[str, Any]:
    ensure_surface(workspace)
    current_path = workspace / CURRENT_FILE
    if not current_path.exists():
        return idle_status(workspace)
    with current_path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def write_current(workspace: Path, current: dict[str, Any]) -> None:
    current_path = workspace / CURRENT_FILE
    tmp_path = current_path.with_name(f"{current_path.name}.{now_iso()}.tmp")
    tmp_path.write_text(json.dumps(current, indent=2) + "\n", encoding="utf-8")
    tmp_path.replace(current_path)


def append_event(
    workspace: Path,
    event_name: str,
    source: str,
    payload: dict[str, Any],
) -> None:
    record = {
        "schema_version": STATUS_SCHEMA_VERSION,
        "event": event_name,
        "source": source,
        "workspace_cwd": str(workspace.resolve()),
        "occurred_at": now_iso(),
        "payload": payload,
    }
    with (workspace / EVENTS_FILE).open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(record) + "\n")


def summarize_text(value: str | None) -> str | None:
    if value is None:
        return None
    trimmed = value.strip()
    if not trimmed:
        return None
    if len(trimmed) <= 96:
        return trimmed
    return trimmed[:96] + "..."


def apply_event(
    current: dict[str, Any],
    event_name: str,
    payload: dict[str, Any],
) -> dict[str, Any]:
    next_status = dict(current)
    next_status["schema_version"] = STATUS_SCHEMA_VERSION
    next_status["workspace_cwd"] = current["workspace_cwd"]
    next_status["updated_at"] = now_iso()

    if event_name == "shell_process_started":
        next_status.update(
            {
                "source": "cli",
                "phase": "shell_active",
                "shell_pid": payload.get("shell_pid"),
                "client": payload.get("client") or "codex-cli",
            }
        )
        return next_status

    if event_name == "shell_process_exited":
        shell_pid = payload.get("shell_pid")
        if next_status.get("shell_pid") not in (None, shell_pid):
            return next_status
        next_status.update(
            {
                "source": None,
                "phase": "idle",
                "shell_pid": None,
                "client": None,
                "session_id": None,
                "turn_id": None,
                "summary": None,
            }
        )
        return next_status

    if event_name == "session_started":
        next_status.update(
            {
                "source": "cli",
                "phase": "shell_active",
                "session_id": payload.get("session_id"),
                "summary": summarize_text(payload.get("source")),
            }
        )
        return next_status

    if event_name == "user_prompt_submitted":
        next_status.update(
            {
                "source": "cli",
                "phase": "turn_running",
                "session_id": payload.get("session_id"),
                "summary": summarize_text(payload.get("prompt")),
            }
        )
        return next_status

    if event_name == "stop_reached":
        next_status.update(
            {
                "source": "cli",
                "phase": "turn_finalizing",
                "session_id": payload.get("session_id"),
            }
        )
        return next_status

    if event_name == "turn_completed":
        next_status.update(
            {
                "source": "cli" if next_status.get("shell_pid") else None,
                "phase": "shell_active" if next_status.get("shell_pid") else "idle",
                "client": payload.get("client") or next_status.get("client"),
                "session_id": payload.get("thread-id"),
                "turn_id": payload.get("turn-id"),
                "summary": summarize_text(payload.get("last-assistant-message")),
            }
        )
        if not next_status.get("shell_pid"):
            next_status.update(
                {
                    "client": None,
                    "session_id": None,
                    "turn_id": None,
                    "summary": None,
                }
            )
        return next_status

    raise ValueError(f"unsupported event: {event_name}")


def parse_hook_payload(stdin_text: str) -> dict[str, Any]:
    if not stdin_text.strip():
        return {}
    return json.loads(stdin_text)


def command_event(args: argparse.Namespace) -> int:
    workspace = workspace_root(args.workspace)
    ensure_surface(workspace)

    if args.hook_event:
        stdin_payload = parse_hook_payload(sys.stdin.read())
        hook_event = args.hook_event
        if hook_event == "SessionStart":
            event_name = "session_started"
        elif hook_event == "UserPromptSubmit":
            event_name = "user_prompt_submitted"
        elif hook_event == "Stop":
            event_name = "stop_reached"
        else:
            raise SystemExit(f"unsupported hook event: {hook_event}")
        payload = stdin_payload
    else:
        if not args.event_name:
            raise SystemExit("event name is required")
        event_name = args.event_name
        payload = {
            "shell_pid": args.shell_pid,
            "exit_code": args.exit_code,
            "client": "codex-cli",
        }

    current = read_current(workspace)
    next_status = apply_event(current, event_name, payload)
    append_event(workspace, event_name, "cli", payload)
    write_current(workspace, next_status)
    return 0


def command_notify(args: argparse.Namespace) -> int:
    workspace = workspace_root(args.workspace)
    ensure_surface(workspace)
    payload_arg = args.payload
    if payload_arg is None and len(args.extra) == 1:
        payload_arg = args.extra[0]
    if not payload_arg:
        raise SystemExit("notify payload is required")
    payload = json.loads(payload_arg)
    if payload.get("type") != "agent-turn-complete":
        return 0
    current = read_current(workspace)
    next_status = apply_event(current, "turn_completed", payload)
    append_event(workspace, "turn_completed", "cli", payload)
    write_current(workspace, next_status)
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)

    event_parser = subparsers.add_parser("event")
    event_parser.add_argument("event_name", nargs="?")
    event_parser.add_argument("--hook-event")
    event_parser.add_argument("--workspace")
    event_parser.add_argument("--shell-pid", type=int)
    event_parser.add_argument("--exit-code", type=int)
    event_parser.set_defaults(func=command_event)

    notify_parser = subparsers.add_parser("notify")
    notify_parser.add_argument("--workspace")
    notify_parser.add_argument("payload", nargs="?")
    notify_parser.add_argument("extra", nargs="*")
    notify_parser.set_defaults(func=command_notify)

    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()
    try:
        return args.func(args)
    except Exception as error:  # pragma: no cover
        print(str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
