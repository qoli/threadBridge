# threadBridge

`threadBridge` is a workspace-first Codex runtime with a macOS desktop owner, a Telegram adapter, and a local browser management surface.

The current supported product shape is:

- a macOS desktop runtime owner started via `threadbridge_desktop`
- a local management API and browser UI
- a Telegram bot adapter for workspace threads
- real bound workspaces with a managed `.threadbridge/` runtime surface
- shared workspace `codex app-server` daemons plus a managed local `hcodex` entrypoint

If docs and implementation differ, treat the code as authoritative. The maintainer plan registry lives in [docs/plan/README.md](docs/plan/README.md).

## Current Status

The current codebase already ships these major pieces:

- desktop-first runtime ownership and reconcile flow
- one managed Telegram workspace thread per real workspace
- shared workspace runtime control for workspace ensure, session bind/new/repair, and Telegram-to-TUI routing
- app-server observer projection plus adapter-owned Telegram interaction bridging
- Telegram text and image turns that resume the saved Codex session
- Telegram collaboration mode switching between `default` and `plan`
- Telegram question flow for `Questions` prompts plus the post-plan `Implement this plan?` callback
- preview drafts via `sendMessageDraft`, with HTML-first delivery and plain-text fallback
- final replies with HTML-first delivery, plain-text fallback, and Markdown attachment fallback for oversized inline responses
- local/browser management UI for setup, launch, reconnect, archive/restore, runtime repair, and transcript inspection
- workspace-scoped execution modes: `full_auto` and `yolo`

Still tracked as design or follow-up work:

- deeper runtime and adapter abstraction beyond Telegram
- richer delivery queue and status-control semantics
- broader observability beyond the current local management surface

For slash-command details, see the maintainer reference in [docs/telegram-slash-commands.md](docs/telegram-slash-commands.md).

## Runtime Architecture

The current runtime is organized like this:

1. `threadbridge_desktop` is the formal runtime owner.
2. The desktop process hosts the local management API, tray menu, and runtime-owner reconcile loop.
3. Shared runtime control handles workspace runtime ensure, session bind/new/repair, and Telegram-to-live-TUI routing.
4. App-server observer owns transcript and process projection; Telegram interaction UI is bridged separately.
5. Telegram is an adapter on top of the runtime, not the runtime owner.
6. Each managed workspace is a real local directory, not a projected copy under the bot-local data root.
7. Each workspace gets a managed `.threadbridge/` surface plus a Codex-discoverable `threadbridge-runtime` repo skill symlink.
8. Codex session continuity is stored in bot-local metadata under the runtime data root, for example `data/<thread-key>/session-binding.json` in debug builds.

The supported startup path is desktop-first. Any non-desktop compatibility paths are internal support surfaces, not the intended operating model.

## Workspace Execution Modes

Execution mode is a workspace-scoped runtime setting stored in `./.threadbridge/state/workspace-config.json`.

- default mode: `full_auto`
- optional mode: `yolo`

Current semantics are aligned to Codex:

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

1. Copy `.env.example` to `data/config.env.local`.
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
- on macOS, the bundled release data root is `~/Library/Application Support/threadBridge/data`
- bundled releases install runtime support under `~/Library/Application Support/threadBridge/runtime_support`
- `DATA_ROOT` and `DEBUG_LOG_PATH` can override either mode explicitly

Default local management address:

```text
http://127.0.0.1:38420
```

Override it with `THREADBRIDGE_MANAGEMENT_BIND_ADDR`.

## Local Development

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

### Local Helper Script

`scripts/local_threadbridge.sh` is the supported macOS helper for day-to-day local development. It builds the latest code, bundles the app locally, and restarts the desktop runtime quickly.

Typical usage:

```bash
scripts/local_threadbridge.sh build
scripts/local_threadbridge.sh bundle
scripts/local_threadbridge.sh start
scripts/local_threadbridge.sh restart
scripts/local_threadbridge.sh status
scripts/local_threadbridge.sh logs
```

The helper also controls which Codex binary the managed `hcodex` path should prefer:

```bash
scripts/local_threadbridge.sh build --codex-source brew
scripts/local_threadbridge.sh build --codex-source source
```

- `brew`: prefer the system `codex` on `PATH`
- `source`: build `codex-cli` from a local Codex Rust workspace and cache it under `.threadbridge/codex/codex`

That preference is persisted in `.threadbridge/codex/source.txt`.

On macOS, `build`, `bundle`, and `start` refresh the app icon assets from the canonical source [icon/EXPORT_mac_icon.png](icon/EXPORT_mac_icon.png). This image is already the intended `1024x1024` macOS tile, so the icon pipeline does not apply an extra zoom or rounded-mask step.

`start` launches the bundled app executable from `threadBridge.app`, so the running desktop process inherits the bundle icon instead of the generic bare-binary icon. The bundled desktop runtime is also marked as a menubar-only app, so normal operation stays out of the Dock.

### Public Release Script

Use `scripts/release_rc.sh` for the normal macOS RC path. It is a thin wrapper around `scripts/release_threadbridge.sh` that fills the repo defaults automatically:

```bash
scripts/release_rc.sh 0.1.0-rc.2
```

By default the wrapper:

- uses `docs/releases/<version>.md` for release notes
- creates that notes file if it does not exist yet
- defaults the notary profile to `threadbridge-notary`
- bootstraps that notary profile from the local `fastlane/threadbridge-asc` API key when needed
- falls back to the local fastlane `bootstrap_notary_profile` lane when the ASC key path is unavailable
- defaults the GitHub repo to `qoli/threadBridge`
- auto-detects the `Developer ID Application` identity if the machine only has one

If you need the wrapper to also push the git tag and publish the draft prerelease:

```bash
scripts/release_rc.sh 0.1.0-rc.2 --publish-final
```

For lower-level debugging or partial reruns, use `scripts/release_threadbridge.sh` directly. The underlying `release` command still performs the full build -> sign -> DMG -> notarize -> publish pipeline, and artifacts are written to `dist/release/<version>/`.

Current pipeline contract:

- builds a universal macOS app bundle for `arm64` and `x86_64`
- copies `app_server_ws_worker` into the bundled app so the distributed runtime can launch its workspace worker
- signs the app with hardened runtime
- creates a single canonical DMG and checksum
- submits that DMG with `notarytool`, then staples and validates it
- publishes the notarized DMG and checksum to a GitHub draft prerelease

The release script is macOS-only and fails fast if the worktree is dirty or required CLIs are missing.

## First Run Flow

The intended user flow is:

1. Start `threadbridge_desktop`.
2. Open the local management UI or use the tray.
3. Send `/start` to the bot in your private Telegram chat so the control chat exists.
4. Add a workspace:
   - from Telegram: `/add_workspace <absolute-path>`
   - or from the local management UI or tray folder picker
5. threadBridge binds that real workspace, installs `.threadbridge/`, and starts a fresh Codex session.
6. Continue using either:
   - the Telegram workspace thread
   - or local `./.threadbridge/bin/hcodex`

## Telegram Commands

The maintained slash-command reference lives in [docs/telegram-slash-commands.md](docs/telegram-slash-commands.md).

Current command groups:

- control chat: `/start`, `/add_workspace <absolute-path>`, `/restore_workspace`
- workspace thread: `/start`, `/start_fresh_session`, `/repair_session_binding`, `/workspace_info`, `/rename_workspace`, `/archive_workspace`, `/launch_local_session`, `/get_workspace_execution_mode`, `/set_workspace_execution_mode`, `/sessions`, `/session_log <session_id>`, `/stop`, `/plan_mode`, `/default_mode`

Operationally:

- the main private chat is the control console
- each managed workspace gets its own Telegram topic or thread
- normal messages in a workspace thread continue the saved Codex session
- option-based questions render inline buttons plus `Other`; freeform questions use the next text message in the same thread
- plan-mode turns can end with an `Implement this plan?` inline prompt that continues on the same saved session

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
- debug mode stores `data/main-thread/` for the control console
- debug mode also stores `data/<thread-key>/` for metadata, transcripts, session binding, and image-state artifacts

Workspace-local managed runtime surface:

- `.threadbridge/bin/build_prompt_config`
- `.threadbridge/bin/generate_image`
- `.threadbridge/bin/hcodex`
- `.threadbridge/bin/send_telegram_media`
- `.threadbridge/skills/threadbridge-runtime/SKILL.md`
- `.threadbridge/skills/threadbridge-runtime/references/`
- `.codex/skills/threadbridge-runtime` symlink to `.threadbridge/skills/threadbridge-runtime/`
- `.threadbridge/state/workspace-config.json`
- `.threadbridge/state/app-server/current.json`
- `.threadbridge/state/runtime-observer/current.json`
- `.threadbridge/state/runtime-observer/events.jsonl`
- `.threadbridge/tool_requests/`
- `.threadbridge/tool_results/`

The real workspace is authoritative for project files. The bot-local runtime data root is threadBridge state, not a projected copy of the repo.

## Docs

- plan registry and design references: [docs/plan/README.md](docs/plan/README.md)
- maintainer guide: [AGENTS.md](AGENTS.md)
- workspace runtime skill source: [runtime_support/templates/threadbridge-runtime-skill/SKILL.md](runtime_support/templates/threadbridge-runtime-skill/SKILL.md)
- slash-command reference: [docs/telegram-slash-commands.md](docs/telegram-slash-commands.md)
- release notes index: [docs/releases/README.md](docs/releases/README.md)

The plan directory contains a mix of landed work, partial work, and pure drafts. Do not assume every plan document describes current behavior.

## Security

- Keep secrets in `data/config.env.local`.
- Do not commit repo-local `data/`, logs, generated images, or provider payloads unless they are intentional fixtures.
- Bot-local state and workspace-local runtime artifacts may contain prompts, transcripts, image references, and provider metadata.

## Related Assets

- app icon source: [icon/EXPORT_mac_icon.png](icon/EXPORT_mac_icon.png)
- bundled app icon: [rust/static/app_icon/threadBridge.icns](rust/static/app_icon/threadBridge.icns)
