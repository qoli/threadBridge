#!/usr/bin/env python3

import argparse
import json
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


STATUS_SCHEMA_VERSION = 2
STATUS_DIR = Path(".threadbridge/state/codex-sync")
CURRENT_FILE = STATUS_DIR / "current.json"
EVENTS_FILE = STATUS_DIR / "events.jsonl"
SESSIONS_DIR = STATUS_DIR / "sessions"
CLI_OWNER_FILE = STATUS_DIR / "cli-owner.json"
ATTACH_INTENT_FILE = STATUS_DIR / "attach-intent.json"


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
    (workspace / SESSIONS_DIR).mkdir(parents=True, exist_ok=True)
    current_path = workspace / CURRENT_FILE
    if not current_path.exists():
        write_current(workspace, default_workspace_status(workspace))
    events_path = workspace / EVENTS_FILE
    if not events_path.exists():
        events_path.write_text("", encoding="utf-8")


def read_json_file(path: Path) -> dict[str, Any] | None:
    if not path.exists():
        return None
    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def atomic_write_json(path: Path, value: dict[str, Any]) -> None:
    tmp_path = path.with_name(
        f"{path.name}.{int(datetime.now(timezone.utc).timestamp() * 1000)}.tmp"
    )
    tmp_path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")
    tmp_path.replace(path)


def default_workspace_status(workspace: Path) -> dict[str, Any]:
    return {
        "schema_version": STATUS_SCHEMA_VERSION,
        "workspace_cwd": str(workspace.resolve()),
        "live_cli_session_ids": [],
        "active_shell_pids": [],
        "updated_at": now_iso(),
    }


def session_file_name(session_id: str) -> str:
    safe = "".join(
        ch if (ch.isalnum() or ch in "-_.") else "_" for ch in session_id
    )
    return f"{safe}.json"


def session_status_path(workspace: Path, session_id: str) -> Path:
    return workspace / SESSIONS_DIR / session_file_name(session_id)


def default_session_status(
    workspace: Path, session_id: str, owner: str = "cli"
) -> dict[str, Any]:
    return {
        "schema_version": STATUS_SCHEMA_VERSION,
        "workspace_cwd": str(workspace.resolve()),
        "session_id": session_id,
        "owner": owner,
        "live": owner == "cli",
        "phase": "idle",
        "shell_pid": None,
        "child_pid": None,
        "child_pgid": None,
        "child_command": None,
        "client": None,
        "turn_id": None,
        "summary": None,
        "updated_at": now_iso(),
    }


def read_current(workspace: Path) -> dict[str, Any]:
    ensure_surface(workspace)
    current_path = workspace / CURRENT_FILE
    if not current_path.exists():
        return default_workspace_status(workspace)
    with current_path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def write_current(workspace: Path, current: dict[str, Any]) -> None:
    current_path = workspace / CURRENT_FILE
    atomic_write_json(current_path, current)


def read_session(workspace: Path, session_id: str) -> dict[str, Any] | None:
    path = session_status_path(workspace, session_id)
    if not path.exists():
        return None
    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def write_session(workspace: Path, session: dict[str, Any]) -> None:
    path = session_status_path(workspace, session["session_id"])
    atomic_write_json(path, session)


def read_owner_claim(workspace: Path) -> dict[str, Any] | None:
    return read_json_file(workspace / CLI_OWNER_FILE)


def write_owner_claim(workspace: Path, claim: dict[str, Any]) -> None:
    claim["schema_version"] = STATUS_SCHEMA_VERSION
    claim["workspace_cwd"] = str(workspace.resolve())
    atomic_write_json(workspace / CLI_OWNER_FILE, claim)


def remove_owner_claim(workspace: Path) -> None:
    path = workspace / CLI_OWNER_FILE
    try:
        path.unlink()
    except FileNotFoundError:
        return


def read_attach_intent(workspace: Path) -> dict[str, Any] | None:
    return read_json_file(workspace / ATTACH_INTENT_FILE)


def write_attach_intent(workspace: Path, intent: dict[str, Any]) -> None:
    intent["schema_version"] = STATUS_SCHEMA_VERSION
    intent["workspace_cwd"] = str(workspace.resolve())
    atomic_write_json(workspace / ATTACH_INTENT_FILE, intent)


def remove_attach_intent(workspace: Path) -> None:
    path = workspace / ATTACH_INTENT_FILE
    try:
        path.unlink()
    except FileNotFoundError:
        return


def list_sessions(workspace: Path) -> list[dict[str, Any]]:
    root = workspace / SESSIONS_DIR
    if not root.exists():
        return []
    sessions = []
    for path in root.glob("*.json"):
        with path.open("r", encoding="utf-8") as handle:
            sessions.append(json.load(handle))
    sessions.sort(key=lambda item: item.get("updated_at", ""), reverse=True)
    return sessions


def refresh_current(workspace: Path, current: dict[str, Any]) -> dict[str, Any]:
    live_cli_session_ids = sorted(
        {
            session["session_id"]
            for session in list_sessions(workspace)
            if session.get("owner") == "cli" and session.get("live")
        }
    )
    current["schema_version"] = STATUS_SCHEMA_VERSION
    current["workspace_cwd"] = str(workspace.resolve())
    current["live_cli_session_ids"] = live_cli_session_ids
    current["updated_at"] = now_iso()
    write_current(workspace, current)
    return current


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


def normalize_session(
    workspace: Path,
    session_id: str,
    owner: str = "cli",
) -> dict[str, Any]:
    existing = read_session(workspace, session_id)
    if existing is None:
        existing = default_session_status(workspace, session_id, owner=owner)
    existing["schema_version"] = STATUS_SCHEMA_VERSION
    existing["workspace_cwd"] = str(workspace.resolve())
    existing["session_id"] = session_id
    existing["updated_at"] = now_iso()
    return existing


def apply_shell_exit(workspace: Path, shell_pid: int | None) -> None:
    if shell_pid is None:
        return
    for session in list_sessions(workspace):
        if session.get("owner") != "cli":
            continue
        if session.get("shell_pid") != shell_pid:
            continue
        session["live"] = False
        session["phase"] = "idle"
        session["turn_id"] = None
        session["updated_at"] = now_iso()
        write_session(workspace, session)
    claim = read_owner_claim(workspace)
    if claim and claim.get("shell_pid") == shell_pid:
        remove_owner_claim(workspace)


def apply_event(
    workspace: Path,
    current: dict[str, Any],
    event_name: str,
    payload: dict[str, Any],
) -> dict[str, Any]:
    current["schema_version"] = STATUS_SCHEMA_VERSION
    current["workspace_cwd"] = str(workspace.resolve())
    current.setdefault("active_shell_pids", [])

    shell_pid = payload.get("shell_pid")

    if event_name == "shell_process_started":
        if shell_pid is not None and shell_pid not in current["active_shell_pids"]:
            current["active_shell_pids"].append(shell_pid)
        return refresh_current(workspace, current)

    if event_name == "shell_process_exited":
        current["active_shell_pids"] = [
            pid for pid in current.get("active_shell_pids", []) if pid != shell_pid
        ]
        apply_shell_exit(workspace, shell_pid)
        return refresh_current(workspace, current)

    if event_name == "session_started":
        session_id = payload.get("session_id")
        if not session_id:
            return refresh_current(workspace, current)
        claim = read_owner_claim(workspace)
        session = normalize_session(workspace, session_id, owner="cli")
        session.update(
            {
                "owner": "cli",
                "live": True,
                "phase": "shell_active",
                "shell_pid": shell_pid or session.get("shell_pid"),
                "child_pid": claim.get("child_pid") if claim else session.get("child_pid"),
                "child_pgid": claim.get("child_pgid") if claim else session.get("child_pgid"),
                "child_command": claim.get("child_command")
                if claim
                else session.get("child_command"),
                "client": payload.get("client") or "codex-cli",
                "summary": summarize_text(payload.get("source")),
                "turn_id": None,
            }
        )
        write_session(workspace, session)
        owner_thread_key = payload.get("owner_thread_key")
        if claim and claim.get("shell_pid") == shell_pid:
            claim["session_id"] = session_id
            claim["updated_at"] = now_iso()
            write_owner_claim(workspace, claim)
        elif owner_thread_key:
            write_owner_claim(
                workspace,
                {
                    "thread_key": owner_thread_key,
                    "shell_pid": shell_pid,
                    "session_id": session_id,
                    "child_pid": None,
                    "child_pgid": None,
                    "child_command": None,
                    "started_at": now_iso(),
                    "updated_at": now_iso(),
                },
            )
        return refresh_current(workspace, current)

    if event_name == "user_prompt_submitted":
        session_id = payload.get("session_id")
        if not session_id:
            return refresh_current(workspace, current)
        session = normalize_session(workspace, session_id, owner="cli")
        session.update(
            {
                "owner": "cli",
                "live": True,
                "phase": "turn_running",
                "shell_pid": shell_pid or session.get("shell_pid"),
                "child_pid": session.get("child_pid"),
                "child_pgid": session.get("child_pgid"),
                "child_command": session.get("child_command"),
                "client": payload.get("client") or session.get("client") or "codex-cli",
                "summary": summarize_text(payload.get("prompt")),
            }
        )
        write_session(workspace, session)
        return refresh_current(workspace, current)

    if event_name == "stop_reached":
        session_id = payload.get("session_id")
        if not session_id:
            return refresh_current(workspace, current)
        session = normalize_session(workspace, session_id, owner="cli")
        session.update(
            {
                "owner": "cli",
                "live": True,
                "phase": "turn_finalizing",
                "shell_pid": shell_pid or session.get("shell_pid"),
                "child_pid": session.get("child_pid"),
                "child_pgid": session.get("child_pgid"),
                "child_command": session.get("child_command"),
                "client": payload.get("client") or session.get("client") or "codex-cli",
            }
        )
        write_session(workspace, session)
        return refresh_current(workspace, current)

    if event_name == "turn_completed":
        session_id = payload.get("thread-id")
        if not session_id:
            return refresh_current(workspace, current)
        session = normalize_session(workspace, session_id, owner="cli")
        session.update(
            {
                "owner": "cli",
                "phase": "shell_active" if session.get("live") else "idle",
                "child_pid": session.get("child_pid"),
                "child_pgid": session.get("child_pgid"),
                "child_command": session.get("child_command"),
                "client": payload.get("client") or session.get("client") or "codex-cli",
                "turn_id": payload.get("turn-id"),
                "summary": summarize_text(payload.get("last-assistant-message")),
            }
        )
        write_session(workspace, session)
        return refresh_current(workspace, current)

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
        payload = dict(stdin_payload)
        if args.shell_pid is not None:
            payload["shell_pid"] = args.shell_pid
        payload.setdefault("client", "codex-cli")
        owner_thread_key = args.owner_thread_key or payload.get("owner_thread_key")
        if owner_thread_key:
            payload["owner_thread_key"] = owner_thread_key
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
    next_status = apply_event(workspace, current, event_name, payload)
    append_event(workspace, event_name, "cli", payload)
    write_current(workspace, next_status)
    return 0


def list_active_bound_threads(data_root: Path, workspace: Path) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    if not data_root.exists():
        return records
    workspace_cwd = str(workspace.resolve())
    for entry in data_root.iterdir():
        if not entry.is_dir():
            continue
        metadata = read_json_file(entry / "metadata.json")
        binding = read_json_file(entry / "session-binding.json")
        if not metadata or not binding:
            continue
        if metadata.get("scope") != "thread":
            continue
        if metadata.get("status") != "active":
            continue
        if binding.get("workspace_cwd") != workspace_cwd:
            continue
        records.append(
            {
                "thread_key": metadata.get("thread_key"),
                "title": metadata.get("title"),
                "message_thread_id": metadata.get("message_thread_id"),
            }
        )
    records.sort(key=lambda item: item.get("thread_key") or "")
    return records


def clear_workspace_cli_handoff_bindings(data_root: Path, workspace: Path) -> int:
    if not data_root.exists():
        return 0

    workspace_cwd = str(workspace.resolve())
    cleared = 0
    for entry in data_root.iterdir():
        if not entry.is_dir():
            continue
        metadata = read_json_file(entry / "metadata.json")
        binding_path = entry / "session-binding.json"
        binding = read_json_file(binding_path)
        if not metadata or not binding:
            continue
        if metadata.get("scope") != "thread":
            continue
        if metadata.get("status") != "active":
            continue
        if binding.get("workspace_cwd") != workspace_cwd:
            continue
        if binding.get("attachment_state") != "cli_handoff":
            continue
        binding["attachment_state"] = "none"
        binding["updated_at"] = now_iso()
        binding["last_verified_at"] = binding.get("last_verified_at") or binding["updated_at"]
        atomic_write_json(binding_path, binding)
        cleared += 1

    return cleared


def resolve_owner_thread(
    data_root: Path, workspace: Path, requested_thread_key: str | None
) -> str:
    active = list_active_bound_threads(data_root, workspace)
    if requested_thread_key:
        for item in active:
            if item.get("thread_key") == requested_thread_key:
                return requested_thread_key
        raise SystemExit(
            f"thread_key {requested_thread_key!r} is not an active bound thread for {workspace}"
        )
    if len(active) == 1:
        return active[0]["thread_key"]
    if not active:
        raise SystemExit(f"no active bound Telegram threads are available for {workspace}")
    details = "\n".join(
        f"- {item['thread_key']} (topic={item.get('message_thread_id')})" for item in active
    )
    raise SystemExit(
        "multiple active bound Telegram threads are available for this workspace.\n"
        "rerun hcodex with --thread-key <thread-key> using one of:\n"
        f"{details}"
    )


def command_prepare_launch(args: argparse.Namespace) -> int:
    workspace = workspace_root(args.workspace)
    ensure_surface(workspace)
    data_root = Path(args.data_root).resolve()
    clear_workspace_cli_handoff_bindings(data_root, workspace)
    thread_key = resolve_owner_thread(data_root, workspace, args.thread_key)
    current = read_current(workspace)
    claim = read_owner_claim(workspace)
    if current.get("live_cli_session_ids"):
        if claim and claim.get("shell_pid") == args.shell_pid:
            print(thread_key)
            return 0
        raise SystemExit(
            "a live Codex CLI session is already managed in this workspace; attach or resume that session instead of starting another one"
        )
    if claim and claim.get("shell_pid") != args.shell_pid:
        raise SystemExit(
            f"workspace already has a managed CLI owner for thread {claim.get('thread_key')}"
        )
    now = now_iso()
    write_owner_claim(
        workspace,
        {
            "thread_key": thread_key,
            "shell_pid": args.shell_pid,
            "session_id": claim.get("session_id") if claim else None,
            "child_pid": claim.get("child_pid") if claim else None,
            "child_pgid": claim.get("child_pgid") if claim else None,
            "child_command": claim.get("child_command") if claim else None,
            "started_at": claim.get("started_at", now) if claim else now,
            "updated_at": now,
        },
    )
    print(thread_key)
    return 0


def command_record_child_process(args: argparse.Namespace) -> int:
    workspace = workspace_root(args.workspace)
    ensure_surface(workspace)
    claim = read_owner_claim(workspace)
    if not claim or claim.get("shell_pid") != args.shell_pid:
        return 0
    claim["child_pid"] = args.child_pid
    claim["child_pgid"] = args.child_pgid
    claim["child_command"] = args.child_command
    claim["updated_at"] = now_iso()
    write_owner_claim(workspace, claim)

    session_id = claim.get("session_id")
    if session_id:
        session = normalize_session(workspace, session_id, owner="cli")
        session["child_pid"] = args.child_pid
        session["child_pgid"] = args.child_pgid
        session["child_command"] = args.child_command
        write_session(workspace, session)
        refresh_current(workspace, read_current(workspace))
    return 0


def command_consume_attach_intent(args: argparse.Namespace) -> int:
    workspace = workspace_root(args.workspace)
    ensure_surface(workspace)
    intent = read_attach_intent(workspace)
    if not intent or intent.get("shell_pid") != args.shell_pid:
        return 0
    print(
        "\t".join(
            [
                intent["thread_key"],
                intent["session_id"],
                intent["created_at"],
            ]
        )
    )
    remove_attach_intent(workspace)
    return 0


def command_record_exit_diagnostic(args: argparse.Namespace) -> int:
    workspace = workspace_root(args.workspace)
    ensure_surface(workspace)
    payload = {
        "shell_pid": args.shell_pid,
        "exit_code": args.exit_code,
        "owner_thread_key": args.owner_thread_key,
        "attach_intent_present": args.attach_intent_present,
        "shell_ppid": args.shell_ppid,
        "shell_pgid": args.shell_pgid,
        "tty": args.tty,
        "child_pid": args.child_pid,
        "child_pgid": args.child_pgid,
        "child_command": args.child_command,
    }
    append_event(workspace, "shell_exit_diagnostic", "cli", payload)
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
    next_status = apply_event(workspace, current, "turn_completed", payload)
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
    event_parser.add_argument("--owner-thread-key")
    event_parser.set_defaults(func=command_event)

    notify_parser = subparsers.add_parser("notify")
    notify_parser.add_argument("--workspace")
    notify_parser.add_argument("payload", nargs="?")
    notify_parser.add_argument("extra", nargs="*")
    notify_parser.set_defaults(func=command_notify)

    launch_parser = subparsers.add_parser("prepare-launch")
    launch_parser.add_argument("--workspace")
    launch_parser.add_argument("--data-root", required=True)
    launch_parser.add_argument("--shell-pid", type=int, required=True)
    launch_parser.add_argument("--thread-key")
    launch_parser.set_defaults(func=command_prepare_launch)

    consume_intent_parser = subparsers.add_parser("consume-attach-intent")
    consume_intent_parser.add_argument("--workspace")
    consume_intent_parser.add_argument("--shell-pid", type=int, required=True)
    consume_intent_parser.set_defaults(func=command_consume_attach_intent)

    child_parser = subparsers.add_parser("record-child-process")
    child_parser.add_argument("--workspace")
    child_parser.add_argument("--shell-pid", type=int, required=True)
    child_parser.add_argument("--child-pid", type=int, required=True)
    child_parser.add_argument("--child-pgid", type=int, required=True)
    child_parser.add_argument("--child-command", required=True)
    child_parser.set_defaults(func=command_record_child_process)

    exit_diag_parser = subparsers.add_parser("record-exit-diagnostic")
    exit_diag_parser.add_argument("--workspace")
    exit_diag_parser.add_argument("--shell-pid", type=int, required=True)
    exit_diag_parser.add_argument("--exit-code", type=int, required=True)
    exit_diag_parser.add_argument("--owner-thread-key")
    exit_diag_parser.add_argument("--shell-ppid")
    exit_diag_parser.add_argument("--shell-pgid")
    exit_diag_parser.add_argument("--tty")
    exit_diag_parser.add_argument("--child-pid")
    exit_diag_parser.add_argument("--child-pgid")
    exit_diag_parser.add_argument("--child-command")
    exit_diag_parser.add_argument("--attach-intent-present", action="store_true")
    exit_diag_parser.set_defaults(func=command_record_exit_diagnostic)

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
