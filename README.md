# threadBridge

Telegram bot that maps Telegram threads to Codex app-server threads bound to real local workspaces.

## What It Does

- Uses Telegram as the UI layer for thread-based interaction.
- Stores bot-local thread metadata under `data/`.
- Binds each Telegram thread to a real workspace path with `/bind_workspace <absolute-path>`.
- Starts workspace-scoped shared Codex app-server daemons on loopback websocket and connects the bot over JSON-RPC.
- Installs a managed runtime appendix and `.threadbridge/` wrapper surface into the bound workspace.
- Exposes a managed `hcodex` launcher that resumes the bound Codex thread through `codex --remote <ws-url>`.

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
scripts/local_threadbridge.sh restart --codex-source brew
scripts/local_threadbridge.sh restart --codex-source alpha
```

`--codex-source brew|alpha` controls which local `codex` binary `hcodex` should prefer. The choice is persisted in `.threadbridge/codex/source.txt` and is picked up the next time a workspace runtime is bootstrapped.

## Behavior

- Main private chat acts as the control console.
- Only Telegram user IDs listed in `AUTHORIZED_TELEGRAM_USER_IDS` can trigger the bot.
- `/new_thread` creates a Telegram topic and bot-local metadata only.
- `/bind_workspace <absolute-path>` installs the runtime appendix into the target workspace and starts a fresh Codex thread for it.
- Normal thread messages resume the saved `current_codex_thread_id` instead of creating a new one.
- `/new` starts a fresh Codex thread for the already bound workspace.
- `/reconnect_codex` verifies that the saved Codex thread still matches the stored workspace path.
- Topic titles currently reflect `busy` and `broken` state.
- `hcodex` is the managed local TUI path. It resolves the workspace daemon from `.threadbridge/state/app-server/current.json` and launches `codex --remote ...`.

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
  - `.threadbridge/state/app-server/current.json`
  - `.threadbridge/state/codex-sync/current.json`
  - `.threadbridge/state/codex-sync/events.jsonl`
  - `.threadbridge/tool_requests/`
  - `.threadbridge/tool_results/`
  - `.codex/hooks.json`

## Local TUI Path

- After `/bind_workspace`, source `./.threadbridge/shell/codex-sync.bash` inside that workspace and use `hcodex`.
- With no extra args, `hcodex` resumes the thread's current Codex session through the shared workspace daemon.
- The shell snippet still installs legacy `codex-sync` hooks and state files for compatibility while the shared-runtime migration is in progress, but `/attach_cli_session` and viewer handoff are no longer part of the supported command surface.
- Raw `codex` launches that bypass `hcodex` are not part of the managed local path.

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
