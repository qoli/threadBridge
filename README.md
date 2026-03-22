# threadBridge

`threadBridge` is a workspace-first Codex runtime with Telegram and a local desktop management surface.

Today, the supported product shape is:

- a macOS desktop runtime owner started via `threadbridge_desktop`
- a local management API and browser UI
- a Telegram bot adapter for workspace threads
- real bound workspaces with a managed `.threadbridge/` runtime surface
- shared workspace `codex app-server` daemons plus a managed local `hcodex` entrypoint

## Current Status

The current codebase has already landed these major pieces:

- desktop-first runtime ownership and reconcile flow
- workspace-first binding model: one managed workspace thread per workspace
- shared workspace app-server daemon plus local TUI proxy
- Telegram text and image turns bound to the saved Codex session
- local/browser management UI for setup, launch, reconnect, archive/restore, runtime repair, and transcript inspection
- workspace-scoped execution modes: `full_auto` and `yolo`

Still in progress at the plan level:

- deeper runtime/adapter abstraction beyond Telegram
- richer delivery queue and status-control semantics
- Telegram Web App style observability beyond the current local management UI

If docs and implementation differ, treat the code as authoritative. Plan maturity is tracked in [docs/plan/README.md](/Volumes/Data/Github/threadBridge/docs/plan/README.md).

## Runtime Model

The current runtime is organized like this:

1. `threadbridge_desktop` is the formal runtime owner.
2. The desktop process hosts the local management API, tray menu, and runtime-owner reconcile loop.
3. Telegram is an adapter on top of that runtime, not the owner.
4. Each managed workspace is a real local directory, not a mirrored copy under `data/`.
5. Each workspace gets a managed `.threadbridge/` surface plus an appended runtime block in `AGENTS.md`.
6. Codex continuity is stored in bot-local metadata under `data/<thread-key>/session-binding.json`.

The supported startup path is desktop-first. Headless startup is no longer the intended operating model.

## Workspace Execution Modes

Execution mode is now a workspace-scoped runtime setting.

- Default mode: `full_auto`
- Optional mode: `yolo`
- Workspace config path: `./.threadbridge/state/workspace-config.json`

Current mode semantics are aligned to Codex:

- `full_auto` => `approvalPolicy=on-request` + `sandbox=workspace-write`
- `yolo` => `approvalPolicy=never` + `sandbox=danger-full-access`

This setting is sticky per workspace:

- fresh sessions start with the workspace mode
- `hcodex` new-session and resume commands use the workspace mode
- Telegram turns re-assert the workspace mode on resume
- if the workspace mode changes, the next turn or resume converges the current session to that mode

## Requirements

- macOS for the supported desktop runtime path
- Rust toolchain
- Python 3
- `codex` CLI installed and authenticated on the machine
- a Telegram bot token from BotFather
- Telegram topics enabled if you want workspace-thread workflows in private chat

## Setup

1. Copy `.env.example` to `.env.local`.
2. Fill in at least:
   - `TELEGRAM_BOT_TOKEN`
   - `AUTHORIZED_TELEGRAM_USER_IDS`
3. Start the desktop runtime:

```bash
export CARGO_HOME="$PWD/.cargo" CARGO_TARGET_DIR="$PWD/target"
cargo run --bin threadbridge_desktop
```

The desktop runtime can also start without Telegram credentials. In that state, the tray and local management UI still work, and you can finish setup before polling is active.

Default local management address:

```text
http://127.0.0.1:38420
```

Override it with `THREADBRIDGE_MANAGEMENT_BIND_ADDR`.

## Local Helper Script

For local development on macOS, you can use:

```bash
scripts/local_threadbridge.sh build
scripts/local_threadbridge.sh start
scripts/local_threadbridge.sh restart
scripts/local_threadbridge.sh status
scripts/local_threadbridge.sh logs
```

The helper also manages which Codex binary the managed `hcodex` path should prefer:

```bash
scripts/local_threadbridge.sh build --codex-source brew
scripts/local_threadbridge.sh build --codex-source source
```

- `brew`: prefer the system `codex` on `PATH`
- `source`: build `codex-cli` from a local Codex Rust workspace and cache it under `.threadbridge/codex/codex`

That preference is persisted in `.threadbridge/codex/source.txt`.

## First Run Flow

The intended user flow is:

1. Start `threadbridge_desktop`.
2. Open the local management UI or use the tray.
3. Send `/start` to the bot in your private Telegram chat so the control chat exists.
4. Add a workspace:
   - from Telegram: `/add_workspace <absolute-path>`
   - or from the local management UI / tray folder picker
5. threadBridge binds that real workspace, installs `.threadbridge/`, and starts a fresh Codex session.
6. Continue using either:
   - the Telegram workspace thread
   - or local `./.threadbridge/bin/hcodex`

## Telegram Commands

The current workspace-thread flow uses these commands:

- `/start`
- `/add_workspace <absolute-path>`
- `/new_session`
- `/repair_session`
- `/workspace_info`
- `/archive_workspace`
- `/restore_workspace`
- `/rename_workspace`

Operationally:

- the main private chat is the control console
- each managed workspace gets its own Telegram topic/thread
- normal messages in that workspace thread continue the saved Codex session

## Local `hcodex`

After a workspace is bound, use the managed local TUI path:

```bash
./.threadbridge/bin/hcodex
```

Resume a specific session with:

```bash
./.threadbridge/bin/hcodex resume <session-id>
```

Important current behavior:

- `hcodex` depends on the desktop runtime owner
- `hcodex` no longer self-heals missing workspace runtime state
- `hcodex` launch and resume commands follow the workspace execution mode
- raw `codex` launches that bypass `hcodex` are outside the managed path

## Runtime Layout

Bot-local state lives under `data/`:

- `data/main-thread/` for the control console
- `data/<thread-key>/` for metadata, transcripts, session binding, and image-state artifacts

Workspace-local managed runtime surface:

- `AGENTS.md` managed appendix block
- `.threadbridge/bin/build_prompt_config`
- `.threadbridge/bin/generate_image`
- `.threadbridge/bin/hcodex`
- `.threadbridge/bin/send_telegram_media`
- `.threadbridge/state/workspace-config.json`
- `.threadbridge/state/app-server/current.json`
- `.threadbridge/state/shared-runtime/current.json`
- `.threadbridge/state/shared-runtime/events.jsonl`
- `.threadbridge/tool_requests/`
- `.threadbridge/tool_results/`

The real workspace is authoritative for project files. `data/` is threadBridge runtime state, not a projected copy of the repo.

## Management Surface

The local management API and browser UI currently provide:

- Telegram setup and polling state
- managed workspace list and archive list
- runtime-owner health and reconcile controls
- workspace launch controls for new, continue-current, and resume
- runtime repair
- Codex cache refresh and source-build controls
- transcript inspection
- workspace execution mode changes

On macOS, the tray menu also exposes:

- one submenu per managed workspace
- `New Session`
- `Continue Telegram Session`
- `Add Workspace`
- `Settings`

## Development

Use the repo-local Cargo paths:

```bash
export CARGO_HOME="$PWD/.cargo" CARGO_TARGET_DIR="$PWD/target"
cargo check
cargo test
cargo fmt
cargo clippy --all-targets --all-features
```

Supported desktop runtime entrypoint:

```bash
cargo run --bin threadbridge_desktop
```

## Plans And Docs

- plan index: [docs/plan/README.md](/Volumes/Data/Github/threadBridge/docs/plan/README.md)
- maintainer guide: [AGENTS.md](/Volumes/Data/Github/threadBridge/AGENTS.md)
- workspace runtime appendix source: [templates/AGENTS.md](/Volumes/Data/Github/threadBridge/templates/AGENTS.md)

The plan directory contains a mix of landed work, partial work, and pure drafts. Do not assume every plan describes current behavior.

## Security

- Keep secrets in `.env.local`.
- Do not commit `data/`, logs, generated images, or provider payloads unless they are intentional fixtures.
- Bot-local state and workspace-local runtime artifacts may contain prompts, transcripts, image references, and provider metadata.
