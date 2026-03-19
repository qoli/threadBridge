# threadBridge

Telegram bot that maps Telegram threads to Codex app-server threads bound to real local workspaces.

## What It Does

- Uses Telegram as the UI layer for thread-based interaction.
- Stores bot-local thread metadata under `data/`.
- Binds each Telegram thread to a real workspace path with `/bind_workspace <absolute-path>`.
- Starts and resumes Codex threads through `codex app-server --listen stdio://`.
- Installs a managed runtime appendix and `.threadbridge/` wrapper surface into the bound workspace.
- Can mirror local Codex CLI activity back into Telegram thread status through workspace-local Bash and Codex hooks.

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
- `/new` starts a fresh Codex thread for the already bound workspace.
- `/reconnect_codex` verifies that the saved Codex thread still matches the stored workspace path.
- When the shared workspace status shows local CLI activity, Telegram blocks new turns and reflects the status in the topic title.

## Runtime Layout

- `data/main-thread/` stores the control-console state.
- `data/<thread-key>/` stores bot-local metadata, transcripts, session binding, and image-state artifacts.
- The real workspace is not mirrored or symlinked under `data/`.
- threadBridge installs the following runtime surface into a bound workspace:
  - `AGENTS.md` managed block markers
  - `.threadbridge/bin/build_prompt_config`
  - `.threadbridge/bin/codex_sync_event`
  - `.threadbridge/bin/codex_sync_notify`
  - `.threadbridge/bin/generate_image`
  - `.threadbridge/bin/send_telegram_media`
  - `.threadbridge/shell/codex-sync.bash`
  - `.threadbridge/state/codex-sync/current.json`
  - `.threadbridge/state/codex-sync/events.jsonl`
  - `.threadbridge/tool_requests/`
  - `.threadbridge/tool_results/`
  - `.codex/hooks.json`

## Local Codex CLI Sync

- After `/bind_workspace`, source `./.threadbridge/shell/codex-sync.bash` inside that workspace and use `hcodex` if you want managed local CLI / Telegram sync.
- `· cli` means `hcodex` is live and Telegram is only the viewer.
- `· attach` means Telegram is live and the local terminal has been handed off to `threadbridge_viewer`.
- The generated shell wrapper injects `features.codex_hooks=true` and a workspace-local `notify` override, then writes CLI lifecycle state into `.threadbridge/state/codex-sync/`.
- This v1 sync path is Bash-only. Raw `codex` launches that bypass the sourced wrapper are not guaranteed to update Telegram status.
- Manual test flow for `.cli` / `.attach` session behavior: [docs/session-sync-manual-test.md](/Volumes/Data/Github/threadBridge/docs/session-sync-manual-test.md)

## Commands

- `/start`
- `/new_thread`
- `/new`
- `/bind_workspace`
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
