# Repository Guidelines

## Purpose
This root `AGENTS.md` is the maintainer guide for the `threadBridge` repository. It documents the repo layout, runtime boundaries, workspace lifecycle, and contributor conventions for the Telegram bot and its Codex-session binding runtime.

It is not the thread-local runtime instruction file that Codex follows for one Telegram thread. That runtime contract lives in the seeded and generated thread-level `AGENTS.md` files described below.

## Project Structure & Runtime Architecture
The runtime is organized in three layers:

- Telegram UI and orchestration: the Rust bot receives Telegram updates, enforces authorization, manages thread commands, streams live Codex previews, and sends results back to Telegram.
- Workspace agent runtime by Codex CLI: the Rust runtime layer maps each Telegram thread to bot-local metadata under `data/`, seeds `data/<thread-key>/AGENTS.md`, binds the thread to an existing Codex session from `~/.codex`, projects the session `cwd` into `data/<thread-key>/workspace`, and invokes Codex CLI inside that linked workspace while explicitly telling Codex to read the thread-level `AGENTS.md`.
- Tool executors: workspace-local wrapper commands call Python scripts in `tools/` to materialize prompt configs and generated image artifacts.

Important repo areas:

- `rust/src/bin/threadbridge.rs`: Telegram bot entrypoint and command handling for `/new_thread`, `/list_sessions`, `/bind_session`, image analysis, archive, restore, and reconnect flows.
- `rust/src/codex.rs`: Codex CLI wrapper that starts or resumes sessions with `codex exec`, streams JSON events, and exposes the maintainer-facing runtime operations.
- `rust/src/codex_home.rs`: integration layer that reads Codex session metadata from the local `~/.codex` home.
- `rust/src/workspace.rs`: runtime bootstrap logic that seeds thread-root `AGENTS.md`, creates workspace-local wrapper scripts in `bin/`, and keeps the runtime contract section in sync.
- `rust/src/repository.rs`: persistent thread state for metadata, transcripts, session bindings, pending image batches, and analysis artifacts.
- `templates/`: seed assets used to initialize or maintain workspaces. `templates/AGENTS.md` is the active seed runtime contract. Other template files here are maintainer-side assets, not the primary runtime dependency.
- `tools/`: Python executors invoked from workspace-local wrappers. `build_prompt_config.py` writes prompt artifacts; `generate_image.py` calls the image provider and stores generated outputs.
- `data/`: runtime state. `data/main-thread/` stores the control console state. Each thread maps to `data/<thread-key>/`.
- `docs/`: supplemental notes such as provider-specific documentation. These files support maintenance but are not the normal workspace runtime surface.

Treat `target/` and most of `data/` as generated output.

## AGENTS.md Roles
There are three distinct `AGENTS.md` roles in this repo. Do not conflate them when maintaining the system.

- Root `AGENTS.md`: this file. It explains how to maintain the repository and how the runtime is structured.
- `templates/AGENTS.md`: the seed template copied into each new thread runtime root. It defines the thread runtime contract, including the workspace symlink model and the stable wrapper command names.
- `data/<thread-key>/AGENTS.md`: the child, thread-local runtime instruction file used by Codex for one Telegram thread. `/new_thread` seeds it from the template, and later maintenance should update it through repo-side template or workspace-generation changes rather than Telegram slash commands.

When updating maintainer docs, describe the runtime behavior from the repo perspective. When updating workspace behavior, change the template or the child-workspace generation flow instead.

## Workspace Lifecycle & Data Flow
The operational flow is: Telegram thread -> Rust bot -> Codex workspace runtime -> workspace-local wrapper tools -> workspace artifacts -> Telegram reply.

From a maintainer perspective, the lifecycle is:

- `/new_thread` creates a Telegram topic and a bot-local metadata folder under `data/<thread-key>/`.
- `/list_sessions` reads recent Codex sessions from the local `~/.codex` home.
- `/bind_session` attaches a Telegram thread to an existing Codex session and ensures `data/<thread-key>/workspace` points to that session `cwd`.
- Normal thread messages append to the thread transcript, explicitly tell Codex to read `data/<thread-key>/AGENTS.md`, and then run Codex in the bound workspace directory. The bot resumes the saved Codex thread when possible instead of replaying long transcripts.
- Uploaded images are stored under `data/<thread-key>/state/`, accumulated into a pending batch, and then analyzed by Codex vision in the same bound workspace session context when the user triggers analysis or sends a follow-up text.
- If Codex session continuity breaks, the bot marks that binding as disconnected and requires `/reconnect_codex` or `/bind_session` instead of silently creating a replacement session.

## Workspace Artifacts & Ownership
Maintain these ownership boundaries because they define the runtime contract:

- Rust bot and repository layer own thread state such as `metadata.json`, `conversations.jsonl`, `session-binding.json`, `state/pending-image-batch.json`, `state/images/source/`, and `state/images/analysis/`.
- Workspace bootstrap logic owns the presence of `data/<thread-key>/AGENTS.md`, the `workspace` symlink, and workspace-local wrapper surfaces such as `bin/` and `tool_requests/`.
- `tools/build_prompt_config.py` owns `concept.json`, append-only `prompts/*.json`, and `tool_results/build_prompt_config.result.json`.
- `tools/generate_image.py` owns `tool_results/generate_image.result.json` and the generated run folders under `images/generated/`, including request payloads, response payloads, and output images.
- Telegram is the UI surface only. It should not become the source of truth for workspace artifacts or Codex session continuity.

When adding new workspace features, decide first which layer owns the artifact and which layer merely presents it.

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

`cargo run --bin threadbridge` starts the Telegram bot. `cargo check` is the fastest correctness pass. `cargo test` runs the inline unit tests. `cargo fmt` and `cargo clippy` use standard Rust tooling; run them before opening a PR.

## Coding Style & Naming Conventions
Follow `rustfmt` defaults for Rust: 4-space indentation, `snake_case` for functions and modules, `PascalCase` for types, and small focused modules. Match the existing style in `rust/src/` by returning `anyhow::Result`, using `serde`-friendly structs, and keeping async I/O in Tokio-aware helpers.

Python tools in `tools/` also use 4-space indentation, explicit helper functions, and type hints where useful. Keep scripts stdlib-first unless a dependency is already justified elsewhere.

When changing runtime behavior, keep the separation between Telegram orchestration, Codex workspace control, and tool execution clear in both code and documentation.

## Testing Guidelines
Tests currently live inline under `#[cfg(test)]` blocks in the Rust modules, for example in `rust/src/codex.rs` and `rust/src/repository.rs`. Prefer `#[tokio::test]` for async paths and descriptive test names like `pending_image_batch_roundtrip`.

Add or update tests when behavior changes in:

- repository persistence and workspace state transitions
- Codex CLI argument building and session-handling behavior
- workspace bootstrapping and wrapper generation
- tool-result parsing and artifact path handling

## Commit & Pull Request Guidelines
Use short imperative commit subjects. Conventional Commit style with a scope is a good default, for example `feat(threadbridge): add session binding`.

Pull requests should explain the user-visible behavior change, note any config or data migration impact, link the related issue or thread, and include screenshots or log snippets when changing Telegram flows, previews, or generated workspace artifacts.

## Security & Configuration Tips
Keep secrets in `.env.local`; never commit real tokens. Start from `.env.example`, and avoid checking generated workspace files, debug logs, or image outputs into Git unless they are intentional fixtures.

Treat workspace-local generated files under `data/` as potentially sensitive because they can contain prompts, summaries, transcripts, image references, provider payloads, and output metadata.

For deployment examples in this repository, use placeholders and environment variables only. Do not add personal hosts, production paths, real Telegram bot tokens, provider API keys, cookies, or session captures to source control.
