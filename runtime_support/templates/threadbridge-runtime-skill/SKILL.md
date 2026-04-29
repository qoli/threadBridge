---
name: threadbridge-runtime
description: Use when working with threadBridge-managed workspace runtime, .threadbridge tools, Telegram media delivery, image generation, prompt config artifacts, hcodex, runtime status, or session binding repair.
---

# threadBridge Runtime

This skill describes workspace-local threadBridge capabilities. Project instructions from the workspace remain authoritative for project work; use this skill only for threadBridge runtime operations.

## Runtime Surface

Workspace-local runtime files live under `.threadbridge/`.

Stable wrappers:
- `.threadbridge/bin/hcodex`
- `.threadbridge/bin/build_prompt_config`
- `.threadbridge/bin/generate_image`
- `.threadbridge/bin/send_telegram_media`

Request files live under `.threadbridge/tool_requests/`. Result files live under `.threadbridge/tool_results/`.

`runtime-observer/` is a local activity surface. Desktop owner and management/runtime protocol views remain the runtime-health authority.

## Local Codex TUI

Use `.threadbridge/bin/hcodex` for the managed local TUI path in this workspace.

Use `hcodex resume <session-id>` only when explicitly continuing an existing Codex session.

## Prompt Config

Use `.threadbridge/bin/build_prompt_config` only when the current thread needs prompt artifacts.

Before running it:
- Decide whether the current Codex thread already has enough information.
- If information is missing, ask follow-up questions and do not run the tool.
- If information is sufficient, write `.threadbridge/tool_requests/build_prompt_config.request.json`, run the wrapper, then inspect `.threadbridge/tool_results/build_prompt_config.result.json`.

Read `references/build_prompt_config.request.schema.json` before writing the request.

## Image Generation

Use `.threadbridge/bin/generate_image` when the thread needs image generation from workspace artifacts.

By default, use the latest prompt config in `prompts/` unless the session clearly requires another one. If required prompt config or image inputs are missing, ask follow-up questions and do not run the tool.

After a run, inspect `.threadbridge/tool_results/generate_image.result.json`.

## Telegram Media

Use `.threadbridge/bin/send_telegram_media` only when sending text, images, or files back into the current Telegram thread materially helps the user.

Before running it:
- Write `.threadbridge/tool_requests/send_telegram_media.request.json`.
- Keep all referenced files inside the current workspace.
- Prefer concise captions and user-facing filenames that already exist on disk.

Read `references/send_telegram_media.request.schema.json` before writing the request.

## Artifact Boundaries

threadBridge-owned runtime surface inside this workspace:
- `.threadbridge/bin/`
- `.threadbridge/state/`
- `.threadbridge/skills/threadbridge-runtime/`
- `.threadbridge/tool_requests/`
- `.threadbridge/tool_results/`

Workspace/project artifacts produced by tools:
- `concept.json`
- `prompts/*.json`
- `images/generated/`

Do not overwrite project instructions or project files unless the current task explicitly requires it.
