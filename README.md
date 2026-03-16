# threadBridge

Telegram bot that binds Telegram threads to existing Codex sessions.

## What It Does

- Uses Telegram as the UI layer for thread-based interaction.
- Treats a Telegram thread as a bot-managed binding to one existing Codex session.
- Reads session metadata from the local `~/.codex` home.
- Projects the bound session `cwd` into `data/<thread-key>/workspace` as a symlink.
- Runs Codex and workspace-local wrapper tools inside that linked workspace.

## Requirements

- Rust toolchain
- Python 3
- `codex` CLI installed and already authenticated on the machine
- A Telegram bot token from BotFather
- Telegram topics enabled if you want private-thread workflows

## Setup

1. Copy `.env.example` to `.env.local`.
2. Fill in your own Telegram token, authorized user IDs, and image-provider settings.
3. Start the bot:

```bash
export CARGO_HOME="$PWD/.cargo" CARGO_TARGET_DIR="$PWD/target"
cargo run --bin threadbridge
```

Or use the local helper script:

```bash
scripts/local_threadbridge.sh start
```

## Behavior

- Main private chat acts as the control console.
- Only Telegram user IDs listed in `AUTHORIZED_TELEGRAM_USER_IDS` can trigger the bot.
- `/new_thread` creates a Telegram topic and bot-local metadata only.
- `/list_sessions` shows recent Codex sessions discovered from the local `~/.codex` home.
- `/bind_session <session_id>` binds the current Telegram thread to an existing Codex session and ensures `data/<thread-key>/workspace` points at that session's `cwd`.
- Normal thread messages resume the bound Codex session instead of creating a new one.
- If the bound session becomes invalid, the bot marks it broken and requires `/reconnect_codex` or a fresh `/bind_session`.

## Workspace Layout

- `data/main-thread/` stores the control-console state.
- `data/<thread-key>/` stores bot-local metadata and transcripts for one Telegram thread.
- `data/<thread-key>/workspace` is a symlink to the bound Codex session `cwd`.
- The linked workspace contains the thread runtime contract and wrappers:
  - `AGENTS.md`
  - `bin/build_prompt_config`
  - `bin/generate_image`
  - `bin/send_telegram_media`

## Commands

- `/start`
- `/new_thread`
- `/list_sessions`
- `/bind_session`
- `/generate_title`
- `/archive_thread`
- `/reconnect_codex`
- `/restore_thread`

## Development

```bash
export CARGO_HOME="$PWD/.cargo" CARGO_TARGET_DIR="$PWD/target"
cargo check
cargo test
```

## Security

- Keep secrets in `.env.local`.
- Do not commit `data/`, logs, generated images, or provider payloads.
- Use separate Telegram bot tokens for separate polling runtimes.
- Review `docs/` before publishing examples copied from live provider traffic.

## Public Repository Scope

This repository is published as a codebase and local runtime example. It does not include any production Telegram token, provider API key, remote host, or real Codex session data.
