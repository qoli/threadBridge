# Repository Guidelines

## Purpose
This root `AGENTS.md` is the maintainer guide for `threadBridge`. It documents the repo layout, runtime boundaries, workspace lifecycle, and contributor conventions for the Telegram bot and its Codex app-server integration.

It is not the runtime appendix followed inside a bound project workspace. That appendix lives in [templates/AGENTS.md](/Volumes/Data/Github/threadBridge/templates/AGENTS.md) and is appended into a workspace `AGENTS.md` by the runtime bootstrap.

## Project Structure & Runtime Architecture
The runtime is organized in three layers:

- Telegram orchestration: the Rust bot receives Telegram updates, enforces authorization, manages thread commands, streams live Codex previews, and sends results back to Telegram.
- Codex thread control: the Rust runtime maps each Telegram thread to bot-local metadata under `data/`, binds it to a real workspace path, starts workspace-scoped shared `codex app-server` daemons on loopback websocket, resumes Codex threads through that shared runtime, and validates thread `cwd` against the stored workspace binding.
- Tool executors: workspace-local wrapper commands under `.threadbridge/bin/` call Python scripts in `tools/` to materialize prompt configs, generated images, and Telegram outbox payloads.

Important repo areas:

- `rust/src/bin/threadbridge.rs`: Telegram bot entrypoint.
- `rust/src/codex.rs`: app-server JSON-RPC client, thread lifecycle helpers, and event normalization for previews.
- `rust/src/workspace.rs`: workspace bootstrap logic that appends the managed runtime block into a real workspace `AGENTS.md` and installs `.threadbridge/`.
- `rust/src/repository.rs`: persistent bot-local thread state for metadata, transcripts, session bindings, and image-state artifacts.
- `rust/src/telegram_runtime/`: Telegram command handling, message flows, image handling, restore UI, and preview rendering.
- `templates/AGENTS.md`: managed runtime appendix appended to real workspace `AGENTS.md` files.
- `tools/`: Python executors invoked from `.threadbridge/bin/*`.
- `data/`: bot-local runtime state. `data/main-thread/` stores the control console state. Each thread maps to `data/<thread-key>/`.

Treat `target/` and most of `data/` as generated output.

## AGENTS.md Roles
There are two relevant `AGENTS.md` roles now:

- Root `AGENTS.md`: this maintainer guide.
- `templates/AGENTS.md`: the workspace runtime appendix managed by threadBridge and appended into a real bound workspace `AGENTS.md`.

There is no thread-local `data/<thread-key>/AGENTS.md` runtime surface anymore.

## Workspace Lifecycle & Data Flow
The operational flow is: Telegram thread -> Rust bot -> Codex app-server thread -> real workspace runtime -> Python tool wrappers -> Telegram reply.

From a maintainer perspective:

- `/new_thread` creates a Telegram topic and a bot-local folder under `data/<thread-key>/`.
- `/bind_workspace <absolute-path>` installs the runtime appendix and `.threadbridge/` surface into that real workspace, then starts a fresh Codex thread for that workspace through app-server.
- `session-binding.json` stores the mapping between the Telegram thread, the real workspace path, and the current Codex `thread.id`.
- Normal thread messages resume the saved Codex thread through app-server and run turns in the bound workspace.
- Uploaded images are stored under `data/<thread-key>/state/`, accumulated into a pending batch, and analyzed by Codex in the same bound workspace context.
- If Codex session continuity breaks or the returned `thread.cwd` no longer matches the stored workspace path, the binding is marked broken and requires `/reconnect_codex` or `/new`.
- `/restore_thread` is Telegram/local-state only. It restores an archived Telegram topic and local metadata; it does not recreate Codex continuity by itself.

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
  - `.threadbridge/tool_requests/`
  - `.threadbridge/tool_results/`
- `tools/build_prompt_config.py` owns `concept.json`, append-only `prompts/*.json`, and `.threadbridge/tool_results/build_prompt_config.result.json`.
- `tools/generate_image.py` owns `.threadbridge/tool_results/generate_image.result.json` and the generated run folders under `images/generated/`.
- `tools/send_telegram_media.py` owns `.threadbridge/tool_results/send_telegram_media.result.json` and `.threadbridge/tool_results/telegram_outbox.json`.

When adding new features, decide first which layer owns the artifact and which layer merely presents it.

## Build, Test, and Development Commands
Use the repo-local Cargo paths from the README:

```bash
export CARGO_HOME="$PWD/.cargo" CARGO_TARGET_DIR="$PWD/target"
cargo run --bin threadbridge
cargo check
cargo test
cargo fmt
cargo clippy --all-targets --all-features
```

`cargo run --bin threadbridge` starts the Telegram bot. `cargo check` is the fastest correctness pass. `cargo test` runs the Rust unit tests. `cargo fmt` and `cargo clippy` use standard Rust tooling.

## Coding Style & Naming Conventions
Follow `rustfmt` defaults for Rust: 4-space indentation, `snake_case` for functions and modules, `PascalCase` for types, and small focused modules. Match the existing style in `rust/src/` by returning `anyhow::Result`, using `serde`-friendly structs, and keeping async I/O in Tokio-aware helpers.

Python tools in `tools/` use 4-space indentation, explicit helper functions, and stdlib-first implementations unless a dependency is already justified elsewhere.

When changing runtime behavior, keep the separation between Telegram orchestration, Codex thread control, and tool execution clear in both code and documentation.

## Testing Guidelines
Tests live inline under `#[cfg(test)]` blocks in the Rust modules. Prefer `#[tokio::test]` for async paths and descriptive test names.

Add or update tests when behavior changes in:

- repository persistence and state transitions
- app-server request/response handling
- workspace bootstrap and appendix generation
- tool-result parsing and artifact path handling

## Commit & Pull Request Guidelines
Use short imperative commit subjects. Conventional Commit style with a scope is a good default, for example `feat(threadbridge): add workspace binding`.

Pull requests should explain the user-visible behavior change, note any config or data migration impact, link the related issue or thread, and include screenshots or log snippets when changing Telegram flows or generated artifacts.

## Security & Configuration Tips
Keep secrets in `.env.local`; never commit real tokens. Start from `.env.example`, and avoid checking generated workspace files, debug logs, or image outputs into Git unless they are intentional fixtures.

Treat bot-local `data/` and workspace-local generated files as potentially sensitive because they can contain prompts, transcripts, image references, provider payloads, and output metadata.

<!-- threadbridge:runtime:start -->
## threadBridge Runtime Appendix

This managed block is appended by threadBridge to a real project workspace `AGENTS.md`.

### Runtime Model

- The current working directory is the real bound workspace, not a projected copy.
- Preserve this workspace's own conventions and instructions. This appendix adds bot/runtime behavior; it does not replace project-local rules.
- threadBridge tracks Telegram-thread metadata outside the workspace under its own `data/` store. That bot-local state is not the source of truth for project files.
- Use the current Codex thread context as the primary continuity source. Do not rebuild long transcript replays unless a workflow explicitly requires it.

### Runtime Surface

- threadBridge installs wrapper commands under:
  - `./.threadbridge/bin/build_prompt_config`
  - `./.threadbridge/bin/codex_sync_event`
  - `./.threadbridge/bin/codex_sync_notify`
  - `./.threadbridge/bin/generate_image`
  - `./.threadbridge/bin/send_telegram_media`
- threadBridge installs local shell/runtime sync files under:
  - `./.threadbridge/shell/codex-sync.bash`
  - `./.threadbridge/state/app-server/current.json`
  - `./.threadbridge/state/codex-sync/current.json`
  - `./.threadbridge/state/codex-sync/events.jsonl`
  - `./.codex/hooks.json`
- threadBridge request/result files live under:
  - `./.threadbridge/tool_requests/`
  - `./.threadbridge/tool_results/`
- Keep these wrapper names and paths stable.

### Local Codex TUI

- Source `./.threadbridge/shell/codex-sync.bash` before using `hcodex` in this workspace.
- `hcodex` resolves the shared workspace daemon from `./.threadbridge/state/app-server/current.json` and launches `codex --remote ...`.
- With no extra args, `hcodex` resumes the current Telegram-bound Codex thread for this workspace.
- The managed `.codex/hooks.json` and `codex-sync` state files are still installed as compatibility surfaces during the migration, but viewer handoff and `/attach_cli_session` are no longer part of the supported path.

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
- After a successful run, inspect `./.threadbridge/tool_results/send_telegram_media.result.json`.
- The bot runtime will deliver queued items from the workspace outbox after the Codex turn completes.

The request file must look like this:

```json
{
  "items": [
    {
      "type": "text",
      "text": "Short user-facing message."
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
  - `.threadbridge/shell/`
  - `.threadbridge/state/app-server/`
  - `.threadbridge/state/codex-sync/`
  - `.threadbridge/tool_requests/`
  - `.threadbridge/tool_results/`
  - `.codex/hooks.json`
- Workspace/project artifacts produced by the tools:
  - `concept.json`
  - `prompts/*.json`
  - `images/generated/`

### Implementation Discipline

- Keep ordinary chat behavior grounded in the current Codex thread and the actual artifacts on disk.
- Do not overwrite or redefine the rest of the workspace `AGENTS.md`.
- Do not reintroduce diffusion-style placeholder parameters for Nanobanana configs.
<!-- threadbridge:runtime:end -->
