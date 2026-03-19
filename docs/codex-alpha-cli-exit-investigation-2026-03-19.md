# Codex Alpha CLI Exit Investigation

Date: `2026-03-19`

Related upstream issue:
- [openai/codex#15153](https://github.com/openai/codex/issues/15153)

## Summary

`threadBridge` currently supports two local Codex CLI sources for `hcodex`:

- `brew`
- `alpha` (`0.116.0-alpha.11`)

The current finding is:

- `brew` is the only reliable runtime surface for `hcodex`
- `alpha 0.116.0-alpha.11` can exit unexpectedly in managed `hcodex` scenarios

This document records what was observed, what was ruled out, and what still remains unknown.

## Environment

Workspace:
- `/Volumes/Data/Github/codex`

threadBridge repo:
- `/Volumes/Data/Github/threadBridge`

Relevant Codex versions:
- `brew`: `codex-cli 0.115.0`
- `alpha`: `codex-cli 0.116.0-alpha.11`

Current release mapping:
- `0.116.0-alpha.11` corresponds to tag `rust-v0.116.0-alpha.11`
- local `codex` repo `HEAD` is only 2 commits ahead of that tag
- no later commit obviously mentions fixes for signal / exit / hook / attach / child-process handling

## Managed Lifecycle Model

In the managed local CLI path:

1. source `./.threadbridge/shell/codex-sync.bash`
2. start local CLI via `hcodex`
3. `hcodex` installs owner claim / shell lifecycle / hook / notify surface
4. Telegram thread reflects local state with `.cli`
5. `/attach_cli_session` terminates local `codex` and hands the terminal off to `threadbridge_viewer`

Important boundary:

- When Telegram text is rejected by the `.cli` ownership gate, the bot should only reject and return.
- That rejected Telegram text is not supposed to become input to local `hcodex`.

## Confirmed Findings

### 1. Gate-rejected Telegram text does not start a bot-side Codex turn

In the `.cli` ownership gate path, the Rust bot:

- logs the user message
- logs a system rejection
- sends:
  - `Local Codex CLI currently owns this session. Run /attach_cli_session to take it over in Telegram.`
- returns early

The bot does not start a new Codex turn in this path.

Operationally this means:

- Telegram input is consumed by the bot only as a rejection event
- the input is not intentionally forwarded into local Codex execution

### 2. `UserPromptSubmit` is not the root cause

We temporarily removed `UserPromptSubmit` from `.codex/hooks.json` and reproduced the failure again.

So the alpha exit bug is not caused by:

- `UserPromptSubmit`
- CLI prompt mirror handling

### 3. Raw alpha binary can be terminated normally

Running the alpha binary directly:

```bash
cd /Volumes/Data/Github/codex
./.threadbridge/bin/codex
```

and sending `TERM` to the live child process cleanly exits the process while leaving the shell alive.

This means:

- alpha is not fundamentally unkillable
- the bug is not simply “alpha cannot be terminated for handoff”

### 4. `/attach_cli_session` itself is now able to hand off on alpha

With additional wrapper diagnostics enabled, a later run showed the following sequence:

- `telegram.attach.kill_cli_session`
- `wrapper_handoff_stage = before-consume`
- `wrapper_handoff_stage = after-consume` with `attach_payload_present = true`
- `wrapper_handoff_stage = before-viewer`

That means the attach chain reached viewer startup successfully in at least one alpha run.

So the attach mechanism itself is not universally broken on alpha.

### 5. The remaining failure is narrower

The still-reproducible failure is:

- local alpha CLI is live
- Telegram sends ordinary text during `.cli`
- bot rejects it through the ownership gate
- local alpha Codex later exits with `137`

The same scenario does not reproduce with `brew`.

### 6. The final exit shape is always `137`

The recorded `shell_exit_diagnostic` shows:

- `exit_code = 137`
- child command is the managed alpha CLI command

`137` means:

- process ended in a `SIGKILL` shape

What is still unknown:

- whether the `SIGKILL` comes from an external process
- or from some internal runtime/control-path behavior that ultimately results in a forced kill

## Ruled-Out Explanations

These have been investigated and are currently ruled out as the primary cause:

- Rust bot forwarding gate-rejected Telegram text into a new Codex turn
- `/attach_cli_session` being the only failure source
- `UserPromptSubmit`
- inability to terminate the alpha binary at all
- brew local CLI lifecycle implementation being broken in the same way

## Why Brew Still Matters

The same managed lifecycle works under `brew`:

- `.cli` ownership gating works
- `/attach_cli_session` can terminate local `codex`
- viewer handoff works
- normal Telegram-to-bot continuation after attach works

This makes the current working assumption:

- threadBridge lifecycle design is not the main failure
- the unstable part is specific to the alpha CLI runtime under managed integration

## Current Best Hypothesis

The bug appears to be in the interaction between:

- alpha `codex-cli 0.116.0-alpha.11`
- managed `hcodex` wrapper lifecycle
- hooks / notify / owner/session coordination
- external session-state changes triggered by Telegram presence

The strongest practical conclusion today is:

- `brew` should remain the default `hcodex` source
- `alpha` should be treated as experimental only

## Current Recommendation

Use:

```bash
scripts/local_threadbridge.sh restart --codex-source brew
```

And only use:

```bash
scripts/local_threadbridge.sh restart --codex-source alpha
```

for targeted experiments or upstream debugging.

## Open Questions

These are still unresolved:

- Why does gate-rejected Telegram text correlate with alpha CLI exiting?
- What exact actor produces the final `SIGKILL` shape?
- Why can alpha successfully hand off in some attach runs, but still later exit after `.cli` gate rejection?
- Is the problem in alpha TUI runtime, hook/notify integration, or some parent/child process control path?
