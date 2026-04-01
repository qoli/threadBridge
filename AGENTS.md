# Repository Guidelines

## Purpose
This root `AGENTS.md` is the maintainer guide for `threadBridge`. It documents the repo layout, runtime boundaries, workspace lifecycle, and contributor conventions for the Telegram bot and its Codex app-server integration.

It is not the runtime appendix followed inside a bound project workspace. That appendix lives in [runtime_assets/templates/AGENTS.md](/Volumes/Data/Github/threadBridge/runtime_assets/templates/AGENTS.md) and is appended into a workspace `AGENTS.md` by the runtime bootstrap.

## Project Structure & Runtime Architecture
The runtime is organized in four layers:

- Desktop runtime owner and management plane: the macOS desktop entrypoint owns the local management API, tray/web management UI, runtime-owner reconcile loop, managed Codex preferences/builds, and machine-level runtime health authority.
- Shared runtime control and projection: internal services own workspace runtime ensure/repair, workspace session bind/new/repair, Telegram-to-live-TUI routing, and app-server observer projection. This layer is adapter-neutral and is where workspace control semantics now live.
- Telegram adapter: the Rust bot receives Telegram updates, enforces authorization, routes commands into shared runtime control, streams live Codex previews, and sends results back to Telegram, but it is no longer the formal runtime owner nor the primary home of runtime control orchestration.
- Tool executors: workspace-local wrapper commands under `.threadbridge/bin/` call Python scripts in `runtime_assets/tools/` to materialize prompt configs, generated images, and Telegram outbox payloads.

Important repo areas:

- `rust/src/bin/threadbridge_desktop.rs`: desktop runtime entrypoint, tray host, and Telegram bot launcher.
- `rust/src/codex.rs`: app-server JSON-RPC client, thread lifecycle helpers, and event normalization for previews.
- `rust/src/management_api.rs`: local HTTP management API, workspace/thread views, control actions, and setup/runtime endpoints for the desktop management surface.
- `rust/src/runtime_owner.rs`: desktop runtime owner heartbeat, reconcile loop, and workspace runtime health authority.
- `rust/src/runtime_control.rs`: shared runtime control services for workspace runtime, session lifecycle, and Telegram/TUI routing.
- `rust/src/app_server_observer.rs`: app-server observer that projects preview/final/process events and emits adapter-neutral interaction events.
- `rust/src/runtime_interaction.rs`: shared interaction event types for `request_user_input`, resolved requests, and turn completion follow-up.
- `rust/src/process_transcript.rs`: normalized final/process transcript mapping shared by management UI and Telegram preview surfaces.
- `rust/src/workspace.rs`: workspace bootstrap logic that appends the managed runtime block into a real workspace `AGENTS.md` and installs `.threadbridge/`.
- `rust/src/repository.rs`: persistent bot-local thread state for metadata, transcripts, session bindings, and image-state artifacts.
- `rust/src/thread_state.rs`: canonical thread state resolver for `lifecycle_status`, `binding_status`, and `run_status`.
- `rust/src/telegram_runtime/`: Telegram command handling, message flows, image handling, preview rendering, and adapter-owned interaction bridging.
- `runtime_assets/templates/AGENTS.md`: managed runtime appendix appended to real workspace `AGENTS.md` files.
- `runtime_assets/tools/`: Python executors invoked from `.threadbridge/bin/*`.
- bot-local runtime data root: debug builds default to `data/`; bundled release builds default to the platform local app-data directory under `threadBridge/data`. In debug mode, `data/main-thread/` stores the control console state and each thread maps to `data/<thread-key>/`.

Treat `target/` and most bot-local runtime state as generated output.

## AGENTS.md Roles
There are two relevant `AGENTS.md` roles now:

- Root `AGENTS.md`: this maintainer guide.
- `runtime_assets/templates/AGENTS.md`: the workspace runtime appendix managed by threadBridge and appended into a real bound workspace `AGENTS.md`.

There is no thread-local runtime-root `<thread-key>/AGENTS.md` surface anymore.

The runtime appendix block embedded later in this root file is a checked-in copy of the managed appendix for reference. Treat `runtime_assets/templates/AGENTS.md` as the canonical source for appendix wording and behavior; do not hand-edit the root appendix block when maintaining the guide above it.

## Workspace Lifecycle & Data Flow
The operational flow is: desktop runtime owner -> local management API / Telegram adapter -> Codex app-server thread -> real workspace runtime -> Python tool wrappers -> Telegram reply or local management surface.

From a maintainer perspective:

- `threadbridge_desktop` is the supported startup path. It can start the tray and local management API before Telegram polling is configured, and it is the formal owner for runtime health and reconcile behavior.
- `/add_workspace <absolute-path>` creates or reuses the Telegram workspace thread, installs the runtime appendix and `.threadbridge/` surface into that real workspace, and starts a fresh Codex session for that workspace through app-server.
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
  - the managed block inside the real workspace `AGENTS.md`
  - `.threadbridge/bin/`
  - `.threadbridge/state/workspace-config.json`
  - `.threadbridge/codex/source.txt`
  - `.threadbridge/codex/build-config.json`
  - `.threadbridge/codex/build-info.txt`
  - `.threadbridge/codex/codex`
  - `.threadbridge/tool_requests/`
  - `.threadbridge/tool_results/`
- `runtime_assets/tools/build_prompt_config.py` owns `concept.json`, append-only `prompts/*.json`, and `.threadbridge/tool_results/build_prompt_config.result.json`.
- `runtime_assets/tools/generate_image.py` owns `.threadbridge/tool_results/generate_image.result.json` and the generated run folders under `images/generated/`.
- `runtime_assets/tools/send_telegram_media.py` owns `.threadbridge/tool_results/send_telegram_media.result.json` and `.threadbridge/tool_results/telegram_outbox.json`.

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

Python tools in `runtime_assets/tools/` use 4-space indentation, explicit helper functions, and stdlib-first implementations unless a dependency is already justified elsewhere.

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
- workspace bootstrap and appendix generation
- tool-result parsing and artifact path handling

## Commit & Pull Request Guidelines
Use short imperative commit subjects. Conventional Commit style with a scope is a good default, for example `feat(threadbridge): add workspace binding`.

Pull requests should explain the user-visible behavior change, note any config or data migration impact, link the related issue or thread, and include screenshots or log snippets when changing Telegram flows or generated artifacts.

## Security & Configuration Tips
Keep secrets in `data/config.env.local`; never commit real tokens. Start from `.env.example`, and avoid checking generated workspace files, debug logs, or image outputs into Git unless they are intentional fixtures.

Treat bot-local runtime state and workspace-local generated files as potentially sensitive because they can contain prompts, transcripts, image references, provider payloads, and output metadata.

<!-- threadbridge:runtime:start -->
## threadBridge Runtime Appendix

This managed block is appended by threadBridge to a real project workspace `AGENTS.md`.

### Runtime Model

- The current working directory is the real bound workspace, not a projected copy.
- Preserve this workspace's own conventions and instructions. This appendix adds bot/runtime behavior; it does not replace project-local rules.
- threadBridge tracks Telegram-thread metadata outside the workspace under its own bot-local runtime data root. In debug builds this defaults to repo-local `data/`; in release builds it defaults to the platform local app-data directory. That bot-local state is not the source of truth for project files.
- Use the current Codex thread context as the primary continuity source. Do not rebuild long transcript replays unless a workflow explicitly requires it.

### Runtime Surface

- threadBridge installs wrapper commands under:
  - `./.threadbridge/bin/build_prompt_config`
  - `./.threadbridge/bin/generate_image`
  - `./.threadbridge/bin/hcodex`
  - `./.threadbridge/bin/send_telegram_media`
- threadBridge installs local shell/runtime sync files under:
  - `./.threadbridge/state/workspace-config.json`
  - `./.threadbridge/state/app-server/current.json`
  - `./.threadbridge/state/runtime-observer/current.json`
  - `./.threadbridge/state/runtime-observer/events.jsonl`
- threadBridge request/result files live under:
  - `./.threadbridge/tool_requests/`
  - `./.threadbridge/tool_results/`
- Keep these wrapper names and paths stable.
- `./.threadbridge/state/runtime-observer/*` is a workspace-local observation and activity surface.
- Treat desktop owner heartbeat and management/runtime protocol views as the canonical runtime-health authority, not `runtime-observer/*` by itself.

### Local Codex TUI

- Run `./.threadbridge/bin/hcodex` for the managed local TUI path in this workspace.
- `hcodex` resolves the shared workspace daemon from `./.threadbridge/state/app-server/current.json` and launches `codex --remote ...`.
- `hcodex` also reads `./.threadbridge/state/workspace-config.json` so local launch and resume use the workspace execution mode.
- With no extra args, `hcodex` starts a fresh local TUI session for this workspace.
- Fresh `hcodex` sessions project mirror activity from the existing live daemon stream; standalone observer attach is reserved for explicit resume flows.
- Use `hcodex resume <session-id>` when you explicitly want to continue an existing Codex session.

### `./.threadbridge/bin/build_prompt_config`

- Use this command when the current thread needs to build or refresh prompt artifacts in this workspace.
- Before running it, decide whether the current Codex thread already has enough information.
- If information is still missing, ask follow-up questions and do not run the tool.
- If information is sufficient:
  1. Write `./.threadbridge/tool_requests/build_prompt_config.request.json`.
  2. Run `./.threadbridge/bin/build_prompt_config`.
  3. Inspect `./.threadbridge/tool_results/build_prompt_config.result.json`.

The request file must look like this:

```json
{
  "concept": {
    "concept_id": "c_001",
    "title": "Short concept title",
    "summary": "One concise paragraph for the current thread brief.",
    "keywords": ["keyword 1", "keyword 2"],
    "style_notes": ["style note 1", "style note 2"],
    "constraints": ["constraint 1", "constraint 2"],
    "source": "buildpromptconfig",
    "updated_at": "2026-03-16T00:00:00.000Z"
  },
  "prompt": {
    "concept_id": "c_001",
    "variant_id": "primary",
    "provider": "nanobanana",
    "mode": "text_to_image",
    "instruction": "A complete final instruction for Nanobanana.",
    "image_inputs": [],
    "generation_config": {
      "response_modalities": ["IMAGE"],
      "image_config": {
        "aspect_ratio": "1:1",
        "image_size": "1K"
      }
    },
    "safety_settings": [
      {
        "category": "HARM_CATEGORY_HATE_SPEECH",
        "threshold": "BLOCK_MEDIUM_AND_ABOVE"
      }
    ],
    "metadata": {
      "source": "buildpromptconfig",
      "timestamp": "2026-03-16T00:00:00.000Z",
      "notes": ["Short implementation note."]
    }
  }
}
```

### Prompt Guide For `instruction`

- Build the final Nanobanana `instruction` from the current session and any workspace-local images.
- For text-to-image requests, structure the instruction around:
  - `Subject + Action + Setting + Style + Composition + Lighting + Key details + Constraints`
- For image-edit requests, structure the instruction around:
  - `Keep + Change + How/Style + Constraints`
- The `instruction` must be a final provider-ready prompt, not a question to the user.
- Do not invent diffusion-style fields or hidden model settings.
- Only reference source images that actually exist in this workspace.

### `./.threadbridge/bin/generate_image`

- Use this command when the current thread needs to generate images from workspace artifacts.
- By default, use the latest prompt config in `prompts/` unless the session clearly requires another one.
- If the workspace still lacks a usable prompt config or required image inputs, ask follow-up questions and do not run the tool.
- After a successful run, inspect `./.threadbridge/tool_results/generate_image.result.json`.

### `./.threadbridge/bin/send_telegram_media`

- Use this command only when sending text, images, or files back into the current Telegram thread would materially help the user.
- This capability is optional. Ordinary chat replies still work without it.
- Before running it:
  1. Write `./.threadbridge/tool_requests/send_telegram_media.request.json`.
  2. Keep all referenced files inside the current workspace.
  3. Prefer concise captions and user-facing filenames that already exist on disk.
- Each item may include an optional `surface` field. The default is `content`.
- Telegram delivery does upload-size preflight before sending queued files.
- Oversized `photo` items may fall back to `document`; oversized files may fall back to a warning instead of a Telegram upload.
- After a successful run, inspect `./.threadbridge/tool_results/send_telegram_media.result.json`.
- The bot runtime will deliver queued items from the workspace outbox after the Codex turn completes.

The request file must look like this:

```json
{
  "items": [
    {
      "type": "text",
      "text": "Short user-facing message.",
      "surface": "content"
    },
    {
      "type": "photo",
      "path": "images/generated/005/example.png",
      "caption": "Optional caption."
    },
    {
      "type": "document",
      "path": "prompts/005_primary.json",
      "caption": "Optional caption."
    }
  ]
}
```

### Artifact Boundaries

- threadBridge-owned runtime surface inside this workspace:
  - `.threadbridge/bin/`
  - `.threadbridge/state/workspace-config.json`
  - `.threadbridge/state/app-server/`
  - `.threadbridge/state/runtime-observer/`
  - `.threadbridge/tool_requests/`
  - `.threadbridge/tool_results/`
- `runtime-observer/` remains a workspace-local observability/activity lane; it is not the machine-level owner authority.
- Workspace/project artifacts produced by the tools:
  - `concept.json`
  - `prompts/*.json`
  - `images/generated/`

### Implementation Discipline

- Keep ordinary chat behavior grounded in the current Codex thread and the actual artifacts on disk.
- Do not overwrite or redefine the rest of the workspace `AGENTS.md`.
- Do not reintroduce diffusion-style placeholder parameters for Nanobanana configs.
<!-- threadbridge:runtime:end -->
