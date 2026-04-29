# Workspace Runtime Skill

## Status

Implemented baseline: workspace runtime support installs a threadBridge skill under `.threadbridge/skills/threadbridge-runtime/` and no longer injects the runtime contract into project `AGENTS.md` during ordinary workspace ensure.

## Decision

threadBridge runtime behavior is represented as a workspace-local Codex skill instead of a managed `AGENTS.md` appendix.

The project repository remains authoritative for project instructions. threadBridge runtime capability documentation lives in generated runtime state under `.threadbridge/`, alongside the wrapper commands and request/result lanes that it describes.

## Runtime Surface

Runtime ensure writes:

- `.threadbridge/skills/threadbridge-runtime/SKILL.md`
- `.threadbridge/skills/threadbridge-runtime/references/runtime-tools.md`
- `.threadbridge/skills/threadbridge-runtime/references/build_prompt_config.request.schema.json`
- `.threadbridge/skills/threadbridge-runtime/references/send_telegram_media.request.schema.json`
- existing wrapper, state, request, and result directories under `.threadbridge/`

Runtime ensure does not create or update project `AGENTS.md`.

## Rebuild Boundary

Normal workspace ensure, resume, passive reconcile, and managed Codex preference sync must not opportunistically edit project `AGENTS.md`.

Legacy cleanup of prior managed `AGENTS.md` blocks belongs behind explicit runtime support rebuild/migration entrypoints. The cleanup scope must be limited to the managed marker range:

- `<!-- threadbridge:runtime:start -->`
- `<!-- threadbridge:runtime:end -->`

## Rationale

The previous appendix made every bound workspace carry a large runtime manual in the project instruction file. That increased Codex context load and polluted working repositories with threadBridge-specific operational detail.

A skill is weaker than a hard instruction contract, but it is a better fit for runtime capabilities:

- it is installed by threadBridge rather than authored by the project
- it can be updated with runtime support
- detailed schemas and examples can be loaded only when needed
- project `AGENTS.md` remains clean

The startup path should not compensate by adding repeated visible runtime hints to Codex turns. Discovery should be handled by runtime capability registration or by the workspace-local skill surface, not by transcript-visible reminders.
