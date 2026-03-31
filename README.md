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
6. Each managed workspace is a real local directory, not a mirrored copy under the bot-local runtime data root.
7. Each workspace gets a managed `.threadbridge/` surface plus an appended runtime block in `AGENTS.md`.
8. Codex continuity is stored in bot-local metadata under the runtime data root, for example `data/<thread-key>/session-binding.json` in debug builds.

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

Bot-local runtime state defaults by build profile:

- debug builds use repo-local `./data`
- release builds use the platform local app-data directory
- on macOS, the release default is `~/Library/Application Support/threadBridge`
- `DATA_ROOT` and `DEBUG_LOG_PATH` can override either mode explicitly

Default local management address:

```text
http://127.0.0.1:38420
```

Override it with `THREADBRIDGE_MANAGEMENT_BIND_ADDR`.

## Local Helper Script

`scripts/local_threadbridge.sh` is the supported macOS helper for day-to-day local development: build the latest code, bundle it locally, and restart the desktop runtime quickly.

Typical usage:

```bash
scripts/local_threadbridge.sh build
scripts/local_threadbridge.sh bundle
scripts/local_threadbridge.sh start
scripts/local_threadbridge.sh restart
scripts/local_threadbridge.sh status
scripts/local_threadbridge.sh logs
```

Supported local-helper workflow assumes the default `BUILD_PROFILE=dev`, so bot-local runtime state stays in repo-local `./data`.
If you need a different path during development, set `DATA_ROOT` explicitly.
`BUILD_PROFILE=release` still exists as an implementation escape hatch, but public release packaging should use `scripts/release_threadbridge.sh` instead of `local_threadbridge.sh`.

The helper also manages which Codex binary the managed `hcodex` path should prefer:

```bash
scripts/local_threadbridge.sh build --codex-source brew
scripts/local_threadbridge.sh build --codex-source source
```

- `brew`: prefer the system `codex` on `PATH`
- `source`: build `codex-cli` from a local Codex Rust workspace and cache it under `.threadbridge/codex/codex`

That preference is persisted in `.threadbridge/codex/source.txt`.
On macOS, `build` also refreshes the app icon assets from the single approved source [`icon/EXPORT_mac_icon.png`](/Volumes/Data/Github/threadBridge/icon/EXPORT_mac_icon.png). This image is already a finished `1024x1024` macOS tile with the intended rounded corners and padding, so the icon pipeline no longer applies any extra zoom or rounded-mask step.
`start` now launches the bundled app executable from `threadBridge.app`, so the running desktop process can inherit the bundle icon instead of showing the generic bare-binary `exec` icon. The bundled desktop runtime is also marked as a menubar-only app, so normal operation stays out of the Dock.

## Public Release Script

Use `scripts/release_threadbridge.sh` for the public macOS release pipeline. This is separate from the local dev helper and is the supported path for signed, notarized, distributable artifacts.
It stays on the existing Rust/cargo packaging route; there is no Xcode wrapper project and no `signingStyle: automatic` release path in this repo.

Typical subcommands:

```bash
scripts/release_threadbridge.sh build --version 0.1.0-rc.1
scripts/release_threadbridge.sh sign --version 0.1.0-rc.1 --codesign-identity "Developer ID Application: Example, Inc. (TEAMID)"
scripts/release_threadbridge.sh dmg --version 0.1.0-rc.1 --codesign-identity "Developer ID Application: Example, Inc. (TEAMID)"
scripts/release_threadbridge.sh notarize --version 0.1.0-rc.1 --codesign-identity "Developer ID Application: Example, Inc. (TEAMID)"
scripts/release_threadbridge.sh release --version 0.1.0-rc.1 --notes-file docs/releases/0.1.0-rc.1.md --codesign-identity "Developer ID Application: Example, Inc. (TEAMID)"
```

The `release` command performs the full build -> sign -> DMG -> notarize -> publish pipeline.
Artifacts are written to `dist/release/<version>/`.

Current pipeline contract:

- builds a universal macOS app bundle for `arm64` and `x86_64`
- copies `app_server_ws_worker` into the bundled app so the distributed runtime can launch its workspace worker
- signs the app with hardened runtime
- creates a single canonical DMG and checksum
- submits that DMG with `notarytool`, then staples and validates it
- publishes the notarized DMG and checksum to a GitHub draft prerelease
- does not include Homebrew tap publication in the first RC path

The release script is macOS-only and fails fast if the worktree is dirty or required CLIs are missing.
It performs the committed path directly with `codesign`, `notarytool`, `stapler`, `hdiutil`, and `gh`.

## Private Fastfile Pattern

If you prefer using `fastlane` for your own local Apple bootstrap, keep it private and ignored. This repo does not commit `fastlane/` files.

Recommended private helper responsibilities:

```bash
apple_audit
bootstrap_notary_profile
match_developer_id
```

A private/local Fastfile can help with:

- preflighting `Developer ID Application` visibility
- creating the local `threadbridge-notary` profile with Apple ID + app-specific password
- syncing `Developer ID Application` into the local keychain when you prefer fastlane `match`

The committed release contract does not depend on any tracked Fastfile.

## Build macOS App Bundle Icon

The app bundle icon is built from the single canonical source [`icon/EXPORT_mac_icon.png`](/Volumes/Data/Github/threadBridge/icon/EXPORT_mac_icon.png). This is separate from the tray icon used by the running menu bar app.

Generate the macOS iconset and `.icns` directly:

```bash
scripts/build_macos_app_icon.sh
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

The bundle metadata points to [`rust/static/app_icon/threadBridge.icns`](/Volumes/Data/Github/threadBridge/rust/static/app_icon/threadBridge.icns) and injects the Dock-hiding `LSUIElement` flag from `rust/static/macos/menubar-only.plist`. The tray icon path in [`rust/src/bin/threadbridge_desktop.rs`](/Volumes/Data/Github/threadBridge/rust/src/bin/threadbridge_desktop.rs) stays unchanged.

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

Bot-local state lives under the runtime data root:

- debug builds default this root to `data/`
- release builds default this root to the platform local app-data directory
- for example, debug mode stores `data/main-thread/` for the control console
- debug mode also stores `data/<thread-key>/` for metadata, transcripts, session binding, and image-state artifacts

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

The real workspace is authoritative for project files. The bot-local runtime data root is threadBridge state, not a projected copy of the repo.

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
- Do not commit repo-local `data/`, logs, generated images, or provider payloads unless they are intentional fixtures.
- Bot-local state and workspace-local runtime artifacts may contain prompts, transcripts, image references, and provider metadata.
