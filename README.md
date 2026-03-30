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
- shared workspace app-server daemon plus local `hcodex` ingress
- shared runtime control services for workspace runtime, session lifecycle, and Telegram-to-TUI routing
- Telegram text and image turns bound to the saved Codex session
- Telegram collaboration mode commands plus the current Telegram-side interactive question flow:
  - workspace threads can switch between `default` and `plan`
  - app-server observer and `hcodex` ingress now emit adapter-neutral runtime interaction events
  - Telegram can answer `Questions` prompts and the post-plan `Implement this plan?` callback through the adapter-owned interaction bridge on the same session continuity
- Telegram preview drafts and final replies now use the current Telegram delivery pipeline:
  - preview drafts go through `sendMessageDraft`, prefer HTML rendering, and fall back to plain text if draft HTML send fails
  - final replies prefer inline Telegram HTML, fall back to plain text on send failure, and switch to a Markdown attachment when the inline reply is too long
- local/browser management UI for setup, launch, reconnect, archive/restore, runtime repair, and transcript inspection
- workspace-scoped execution modes: `full_auto` and `yolo`

Still in progress at the plan level:

- deeper runtime/adapter abstraction beyond Telegram
- richer delivery queue and status-control semantics
- Telegram Web App style observability beyond the current local management UI

If docs and implementation differ, treat the code as authoritative. Plan maturity is tracked in [docs/plan/README.md](/Volumes/Data/Github/threadBridge/docs/plan/README.md).

For the maintainer-facing plan registry, grouped by maturity status and organized into owner folders, see [docs/plan/README.md](/Volumes/Data/Github/threadBridge/docs/plan/README.md).

## Runtime Model

The current runtime is organized like this:

1. `threadbridge_desktop` is the formal runtime owner.
2. The desktop process hosts the local management API, tray menu, and runtime-owner reconcile loop.
3. Shared runtime control handles workspace runtime ensure, session bind/new/repair, and Telegram-to-live-TUI routing.
4. App-server observer owns transcript/process projection; Telegram interaction UI is bridged separately from observer read-side logic.
5. Telegram is an adapter on top of that runtime, not the owner.
6. Each managed workspace is a real local directory, not a mirrored copy under `data/`.
7. Each workspace gets a managed `.threadbridge/` surface plus an appended runtime block in `AGENTS.md`.
8. Codex continuity is stored in bot-local metadata under `data/<thread-key>/session-binding.json`.

The supported startup path is desktop-first. Headless or self-managed paths still exist as internal fallback/probe surfaces, but they are not the intended operating model.

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
scripts/local_threadbridge.sh bundle
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
On macOS, `build` also refreshes the app icon assets. In `symbol` mode it starts from [`icon/round-04-no-tile-dark-bg-v1.png`](/Volumes/Data/Github/threadBridge/icon/round-04-no-tile-dark-bg-v1.png), scales the source to `150%`, center-crops back to `1024x1024`, then applies the macOS rounded mask before bundling. In `final-tile` mode it starts from a prebuilt tile image such as [`icon/p2-brand-loop-r1-tile-1024.png`](/Volumes/Data/Github/threadBridge/icon/p2-brand-loop-r1-tile-1024.png) and skips the extra zoom + mask pass.
`start` now launches the bundled app executable from `threadBridge.app`, so the running desktop process can inherit the bundle icon instead of showing the generic bare-binary `exec` icon.

## Build macOS App Bundle Icon

The app bundle icon can be generated from either a symbol-only source such as [`icon/round-04-no-tile-dark-bg-v1.png`](/Volumes/Data/Github/threadBridge/icon/round-04-no-tile-dark-bg-v1.png) or a prebuilt tile source such as [`icon/p2-brand-loop-r1-tile-1024.png`](/Volumes/Data/Github/threadBridge/icon/p2-brand-loop-r1-tile-1024.png). This is separate from the tray icon used by the running menu bar app.

Extract the tile from the current reference screenshot and rebuild it as a reusable `1024x1024` source:

```bash
python3 scripts/extract_macos_icon_tile.py
```

Generate the macOS iconset and `.icns` directly:

```bash
scripts/build_macos_app_icon.sh
```

Or build directly from the extracted tile source without adding another rounded mask:

```bash
scripts/build_macos_app_icon.sh --source-mode final-tile
```

Or use the local helper to bundle the desktop app with the generated icon:

```bash
scripts/local_threadbridge.sh bundle
```

Manual equivalent:

```bash
export CARGO_HOME="$PWD/.cargo" CARGO_TARGET_DIR="$PWD/target"
cargo install cargo-bundle
cargo bundle --bin threadbridge_desktop
```

The bundle metadata points to [`rust/static/app_icon/threadBridge.icns`](/Volumes/Data/Github/threadBridge/rust/static/app_icon/threadBridge.icns). The tray icon path in [`rust/src/bin/threadbridge_desktop.rs`](/Volumes/Data/Github/threadBridge/rust/src/bin/threadbridge_desktop.rs) stays unchanged.

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

The maintained slash-command reference lives in [docs/telegram-slash-commands.md](/Volumes/Data/Github/threadBridge/docs/telegram-slash-commands.md).

For the maintainer-facing registry of current responsibility areas across Telegram, management, `hcodex`, and shared runtime specs, see [docs/plan/README.md](/Volumes/Data/Github/threadBridge/docs/plan/README.md).

Current command groups:

- control chat: `/start`, `/add_workspace <absolute-path>`, `/restore_workspace`
- workspace thread: `/start`, `/new_session`, `/repair_session`, `/workspace_info`, `/rename_workspace`, `/archive_workspace`, `/launch`, `/execution_mode`, `/sessions`, `/session_log <session_id>`, `/stop`, `/plan_mode`, `/default_mode`

Operationally:

- the main private chat is the control console
- each managed workspace gets its own Telegram topic/thread
- normal messages in that workspace thread continue the saved Codex session

## Telegram Collaboration

Current collaboration behavior is:

- `/plan_mode` switches the current workspace thread into Plan collaboration mode
- `/default_mode` switches the current workspace thread back to Default collaboration mode
- direct Telegram turns and TUI-mirrored turns can surface `Questions` prompts in Telegram
- option-based questions render inline buttons plus `Other`; freeform questions use the next text message in the same thread
- secret input is still not supported in Telegram v1
- plan-mode turns can end with an `Implement this plan?` inline prompt that continues on the same saved session

## Telegram Delivery

Current Telegram delivery behavior is:

- preview updates use `sendMessageDraft`
- preview drafts try HTML first and retry as plain text if Telegram rejects the HTML draft
- final assistant replies try inline Telegram HTML first
- if inline final HTML send fails, threadBridge retries as plain text
- if the final reply is too long for inline delivery, threadBridge sends a short notice plus a Markdown attachment
- final reply attachments now do upload-size preflight before Telegram send
- workspace outbox file deliveries also do upload-size preflight
- oversized outbox photos may fall back to Telegram document delivery
- oversized documents or attachments fall back to a warning/notice path instead of relying on a raw Telegram upload failure

This is the current implementation shape; broader delivery queue semantics are still tracked as plan work.

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
- `.threadbridge/state/runtime-observer/current.json`
- `.threadbridge/state/runtime-observer/events.jsonl`
- `.threadbridge/tool_requests/`
- `.threadbridge/tool_results/`

The real workspace is authoritative for project files. `data/` is threadBridge runtime state, not a projected copy of the repo.

## Management Surface

The local management API and browser UI currently provide:

- Telegram setup and polling state
- managed workspace list and archive list
- runtime-owner health and reconcile controls
- typed SSE change events for setup/runtime/thread/workspace/archive refresh
- workspace launch controls for new, continue-current, and resume
- runtime repair
- Codex cache refresh and source-build controls
- transcript inspection
- workspace execution mode changes

For the full maintainer-facing design registry, including owner-folder organization, partial specs, and historical architecture, see [docs/plan/README.md](/Volumes/Data/Github/threadBridge/docs/plan/README.md).

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

- plan registry with status groups and owner folders: [docs/plan/README.md](/Volumes/Data/Github/threadBridge/docs/plan/README.md)
- maintainer guide: [AGENTS.md](/Volumes/Data/Github/threadBridge/AGENTS.md)
- workspace runtime appendix source: [templates/AGENTS.md](/Volumes/Data/Github/threadBridge/templates/AGENTS.md)

The plan directory contains a mix of landed work, partial work, and pure drafts. Do not assume every plan describes current behavior.

## Security

- Keep secrets in `.env.local`.
- Do not commit `data/`, logs, generated images, or provider payloads unless they are intentional fixtures.
- Bot-local state and workspace-local runtime artifacts may contain prompts, transcripts, image references, and provider metadata.
