# Workspace Runtime Skill Builder

Use this template only when maintainers intentionally rewrite the managed threadBridge runtime skill.

## Goal

Write a concise `SKILL.md` that can be installed under `.threadbridge/skills/threadbridge-runtime/` without modifying the project `AGENTS.md`.

## Rules

- Write for a real bound workspace, not for `data/<thread-key>/`.
- Preserve the runtime surface under `.threadbridge/`.
- Keep wrapper names stable:
  - `.threadbridge/bin/hcodex`
  - `.threadbridge/bin/build_prompt_config`
  - `.threadbridge/bin/generate_image`
  - `.threadbridge/bin/send_telegram_media`
- Keep request/result expectations under `.threadbridge/tool_requests/` and `.threadbridge/tool_results/`.
- Keep detailed schemas and examples in `references/` when they are not needed for every runtime task.
- Do not introduce `AGENTS.md` injection as part of ordinary workspace ensure or reconcile.
- Keep the skill operational, reusable, and scoped to threadBridge runtime behavior.
