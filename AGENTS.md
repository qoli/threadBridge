# Repository Guidelines

## Purpose
This root `AGENTS.md` is the maintainer guide for `threadBridge`. It documents the repo layout, runtime boundaries, workspace lifecycle, and contributor conventions for the Telegram bot and its Codex app-server integration.

It is not the runtime skill used inside a bound project workspace. That skill lives in [runtime_support/templates/threadbridge-runtime-skill/SKILL.md](/Volumes/Data/Github/threadBridge/runtime_support/templates/threadbridge-runtime-skill/SKILL.md), is installed under `.threadbridge/skills/threadbridge-runtime/` by the runtime bootstrap, and is exposed to Codex through `.codex/skills/threadbridge-runtime`.

## Project Structure & Runtime Architecture
The runtime is organized in four layers:

- Desktop runtime owner and management plane: the macOS desktop entrypoint owns the local management API, tray/web management UI, runtime-owner reconcile loop, managed Codex preferences/builds, and machine-level runtime health authority.
- Shared runtime control and projection: internal services own workspace runtime ensure/repair, workspace session bind/new/repair, Telegram-to-live-TUI routing, and app-server observer projection. This layer is adapter-neutral and is where workspace control semantics now live.
- Telegram adapter: the Rust bot receives Telegram updates, enforces authorization, routes commands into shared runtime control, streams live Codex previews, and sends results back to Telegram, but it is no longer the formal runtime owner nor the primary home of runtime control orchestration.
- Tool executors: workspace-local wrapper commands under `.threadbridge/bin/` call Python scripts in `runtime_support/tools/` to materialize prompt configs, generated images, and Telegram outbox payloads.

Important repo areas:

- `rust/src/bin/threadbridge_desktop.rs`: desktop runtime entrypoint, tray host, and Telegram bot launcher.
- `rust/src/codex.rs`: app-server JSON-RPC client, thread lifecycle helpers, and event normalization for previews.
- `rust/src/management_api.rs`: local HTTP management API, workspace/thread views, control actions, and setup/runtime endpoints for the desktop management surface.
- `rust/src/runtime_owner.rs`: desktop runtime owner heartbeat, reconcile loop, and workspace runtime health authority.
- `rust/src/runtime_control.rs`: shared runtime control services for workspace runtime, session lifecycle, and Telegram/TUI routing.
- `rust/src/app_server_observer.rs`: app-server observer that projects preview/final/process events and emits adapter-neutral interaction events.
- `rust/src/runtime_interaction.rs`: shared interaction event types for `request_user_input`, resolved requests, and turn completion follow-up.
- `rust/src/process_transcript.rs`: normalized final/process transcript mapping shared by management UI and Telegram preview surfaces.
- `rust/src/workspace.rs`: workspace bootstrap logic that installs `.threadbridge/`, the Codex repo-skill symlink, workspace-local runtime skills, wrappers, and runtime state surfaces.
- `rust/src/repository.rs`: persistent bot-local thread state for metadata, transcripts, session bindings, and image-state artifacts.
- `rust/src/thread_state.rs`: canonical thread state resolver for `lifecycle_status`, `binding_status`, and `run_status`.
- `rust/src/telegram_runtime/`: Telegram command handling, message flows, image handling, preview rendering, and adapter-owned interaction bridging.
- `runtime_support/templates/threadbridge-runtime-skill/`: workspace-local runtime skill template and references installed into `.threadbridge/skills/threadbridge-runtime/` and exposed through `.codex/skills/threadbridge-runtime`.
- `runtime_support/tools/`: Python executors invoked from `.threadbridge/bin/*`.
- bot-local runtime data root: debug builds default to `data/`; bundled release builds default to the platform local app-data directory under `threadBridge/data`. In debug mode, `data/main-thread/` stores the control console state and each thread maps to `data/<thread-key>/`.

Treat `target/` and most bot-local runtime state as generated output.

## Runtime Instructions
There is one project `AGENTS.md` role and one threadBridge runtime skill role:

- Root `AGENTS.md`: this maintainer guide.
- `runtime_support/templates/threadbridge-runtime-skill/SKILL.md`: workspace-local threadBridge runtime skill installed under `.threadbridge/skills/threadbridge-runtime/` and exposed through `.codex/skills/threadbridge-runtime`.

There is no thread-local runtime-root `<thread-key>/AGENTS.md` surface anymore.

Do not inject threadBridge runtime contracts into project `AGENTS.md` during ordinary workspace ensure, resume, or reconcile. Runtime capability documentation belongs in the workspace-local skill and its references. Legacy managed `AGENTS.md` cleanup belongs behind explicit runtime support rebuild/migration flows, not opportunistic reconcile.

## Workspace Lifecycle & Data Flow
The operational flow is: desktop runtime owner -> local management API / Telegram adapter -> Codex app-server thread -> real workspace runtime -> Python tool wrappers -> Telegram reply or local management surface.

From a maintainer perspective:

- `threadbridge_desktop` is the supported startup path. It can start the tray and local management API before Telegram polling is configured, and it is the formal owner for runtime health and reconcile behavior.
- `/add_workspace <absolute-path>` creates or reuses the Telegram workspace thread, installs the `.threadbridge/` surface and runtime skill into that real workspace, and starts a fresh Codex session for that workspace through app-server.
- The local management API exposes equivalent create-bind, reconnect, archive/restore, launch, managed Codex, and runtime-owner reconcile flows for the desktop management UI.
- Shared runtime control now owns workspace runtime ensure, session bind/new/repair, and live-TUI routing; Telegram and management surfaces call into that layer rather than each carrying their own runtime helper stack.
- `session-binding.json` stores the mapping between the Telegram thread, the real workspace path, and the current Codex `thread.id`.
- `.threadbridge/state/workspace-config.json` stores the workspace-local execution mode that all fresh and resumed Codex sessions should converge to for that workspace.
- Normal thread messages resume the saved Codex thread through app-server and run turns in the bound workspace.
- Uploaded images are stored under the bot-local runtime data root, for example `data/<thread-key>/state/` in debug builds, accumulated into a pending batch, and analyzed by Codex in the same bound workspace context.
- If Codex session continuity breaks or the returned `thread.cwd` no longer matches the stored workspace path, the binding is marked broken and requires `/repair_session` or `/new_session`.
- Runtime health is owner-canonical: desktop owner heartbeat and reconcile state are the authority for whether a managed workspace runtime is healthy; workspace shared status remains an activity/observation surface.
- `hcodex` is a managed local TUI entrypoint into the shared workspace runtime. It depends on the desktop runtime owner and does not self-heal the workspace runtime by itself.
- Observer projection and Telegram interaction UI are now split: app-server observer emits adapter-neutral interaction events, and Telegram-specific prompt/callback handling lives under `telegram_runtime/interaction_bridge.rs`.
- `/restore_workspace` is Telegram/local-state only. It restores an archived Telegram topic and local metadata; it does not recreate Codex continuity by itself.

## Artifact Boundaries
Maintain these ownership boundaries:

- Rust bot and repository layer own bot-local thread state:
  - `metadata.json`
  - `conversations.jsonl`
  - `session-binding.json`
  - `state/pending-image-batch.json`
  - `state/images/source/`
  - `state/images/analysis/`
- Workspace bootstrap owns:
  - `.threadbridge/bin/`
  - `.threadbridge/skills/threadbridge-runtime/`
  - `.codex/skills/threadbridge-runtime`
  - `.threadbridge/state/workspace-config.json`
  - `.threadbridge/codex/source.txt`
  - `.threadbridge/codex/build-config.json`
  - `.threadbridge/codex/build-info.txt`
  - `.threadbridge/codex/codex`
  - `.threadbridge/tool_requests/`
  - `.threadbridge/tool_results/`
- `runtime_support/tools/build_prompt_config.py` owns `concept.json`, append-only `prompts/*.json`, and `.threadbridge/tool_results/build_prompt_config.result.json`.
- `runtime_support/tools/generate_image.py` owns `.threadbridge/tool_results/generate_image.result.json` and the generated run folders under `images/generated/`.
- `runtime_support/tools/send_telegram_media.py` owns `.threadbridge/tool_results/send_telegram_media.result.json` and `.threadbridge/tool_results/telegram_outbox.json`.

When adding new features, decide first which layer owns the artifact and which layer merely presents it.

## Build, Test, and Development Commands
Use the repo-local Cargo paths from the README:

```bash
export CARGO_HOME="$PWD/.cargo" CARGO_TARGET_DIR="$PWD/target"
cargo run --bin threadbridge_desktop
cargo check
cargo test
cargo fmt
cargo clippy --all-targets --all-features
```

`cargo run --bin threadbridge_desktop` starts the supported desktop runtime, local management API, and Telegram bot launcher path. `cargo check` is the fastest correctness pass. `cargo test` runs the Rust unit tests. `cargo fmt` and `cargo clippy` use standard Rust tooling.

## Coding Style & Naming Conventions
Follow `rustfmt` defaults for Rust: 4-space indentation, `snake_case` for functions and modules, `PascalCase` for types, and small focused modules. Match the existing style in `rust/src/` by returning `anyhow::Result`, using `serde`-friendly structs, and keeping async I/O in Tokio-aware helpers.

Python tools in `runtime_support/tools/` use 4-space indentation, explicit helper functions, and stdlib-first implementations unless a dependency is already justified elsewhere.

When changing runtime behavior, keep the separation between Telegram orchestration, Codex thread control, and tool execution clear in both code and documentation.

## Testing Guidelines
Tests live inline under `#[cfg(test)]` blocks in the Rust modules. Prefer `#[tokio::test]` for async paths and descriptive test names.

Add or update tests when behavior changes in:

- repository persistence and state transitions
- app-server request/response handling
- management API views, control actions, and wire semantics
- runtime owner reconcile and health aggregation
- transcript mirror and process transcript normalization
- canonical thread/workspace state resolution
- workspace bootstrap and runtime skill generation
- tool-result parsing and artifact path handling

## Commit & Pull Request Guidelines
Use short imperative commit subjects. Conventional Commit style with a scope is a good default, for example `feat(threadbridge): add workspace binding`.

Pull requests should explain the user-visible behavior change, note any config or data migration impact, link the related issue or thread, and include screenshots or log snippets when changing Telegram flows or generated artifacts.

## Security & Configuration Tips
Keep secrets in `data/config.env.local`; never commit real tokens. Start from `.env.example`, and avoid checking generated workspace files, debug logs, or image outputs into Git unless they are intentional fixtures.

Treat bot-local runtime state and workspace-local generated files as potentially sensitive because they can contain prompts, transcripts, image references, provider payloads, and output metadata.
