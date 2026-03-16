# Workspace Appendix Builder

Use this template only when maintainers intentionally want to rewrite the managed threadBridge runtime appendix.

## Goal

Write a concise appendix block that can be appended to a real workspace `AGENTS.md` without overwriting the rest of that file.

## Rules

- Write for a real bound workspace, not for `data/<thread-key>/`.
- Preserve the runtime surface under `.threadbridge/`.
- Keep wrapper names stable:
  - `./.threadbridge/bin/build_prompt_config`
  - `./.threadbridge/bin/generate_image`
  - `./.threadbridge/bin/send_telegram_media`
- Keep request/result expectations under `.threadbridge/tool_requests/` and `.threadbridge/tool_results/`.
- Do not introduce Telegram-thread-local paths or symlink language.
- Do not rewrite the surrounding workspace `AGENTS.md`; only replace the managed appendix body.
- Keep the appendix concise, operational, and reusable.
