#!/usr/bin/env python3
import argparse
import json
import shutil
import subprocess
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Optional
from uuid import uuid4


PROMPTS = [
    "笛卡尔指南讲的大概内容，帮我简短撰写一下和艺术相关的摘要和篇章给我",
    "卢梭的内容，也按照以上结果帮我解读和摘取一些摘要",
    "我們產生了多少條對話呢？",
]


@dataclass
class TurnResult:
    index: int
    prompt: str
    command: list[str]
    thread_id: str
    final_response: str
    jsonl_path: Path
    stderr_path: Path


def fail(message: str) -> None:
    raise SystemExit(message)


def ensure_workspace_runtime(workspace_path: Path, template_path: Path) -> None:
    workspace_path.mkdir(parents=True, exist_ok=True)
    runtime_dir = workspace_path / ".threadbridge"
    (runtime_dir / "tool_requests").mkdir(parents=True, exist_ok=True)
    (runtime_dir / "bin").mkdir(parents=True, exist_ok=True)
    shutil.copyfile(template_path, workspace_path / "AGENTS.md")

    for name in ("build_prompt_config", "generate_image"):
        wrapper = runtime_dir / "bin" / name
        wrapper.write_text(f"#!/bin/sh\necho \"stub {name}\"\n", encoding="utf-8")
        wrapper.chmod(0o755)


def parse_jsonl(raw_stdout: str) -> list[dict]:
    parsed: list[dict] = []
    for raw_line in raw_stdout.splitlines():
        line = raw_line.strip()
        if not line:
            continue
        try:
            parsed.append(json.loads(line))
        except json.JSONDecodeError:
            parsed.append({"type": "non_json_line", "raw": line})
    return parsed


def extract_thread_id(events: list[dict]) -> str:
    ids = [event.get("thread_id", "") for event in events if event.get("type") == "thread.started"]
    if not ids:
        fail("No thread.started event was emitted.")
    return str(ids[-1])


def extract_final_response(events: list[dict]) -> str:
    texts = []
    for event in events:
        if event.get("type") != "item.completed":
            continue
        item = event.get("item")
        if not isinstance(item, dict) or item.get("type") != "agent_message":
            continue
        text = item.get("text")
        if isinstance(text, str):
            texts.append(text)
    return texts[-1] if texts else ""


def build_fresh_args(workspace_path: Path, prompt: str, profile: str) -> list[str]:
    if profile == "manual-log":
        return [
            "codex",
            "exec",
            "--json",
            "--skip-git-repo-check",
            "--sandbox",
            "workspace-write",
            "--config",
            'approval_policy="never"',
            "--config",
            "sandbox_workspace_write.network_access=true",
            "--config",
            'web_search="live"',
            prompt,
        ]
    return [
        "codex",
        "exec",
        "--json",
        "--skip-git-repo-check",
        "--sandbox",
        "workspace-write",
        "--cd",
        str(workspace_path),
        prompt,
    ]


def build_resume_args(thread_id: str, prompt: str) -> list[str]:
    return [
        "codex",
        "exec",
        "resume",
        "--json",
        "--skip-git-repo-check",
        thread_id,
        prompt,
    ]


def run_turn(
    *,
    index: int,
    prompt: str,
    cwd: Path,
    command: list[str],
    jsonl_path: Path,
    stderr_path: Path,
    timeout_seconds: int,
) -> TurnResult:
    try:
        completed = subprocess.run(
            command,
            cwd=str(cwd),
            capture_output=True,
            text=True,
            timeout=timeout_seconds,
            check=False,
        )
    except subprocess.TimeoutExpired as exc:
        stdout = exc.stdout or ""
        stderr = exc.stderr or ""
        jsonl_path.write_text(stdout, encoding="utf-8")
        stderr_path.write_text(stderr, encoding="utf-8")
        fail(f"Turn {index} timed out after {timeout_seconds}s.")

    jsonl_path.write_text(completed.stdout, encoding="utf-8")
    stderr_path.write_text(completed.stderr, encoding="utf-8")

    if completed.returncode != 0:
        fail(f"Turn {index} failed with exit code {completed.returncode}. See {stderr_path}")

    events = parse_jsonl(completed.stdout)
    return TurnResult(
        index=index,
        prompt=prompt,
        command=command,
        thread_id=extract_thread_id(events),
        final_response=extract_final_response(events),
        jsonl_path=jsonl_path,
        stderr_path=stderr_path,
    )


def main() -> None:
    parser = argparse.ArgumentParser(description="Run a minimal 3-turn Codex CLI continuity probe.")
    parser.add_argument(
        "--repo-root",
        default=str(Path(__file__).resolve().parents[1]),
        help="Path to the threadBridge repository root.",
    )
    parser.add_argument(
        "--output-root",
        default="data/min-chat-probes",
        help="Directory, relative to repo root unless absolute, where probe output folders are created.",
    )
    parser.add_argument(
        "--timeout-seconds",
        type=int,
        default=180,
        help="Per-turn timeout in seconds.",
    )
    parser.add_argument(
        "--profile",
        choices=["current-bot", "manual-log"],
        default="current-bot",
        help="CLI argument profile to reproduce.",
    )
    args = parser.parse_args()

    repo_root = Path(args.repo_root).resolve()
    output_root = Path(args.output_root)
    if not output_root.is_absolute():
        output_root = (repo_root / output_root).resolve()
    output_root.mkdir(parents=True, exist_ok=True)

    probe_id = f"min-chat-{datetime.now().strftime('%Y%m%d%H%M%S')}-{uuid4().hex[:8]}"
    probe_root = output_root / probe_id
    workspace_path = probe_root / "workspace"
    probe_root.mkdir(parents=True, exist_ok=True)

    template_path = repo_root / "templates" / "AGENTS.md"
    if not template_path.exists():
        fail(f"Missing template AGENTS.md: {template_path}")
    ensure_workspace_runtime(workspace_path, template_path)

    turns: list[TurnResult] = []
    existing_thread_id: Optional[str] = None

    for index, prompt in enumerate(PROMPTS, start=1):
        if existing_thread_id is None:
            command = build_fresh_args(workspace_path, prompt, args.profile)
        else:
            command = build_resume_args(existing_thread_id, prompt)
        turn = run_turn(
            index=index,
            prompt=prompt,
            cwd=workspace_path,
            command=command,
            jsonl_path=probe_root / f"turn{index}.jsonl",
            stderr_path=probe_root / f"turn{index}.stderr.txt",
            timeout_seconds=args.timeout_seconds,
        )
        turns.append(turn)
        existing_thread_id = turn.thread_id

    report = {
        "probeId": probe_id,
        "workspacePath": str(workspace_path),
        "profile": args.profile,
        "turns": [
            {
                "index": turn.index,
                "prompt": turn.prompt,
                "threadId": turn.thread_id,
                "threadIdChangedFromPrevious": False if i == 0 else turn.thread_id != turns[i - 1].thread_id,
                "finalResponse": turn.final_response,
                "command": turn.command,
                "jsonlPath": str(turn.jsonl_path),
                "stderrPath": str(turn.stderr_path),
            }
            for i, turn in enumerate(turns)
        ],
        "summary": {
            "allThreadIds": [turn.thread_id for turn in turns],
            "threadIdStableAcrossAllTurns": len({turn.thread_id for turn in turns}) == 1,
        },
    }

    report_path = probe_root / "report.json"
    report_path.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
