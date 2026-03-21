# threadBridge

Telegram bot that maps Telegram threads to Codex app-server threads bound to real local workspaces.

## What It Does

- Uses Telegram as the UI layer for thread-based interaction.
- Stores bot-local thread metadata under `data/`.
- Binds each Telegram thread to a real workspace path with `/bind_workspace <absolute-path>`.
- Starts workspace-scoped shared Codex app-server daemons on loopback websocket and connects the bot over JSON-RPC.
- Installs a managed runtime appendix and `.threadbridge/` wrapper surface into the bound workspace.
- Exposes a managed `hcodex` launcher for the bound workspace through `codex --remote <ws-url>`.
- Starts a local management API for setup, runtime health, active thread views, and workspace views on loopback HTTP.
- On macOS, also has a desktop runtime entrypoint with a tray menu and embedded settings webview.

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

Or start the macOS desktop runtime:

```bash
export CARGO_HOME="$PWD/.cargo" CARGO_TARGET_DIR="$PWD/target"
cargo run --bin threadbridge_desktop
```

Or use the local helper script:

```bash
scripts/local_threadbridge.sh start
scripts/local_threadbridge.sh restart --codex-source brew
scripts/local_threadbridge.sh restart --codex-source source
```

`--codex-source brew|source` controls which local `codex` binary `hcodex` should prefer. The choice is persisted in `.threadbridge/codex/source.txt` and is picked up the next time a workspace runtime is bootstrapped.

- `brew`: prefer the system `codex` on `PATH`, with the managed copy as fallback.
- `source`: build `codex-cli` from the local Codex source tree and cache it under `.threadbridge/codex/codex`, then prefer that managed copy.

## Behavior

- Main private chat acts as the control console.
- If Telegram credentials are missing, threadBridge still starts the local management API but does not start Telegram polling.
- `threadbridge_desktop` also starts without Telegram credentials; it keeps the tray and local settings UI available for onboarding.
- In the desktop runtime, saving Telegram setup through the local management UI will trigger a background retry to start polling; a full process restart is no longer the only path.
- Only Telegram user IDs listed in `AUTHORIZED_TELEGRAM_USER_IDS` can trigger the bot.
- `/new_thread` creates a Telegram topic and bot-local metadata only.
- `/bind_workspace <absolute-path>` installs the runtime appendix into the target workspace and starts a fresh Codex thread for it.
- Normal thread messages resume the saved `current_codex_thread_id` instead of creating a new one.
- `/new` starts a fresh Codex thread for the already bound workspace.
- `/reconnect_codex` verifies that the saved Codex thread still matches the stored workspace path.
- Topic titles currently reflect `busy` and `broken` state.
- `hcodex` is the managed local TUI path. It resolves the workspace daemon from `.threadbridge/state/app-server/current.json` and launches `codex --remote ...`.
- The local management API defaults to `http://127.0.0.1:38420` and can be changed with `THREADBRIDGE_MANAGEMENT_BIND_ADDR`.
- On macOS, the tray menu lists one submenu per managed workspace, `Start New hcodex Session`, and the recent 5 session IDs for resume.
- The local management UI can open a managed workspace in Finder, repair a workspace runtime, refresh the managed Codex cache from the current `codex` on `PATH`, and build a managed source Codex binary from the local Codex Rust workspace.
- The desktop runtime owner now proactively ensures both the shared app-server daemon and the workspace TUI proxy for managed workspaces.
- The management surface now shows TUI adoption-pending state per thread and workspace, so local handoff is visible without reading raw state files.
- The local management UI can now explicitly adopt or reject a pending TUI session handoff instead of waiting for Telegram callback controls or implicit auto-adopt.
- The local server now serves the management UI from a checked-in static asset instead of embedding the entire page as an inline Rust string.

## Runtime Layout

- `data/main-thread/` stores the control-console state.
- `data/<thread-key>/` stores bot-local metadata, transcripts, session binding, and image-state artifacts.
- The real workspace is not mirrored or symlinked under `data/`.
- threadBridge installs the following runtime surface into a bound workspace:
  - `AGENTS.md` managed block markers
  - `.threadbridge/bin/build_prompt_config`
  - `.threadbridge/bin/generate_image`
  - `.threadbridge/bin/hcodex`
  - `.threadbridge/bin/send_telegram_media`
  - `.threadbridge/state/app-server/current.json`
  - `.threadbridge/state/shared-runtime/current.json`
  - `.threadbridge/state/shared-runtime/events.jsonl`
  - `.threadbridge/tool_requests/`
  - `.threadbridge/tool_results/`

## Local TUI Path

- After `/bind_workspace`, run `./.threadbridge/bin/hcodex` inside that workspace.
- With no extra args, `hcodex` starts a fresh local TUI session through the shared workspace daemon.
- Use `hcodex resume <session-id>` when you explicitly want to continue an existing Codex session.
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
