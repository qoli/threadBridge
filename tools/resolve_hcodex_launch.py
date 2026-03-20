#!/usr/bin/env python3

import argparse
import json
import sys
from pathlib import Path


def canonical(path: Path) -> Path:
    try:
        return path.resolve()
    except OSError:
        return path


def read_json(path: Path):
    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def current_codex_thread_id(binding: dict) -> str | None:
    return (
        binding.get("current_codex_thread_id")
        or binding.get("selected_session_id")
        or binding.get("codex_thread_id")
    )


def iter_bound_threads(data_root: Path, workspace: Path):
    for entry in sorted(data_root.iterdir()):
        if not entry.is_dir():
            continue
        metadata_path = entry / "metadata.json"
        binding_path = entry / "session-binding.json"
        if not metadata_path.exists() or not binding_path.exists():
            continue
        metadata = read_json(metadata_path)
        if metadata.get("scope") != "thread" or metadata.get("status") != "active":
            continue
        binding = read_json(binding_path)
        bound_workspace = binding.get("workspace_cwd")
        if not bound_workspace:
            continue
        if canonical(Path(bound_workspace)) != workspace:
            continue
        yield {
            "thread_key": metadata.get("thread_key"),
            "current_codex_thread_id": current_codex_thread_id(binding),
        }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--data-root", required=True)
    parser.add_argument("--workspace", required=True)
    parser.add_argument("--thread-key")
    args = parser.parse_args()

    workspace = canonical(Path(args.workspace))
    data_root = canonical(Path(args.data_root))
    state_path = workspace / ".threadbridge" / "state" / "app-server" / "current.json"
    if not state_path.exists():
        print(
            f"hcodex: missing shared app-server state at {state_path}",
            file=sys.stderr,
        )
        return 2

    state = read_json(state_path)
    daemon_ws_url = state.get("daemon_ws_url")
    tui_proxy_base_ws_url = state.get("tui_proxy_base_ws_url")
    if not daemon_ws_url:
        print("hcodex: app-server state is missing daemon_ws_url", file=sys.stderr)
        return 2

    matches = list(iter_bound_threads(data_root, workspace))
    if args.thread_key:
        matches = [item for item in matches if item["thread_key"] == args.thread_key]
        if not matches:
            print(
                f"hcodex: no active Telegram thread binding found for --thread-key {args.thread_key}",
                file=sys.stderr,
            )
            return 2
    elif len(matches) > 1:
        print(
            "hcodex: multiple active Telegram thread bindings use this workspace; pass --thread-key",
            file=sys.stderr,
        )
        return 2

    if not matches:
        print(
            "hcodex: no active Telegram thread binding found for this workspace",
            file=sys.stderr,
        )
        return 2

    current_thread = matches[0]["current_codex_thread_id"]
    if not current_thread:
        print(
            "hcodex: bound Telegram thread is missing current_codex_thread_id",
            file=sys.stderr,
        )
        return 2

    launch_ws_url = daemon_ws_url
    if tui_proxy_base_ws_url:
        launch_ws_url = f"{tui_proxy_base_ws_url.rstrip('/')}/thread/{matches[0]['thread_key']}"

    print(
        "\t".join(
            [
                launch_ws_url,
                matches[0]["thread_key"] or "",
                current_thread,
            ]
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
