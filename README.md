# threadBridge

Telegram bot that maps Telegram threads to Codex app-server threads bound to real local workspaces.

## What It Does

- Uses Telegram as the UI layer for thread-based interaction.
- Stores bot-local thread metadata under `data/`.
- Binds each Telegram thread to a real workspace path with `/bind_workspace <absolute-path>`.
- Starts and resumes Codex threads through `codex app-server --listen stdio://`.
- Installs a managed runtime appendix and `.threadbridge/` wrapper surface into the bound workspace.

## Requirements

- Rust toolchain
- Python 3
- `codex` CLI installed and authenticated on the machine
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
- `/bind_workspace <absolute-path>` installs the runtime appendix into the target workspace and starts a fresh Codex thread for it.
- Normal thread messages resume the saved Codex thread instead of creating a new one.
- `/reset_codex_session` starts a fresh Codex thread for the already bound workspace.
- `/reconnect_codex` verifies that the saved Codex thread still matches the stored workspace path.

## Runtime Layout

- `data/main-thread/` stores the control-console state.
- `data/<thread-key>/` stores bot-local metadata, transcripts, session binding, and image-state artifacts.
- The real workspace is not mirrored or symlinked under `data/`.
- threadBridge installs the following runtime surface into a bound workspace:
  - `AGENTS.md` managed block markers
  - `.threadbridge/bin/build_prompt_config`
  - `.threadbridge/bin/generate_image`
  - `.threadbridge/bin/send_telegram_media`
  - `.threadbridge/tool_requests/`
  - `.threadbridge/tool_results/`

## Commands

- `/start`
- `/new_thread`
- `/bind_workspace`
- `/reset_codex_session`
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
