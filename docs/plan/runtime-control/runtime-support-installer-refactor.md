# Runtime Support Installer Refactor

## Status

Draft.

This document captures the architecture debt exposed by the workspace runtime skill migration. The explicit legacy `AGENTS.md` cleanup path has landed through the bundled desktop `Rebuild Runtime Support` action, but the broader installer-manifest refactor is not implemented yet.

## Problem

Changing the workspace runtime documentation surface from an injected `AGENTS.md` appendix to `.threadbridge/skills/threadbridge-runtime/SKILL.md` exposed through `.codex/skills/threadbridge-runtime` required edits across configuration, runtime support validation, workspace bootstrap, owner reconcile, runtime control, management API tests, Telegram status tests, maintainer docs, and plan docs.

That blast radius is too large for a runtime artifact shape change.

The current implementation has three coupled responsibilities:

- installed runtime support seed discovery
- workspace-local runtime surface installation
- legacy workspace migration behavior

Those responsibilities should be separate. A normal runtime ensure should install the current runtime surface. Runtime support rebuild should refresh installed seed files. Legacy migration should be explicit and bounded.

## Current Coupling

Current code paths still treat a specific runtime artifact path as a cross-layer concern:

- `RuntimeConfig` exposes a concrete template path.
- `RuntimeControlContext`, `DesktopRuntimeOwner`, and management actions carry that path into workspace ensure.
- `workspace.rs` decides how runtime support files are materialized.
- tests must create the same seed layout even when the test is not about runtime documentation.

The result is that adding or moving one workspace artifact requires unrelated layers to know about the file name and layout.

## Target Shape

Introduce a runtime support manifest that describes workspace artifacts declaratively.

Conceptual shape:

```rust
struct RuntimeSupportManifest {
    workspace_artifacts: Vec<WorkspaceRuntimeArtifact>,
}

struct WorkspaceRuntimeArtifact {
    source: RuntimeSupportSource,
    destination: WorkspaceRuntimeDestination,
    install_mode: InstallMode,
    permission: Option<u32>,
    owner: RuntimeArtifactOwner,
}

enum InstallMode {
    WriteIfChanged,
    CopyFileIfChanged,
    CopyDirectoryTreeIfChanged,
    Generate,
}
```

The manifest should express facts such as:

- copy `templates/threadbridge-runtime-skill/` to `.threadbridge/skills/threadbridge-runtime/`
- repair `.codex/skills/threadbridge-runtime` symlink to the threadBridge-owned skill directory
- generate `.threadbridge/bin/hcodex`
- generate `.threadbridge/bin/build_prompt_config`
- create `.threadbridge/tool_requests/`
- create `.threadbridge/tool_results/`
- optionally copy managed Codex binary when source preference requires it

The workspace installer should consume the manifest. Higher layers should not know whether the runtime documentation artifact is an `AGENTS.md` appendix, a skill directory, or a future MCP capability descriptor.

## Proposed Module Boundaries

### `runtime_support`

Owns installed seed support:

- validate seed support exists
- rebuild installed support from bundled seed
- load or construct the runtime support manifest
- expose only stable runtime support roots and manifest views

It should not know which active workspaces exist.

### `workspace_runtime_installer`

Owns workspace materialization:

- create runtime directories
- install manifest-described files and trees
- generate wrapper scripts
- generate `hcodex`
- sync managed Codex binary when applicable
- emit telemetry about files written, skipped, and mode changes

It should not decide session binding, Telegram ownership, or runtime health.

### `runtime_control`

Owns workspace lifecycle orchestration:

- call workspace runtime installer
- bind, repair, or create Codex sessions
- route Telegram input to live TUI when appropriate

It should pass an installer dependency or manifest, not concrete template file paths.

### Migration Service

Owns explicit legacy cleanup:

- remove only managed `AGENTS.md` marker blocks
- report exactly what was changed
- run only through explicit rebuild/migration entrypoints

Normal ensure, resume, passive reconcile, and managed Codex preference sync must not perform legacy cleanup.

## Phased Plan

### Phase 1: Installer Extraction

Split `ensure_workspace_runtime` into narrow helpers:

- `ensure_runtime_dirs`
- `install_runtime_skill`
- `install_tool_wrappers`
- `install_hcodex_launcher`
- `sync_managed_codex`
- `ensure_workspace_state`

This phase should preserve behavior and reduce the size of `workspace.rs`.

### Phase 2: Manifest Introduction

Introduce a manifest type for static copy/write artifacts:

- skill directory
- reference files
- request/result directories
- `.gitignore`

Keep generated scripts as explicit installer steps until the manifest supports generators cleanly.

### Phase 3: Runtime Support Path Isolation

Replace `validate_seed_template`-style APIs with manifest validation.

Callers should ask for validated runtime support, not for one specific template path.

### Phase 4: Explicit Legacy Migration

Add a migration operation for old managed `AGENTS.md` blocks.

Current landed slice: bundled desktop `Rebuild Runtime Support` rebuilds installed runtime support, re-syncs active bound workspace runtime surfaces, repairs the Codex repo-skill symlink, and then runs a bounded cleanup over active bound workspaces.

Rules:

- never run during ordinary reconcile
- only remove text between the managed start/end markers
- preserve surrounding project content exactly
- report `removed`, `not_found`, or `ambiguous_markers`

### Phase 5: Test Fixtures

Add a shared `WorkspaceRuntimeFixture` for tests that need runtime state.

The fixture should make hidden dependencies explicit:

- workspace path
- `.threadbridge/state/app-server/current.json` presence or absence
- session status files
- local TUI claim
- worker endpoint behavior

This should reduce unrelated failures such as tests that panic with `workspace runtime state is unavailable` when they only intended to test thread state interpretation.

## Non-Goals

- Do not redesign the app-server protocol in this refactor.
- Do not introduce MCP as part of this installer refactor.
- Do not change Telegram command semantics.
- Runtime support rebuild may scan active bound workspaces only as the explicit migration UX, and it must remove only the managed `AGENTS.md` marker range.

## Success Criteria

- Moving runtime documentation from one artifact type to another changes a manifest or one installer module, not owner/control/management layers.
- Ordinary workspace ensure never edits project `AGENTS.md`.
- Runtime support rebuild and workspace legacy migration stay separate operations in code ownership; the bundled tray action may orchestrate both after user confirmation.
- Tests that need workspace runtime state use a fixture instead of relying on implicit setup.
- `workspace.rs` becomes orchestration over focused installer helpers rather than one large mixed-responsibility function.
