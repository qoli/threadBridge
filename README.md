# threadBridge

Telegram bot that maps Telegram threads to Codex app-server threads bound to real local workspaces.

## What It Does

- Uses Telegram as the UI layer for thread-based interaction.
- Stores bot-local thread metadata under `data/`.
- Uses `/add_workspace <absolute-path>` from the control chat as the workspace-first thread creation flow.
- Starts workspace-scoped shared Codex app-server daemons on loopback websocket and connects the bot over JSON-RPC.
- Installs a managed runtime appendix and `.threadbridge/` wrapper surface into the bound workspace.
- Exposes a managed `hcodex` launcher for the bound workspace through `codex --remote <ws-url>`.
- Starts a local management API for setup, runtime health, active thread views, and workspace views on loopback HTTP.
- Uses a macOS desktop runtime entrypoint with a tray menu that opens the local management UI in the system default browser and can add a workspace from a native folder picker.

## Requirements

- Rust toolchain
- Python 3
- `codex` CLI installed and authenticated on the machine
- A Telegram bot token from BotFather
- Telegram topics enabled if you want private-thread workflows

## Setup

1. Copy `.env.example` to `.env.local`.
2. Fill in your own Telegram token, authorized user IDs, and image-provider settings.
3. Start the macOS desktop runtime:

```bash
export CARGO_HOME="$PWD/.cargo" CARGO_TARGET_DIR="$PWD/target"
cargo run --bin threadbridge_desktop
```

Or use the local helper script:

```bash
scripts/local_threadbridge.sh build
scripts/local_threadbridge.sh build --codex-source source
scripts/local_threadbridge.sh start
scripts/local_threadbridge.sh restart --codex-source brew
scripts/local_threadbridge.sh restart --codex-source source
```

`--codex-source brew|source` controls which local `codex` binary `hcodex` should prefer. The choice is persisted in `.threadbridge/codex/source.txt` and is picked up the next time a workspace runtime is bootstrapped.

- `brew`: prefer the system `codex` on `PATH`, with the managed copy as fallback.
- `source`: build `codex-cli` from the local Codex source tree and cache it under `.threadbridge/codex/codex`, then prefer that managed copy.
- `scripts/local_threadbridge.sh` now manages the desktop runtime only.

## Behavior

- `threadBridge` now starts through the desktop runtime only; supported local startup is macOS-only.
- Main private chat acts as the control console.
- `threadbridge_desktop` also starts without Telegram credentials; it keeps the tray and local management UI available so Telegram setup and workspace management can still happen before polling is active.
- In the desktop runtime, saving Telegram setup through the local management UI will trigger a background retry to start polling; a full process restart is no longer the only path.
- Only Telegram user IDs listed in `AUTHORIZED_TELEGRAM_USER_IDS` can trigger the bot.
- `/add_workspace <absolute-path>` creates a Telegram topic, installs the runtime appendix into the target workspace, and starts a fresh Codex session for it.
- Normal workspace-thread messages resume the saved `current_codex_thread_id` instead of creating a new one.
- `/new_session` starts a fresh Codex session for the already bound workspace.
- `/repair_session` verifies that the saved Codex session still matches the stored workspace path.
- Topic titles currently reflect `busy` and `broken` state.
- `hcodex` is the managed local TUI path. It resolves the workspace daemon from `.threadbridge/state/app-server/current.json` and launches `codex --remote ...`.
- `hcodex` no longer self-heals the shared runtime; if the workspace runtime is unavailable, start `threadbridge_desktop` and repair the workspace runtime from the management UI first.
- The local management API defaults to `http://127.0.0.1:38420` and can be changed with `THREADBRIDGE_MANAGEMENT_BIND_ADDR`.
- On macOS, the tray menu lists one submenu per managed workspace with `New Session` and `Continue Telegram Session`.
- On macOS, tray `Add Workspace` opens a native folder picker, treats `workspace = thread`, creates and binds immediately when the workspace is new, and shows a desktop notification instead of auto-opening the browser.
- The browser-based management UI now follows the same `workspace = thread` model. Its main surface is `Workspaces` and `Archived Workspaces`; first-run onboarding is intentionally not exposed until a usable flow exists.
- The local management UI can open a managed workspace in Finder, repair a workspace runtime, refresh the managed Codex cache from the current `codex` on `PATH`, and build a managed source Codex binary from the local Codex Rust workspace.
- The local management UI can also trigger a global desktop runtime owner reconcile across all non-conflicted managed workspaces.
- In the local management UI, conflicted workspaces are shown but launch/resume controls stay disabled until the binding conflict is resolved, and archive/restore now require explicit confirmation.
- The managed Codex source-build flow now exposes default source repo / Rust workspace / build profile values in the management API and lets the local UI override them per build.
- Those managed Codex build defaults are now persisted under `.threadbridge/codex/build-config.json`, so the desktop runtime keeps using the same local source-build settings across restarts.
- The desktop runtime owner now proactively ensures both the shared app-server daemon and the workspace TUI proxy for managed workspaces.
- The management surface now shows TUI adoption-pending state per thread and workspace, so local handoff is visible without reading raw state files.
- Workspace and aggregate runtime health now treat desktop owner heartbeat as the primary authority and surface handoff state as `pending_adoption`/degraded readiness instead of reporting a fully ready handoff while TUI adoption is still unresolved.
- Aggregate runtime health now also exposes desktop runtime owner state, last successful reconcile timestamp, last error, and the last reconcile report through the local management API.
- In the desktop runtime, saving Telegram setup no longer always implies a restart; the local UI now reports restart-required only when no active runtime owner can auto-retry polling.
- The local management UI can now explicitly adopt or reject a pending TUI session handoff instead of waiting for Telegram callback controls or implicit auto-adopt.
- Transcript mirror now distinguishes final transcript entries from process transcript entries, including plan/tool events from the shared runtime.
- The local management UI now exposes a transcript pane per active workspace through `GET /api/threads/:thread_key/transcript`, and both Telegram-initiated turns and local/TUI mirror updates now feed Telegram rolling preview through the same normalized process transcript summaries instead of separate heuristics.
- Telegram text delivery now uses a unified two-line role layout: `👤` for user text, `🤖` for assistant text, `❗️` for system/status text, and `●/○` for in-progress drafts. Drafts now go through the same HTML renderer path as final replies, with plain-text fallback if `sendMessageDraft` HTML delivery fails.
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

- After `/add_workspace`, run `./.threadbridge/bin/hcodex` inside that workspace.
- With no extra args, `hcodex` starts a fresh local TUI session through the shared workspace daemon.
- Use `hcodex resume <session-id>` when you explicitly want to continue an existing Codex session.
- Raw `codex` launches that bypass `hcodex` are not part of the managed local path.

## Commands

- `/start`
- `/add_workspace`
- `/new_session`
- `/workspace_info`
- `/archive_workspace`
- `/restore_workspace`
- `/rename_workspace`
- `/repair_session`

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
