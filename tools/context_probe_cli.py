#!/usr/bin/env python3
import argparse
import json
import shlex
import shutil
import subprocess
import sys
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from uuid import uuid4


STEP_ONE_PROMPT = "笛卡尔指南讲的大概内容，帮我简短撰写一下和艺术相关的摘要和篇章给我"
STEP_TWO_PROMPT = "卢梭的内容，也按照以上结果帮我解读和摘取一些摘要"
STEP_THREE_PROMPT = "\n".join(
    [
        "Use this workspace runtime to build the next prompt config artifacts.",
        "Read and follow the workspace AGENTS.md, including the threadBridge managed appendix.",
        "Use the local wrapper command ./.threadbridge/bin/build_prompt_config for any file materialization work.",
        "Base all semantic decisions on the current Codex session context.",
        "If the session still lacks enough information, ask follow-up questions in this thread and do not run the tool.",
    ]
)


@dataclass
class StepResult:
    command: list[str]
    final_response: str
    jsonl_path: Path
    name: str
    prompt: str
    return_code: int
    stderr_path: Path
    stdout_lines: list[dict]
    thread_id: str


def fail(message: str) -> None:
    raise SystemExit(message)


def ensure_workspace_runtime(workspace_path: Path, template_path: Path) -> None:
    workspace_path.mkdir(parents=True, exist_ok=True)
    runtime_dir = workspace_path / ".threadbridge"
    (runtime_dir / "tool_requests").mkdir(parents=True, exist_ok=True)
    (runtime_dir / "bin").mkdir(parents=True, exist_ok=True)
    shutil.copyfile(template_path, workspace_path / "AGENTS.md")

    build_wrapper = runtime_dir / "bin" / "build_prompt_config"
    build_wrapper.write_text("#!/bin/sh\necho \"stub build_prompt_config\"\n", encoding="utf-8")
    build_wrapper.chmod(0o755)

    image_wrapper = runtime_dir / "bin" / "generate_image"
    image_wrapper.write_text("#!/bin/sh\necho \"stub generate_image\"\n", encoding="utf-8")
    image_wrapper.chmod(0o755)


def parse_jsonl_lines(raw_stdout: str) -> list[dict]:
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
    thread_ids = [
        event.get("thread_id", "")
        for event in events
        if isinstance(event, dict) and event.get("type") == "thread.started"
    ]
    if not thread_ids:
        fail("Probe run did not emit a thread.started event.")
    return str(thread_ids[-1])


def extract_final_response(events: list[dict]) -> str:
    responses: list[str] = []
    for event in events:
        if not isinstance(event, dict) or event.get("type") != "item.completed":
            continue
        item = event.get("item")
        if not isinstance(item, dict):
            continue
        if item.get("type") != "agent_message":
            continue
        text = item.get("text")
        if isinstance(text, str):
            responses.append(text)
    return responses[-1] if responses else ""


def shell_command(args: list[str]) -> str:
    return " ".join(shlex.quote(part) for part in args)


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


def build_resume_args(thread_id: str, prompt: str, profile: str) -> list[str]:
    if profile == "manual-log":
        return [
            "codex",
            "exec",
            "resume",
            "--json",
            "--skip-git-repo-check",
            thread_id,
            prompt,
        ]

    return [
        "codex",
        "exec",
        "resume",
        "--json",
        "--skip-git-repo-check",
        thread_id,
        prompt,
    ]


def run_step(
    *,
    args: list[str],
    cwd: Path,
    jsonl_path: Path,
    name: str,
    prompt: str,
    stderr_path: Path,
    timeout_seconds: int,
) -> StepResult:
    try:
        completed = subprocess.run(
            args,
            cwd=str(cwd),
            capture_output=True,
            text=True,
            check=False,
            timeout=timeout_seconds,
        )
    except subprocess.TimeoutExpired as exc:
        stdout = exc.stdout or ""
        stderr = exc.stderr or ""
        jsonl_path.write_text(stdout, encoding="utf-8")
        stderr_path.write_text(stderr, encoding="utf-8")
        fail(
            f"{name} timed out after {timeout_seconds}s.\n"
            f"See {stderr_path} for stderr and {jsonl_path} for stdout."
        )
    jsonl_path.write_text(completed.stdout, encoding="utf-8")
    stderr_path.write_text(completed.stderr, encoding="utf-8")

    if completed.returncode != 0:
        fail(
            f"{name} failed with exit code {completed.returncode}.\n"
            f"See {stderr_path} for stderr and {jsonl_path} for stdout."
        )

    events = parse_jsonl_lines(completed.stdout)
    return StepResult(
        command=args,
        final_response=extract_final_response(events),
        jsonl_path=jsonl_path,
        name=name,
        prompt=prompt,
        return_code=completed.returncode,
        stderr_path=stderr_path,
        stdout_lines=events,
        thread_id=extract_thread_id(events),
    )


def write_report(report_path: Path, report: dict) -> None:
    report_path.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Run a minimal 3-step Codex CLI context probe without Telegram or the SDK.",
    )
    parser.add_argument(
        "--repo-root",
        default=str(Path(__file__).resolve().parents[1]),
        help="Path to the threadBridge repository root.",
    )
    parser.add_argument(
        "--output-root",
        default="data/context-probes-cli",
        help="Directory, relative to repo root unless absolute, where probe output folders are created.",
    )
    parser.add_argument(
        "--timeout-seconds",
        type=int,
        default=180,
        help="Per-step timeout in seconds for each codex exec invocation.",
    )
    parser.add_argument(
        "--profile",
        choices=["manual-log", "current-bot"],
        default="manual-log",
        help="CLI argument profile to reproduce. 'manual-log' matches the successful hand-run log.",
    )
    args = parser.parse_args()

    repo_root = Path(args.repo_root).resolve()
    if not repo_root.exists():
        fail(f"Repo root does not exist: {repo_root}")

    output_root = Path(args.output_root)
    if not output_root.is_absolute():
        output_root = (repo_root / output_root).resolve()
    output_root.mkdir(parents=True, exist_ok=True)

    probe_id = f"context-cli-{datetime.now().strftime('%Y%m%d%H%M%S')}-{uuid4().hex[:8]}"
    probe_root = output_root / probe_id
    workspace_path = probe_root / "workspace"
    probe_root.mkdir(parents=True, exist_ok=True)

    template_path = repo_root / "templates" / "AGENTS.md"
    if not template_path.exists():
        fail(f"Missing template AGENTS.md: {template_path}")
    ensure_workspace_runtime(workspace_path, template_path)

    step1 = run_step(
        args=build_fresh_args(workspace_path, STEP_ONE_PROMPT, args.profile),
        cwd=workspace_path,
        jsonl_path=probe_root / "step1.jsonl",
        name="step1",
        prompt=STEP_ONE_PROMPT,
        stderr_path=probe_root / "step1.stderr.txt",
        timeout_seconds=args.timeout_seconds,
    )
    step2 = run_step(
        args=build_resume_args(step1.thread_id, STEP_TWO_PROMPT, args.profile),
        cwd=workspace_path,
        jsonl_path=probe_root / "step2.jsonl",
        name="step2",
        prompt=STEP_TWO_PROMPT,
        stderr_path=probe_root / "step2.stderr.txt",
        timeout_seconds=args.timeout_seconds,
    )
    step3 = run_step(
        args=build_resume_args(step2.thread_id, STEP_THREE_PROMPT, args.profile),
        cwd=workspace_path,
        jsonl_path=probe_root / "step3.jsonl",
        name="step3",
        prompt=STEP_THREE_PROMPT,
        stderr_path=probe_root / "step3.stderr.txt",
        timeout_seconds=args.timeout_seconds,
    )

    report = {
        "probeId": probe_id,
        "probeRoot": str(probe_root),
        "workspacePath": str(workspace_path),
        "profile": args.profile,
        "steps": [
            {
                "name": step1.name,
                "command": step1.command,
                "shellCommand": shell_command(step1.command),
                "prompt": step1.prompt,
                "threadId": step1.thread_id,
                "finalResponse": step1.final_response,
                "jsonlPath": str(step1.jsonl_path),
                "stderrPath": str(step1.stderr_path),
            },
            {
                "name": step2.name,
                "command": step2.command,
                "shellCommand": shell_command(step2.command),
                "prompt": step2.prompt,
                "threadId": step2.thread_id,
                "finalResponse": step2.final_response,
                "jsonlPath": str(step2.jsonl_path),
                "stderrPath": str(step2.stderr_path),
            },
            {
                "name": step3.name,
                "command": step3.command,
                "shellCommand": shell_command(step3.command),
                "prompt": step3.prompt,
                "threadId": step3.thread_id,
                "finalResponse": step3.final_response,
                "jsonlPath": str(step3.jsonl_path),
                "stderrPath": str(step3.stderr_path),
            },
        ],
        "verdict": {
            "step2ReportedMissingPriorResult": "看不到你说的“以上结果”" in step2.final_response
            or "结果样例" in step2.final_response,
            "step2ReusedThreadIdAsInput": True,
            "step3ReportedInsufficientContext": "does not contain enough stable information" in step3.final_response
            or "信息还不足" in step3.final_response
            or "still lacks enough information" in step3.final_response,
        },
    }
    write_report(probe_root / "report.json", report)

    print(json.dumps(report, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("Interrupted.", file=sys.stderr)
        raise SystemExit(130)
