# threadBridge Runtime Tools

This reference belongs to the workspace-local `threadbridge-runtime` skill.

## build_prompt_config

Use `.threadbridge/bin/build_prompt_config` to materialize prompt artifacts from a prepared request at `.threadbridge/tool_requests/build_prompt_config.request.json`.

Expected outputs:
- `concept.json`
- `prompts/NNN_primary.json`
- `.threadbridge/tool_results/build_prompt_config.result.json`

The prompt `instruction` must be provider-ready. Do not write questions, markdown, or self-explanations inside it.

For text-to-image work, structure the instruction around subject, action, setting, style, composition, lighting, key details, and constraints.

For image edits, structure the instruction around what to keep, what to change, how/style, and constraints. Only reference source images that exist in the workspace.

## generate_image

Use `.threadbridge/bin/generate_image` after a usable prompt config exists. Inspect `.threadbridge/tool_results/generate_image.result.json` after the run.

Generated images are written under `images/generated/`.

## send_telegram_media

Use `.threadbridge/bin/send_telegram_media` after writing `.threadbridge/tool_requests/send_telegram_media.request.json`.

The runtime queues delivery through `.threadbridge/tool_results/telegram_outbox.json`; the bot sends queued items after the Codex turn completes.

Oversized photo items may fall back to document delivery. Oversized files may fall back to a warning instead of upload.
