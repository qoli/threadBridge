# Thread AGENTS.md Builder

Use this template when the Telegram bot asks you to update a child `AGENTS.md` for a specific thread.

## Goal

Write a concise, reusable `AGENTS.md` file for the current thread runtime root. This file should help future Codex turns stay aligned with the thread's direction, runtime topology, and workflow.

## Source of Truth

- Use the existing session context in the current Codex thread as the primary source of truth.
- Use any provided source-image paths only when they materially affect the thread's stable workflow or visual direction.
- Distill only reusable, stable instructions. Do not turn the file into a transcript recap.

## Writing Rules

- If the target `AGENTS.md` already exists, read it first and preserve any stable instructions that are still valid.
- Preserve the `## Workspace Runtime Contract` section exactly, including the embedded build prompt guide and the wrapper commands `./bin/build_prompt_config`, `./bin/generate_image`, and `./bin/send_telegram_media`.
- Write Markdown directly to the target `AGENTS.md` path.
- Rewrite the full file from the latest session context instead of patching it incrementally.
- Keep the file concise and operational. Prefer bullets over long prose.
- Write in English.
- Omit unknown preferences rather than inventing them.
- If the session still lacks enough stable information, ask the user follow-up questions in the thread and do not write or modify the file.
- Do not depend on repo-level templates or docs for normal thread operation.
- Explain the thread/runtime structure from the perspective of `data/<thread-key>/AGENTS.md`, not from the perspective of the bound repo.

## Required Sections

Your output must include these sections:

- `# Thread AGENTS.md`
- `## Runtime Topology`
- `## Workspace Runtime Contract`
- `## Thread Direction`
- `## Image & Reference Handling`
- `## Artifact Rules`
- `## Current Priorities`

## Section Intent

- `Runtime Topology`
  - Explain that this file lives in `data/<thread-key>/`.
  - Explain that `workspace/` is a symlink to the bound session `cwd`.
  - Explain that `state/` is the thread-local runtime state area.
- `Workspace Runtime Contract`
  - Preserve the fixed runtime contract, wrapper command names, and result-file expectations that make this workspace executable.
- `Thread Direction`
  - Capture the stable brief, material language, visual direction, and persistent style preferences for this thread.
- `Image & Reference Handling`
  - Capture how uploaded images, reference images, and follow-up text should be interpreted in this thread.
- `Artifact Rules`
  - Capture how `concept.json`, `prompts/*.json`, and related outputs should be shaped for this thread.
- `Current Priorities`
  - Capture the current active objective or the next decision that future turns should preserve.

## Scope Reminder

- This is a child `AGENTS.md` for one `data/<thread-key>/` runtime root.
- Keep the thread runtime self-contained without overwriting the bound repo's own `AGENTS.md`.
- Do not include generic contributor-guide content.
