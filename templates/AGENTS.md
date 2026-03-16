# threadBridge Workflow Contract

This workspace is a Codex-driven runtime for a Telegram thread.

## Core Model

- Treat this workspace as the stable working unit.
- Treat the linked Telegram thread as the current UI container for this workspace.
- Treat the linked Codex session as the current agent continuity for this workspace.
- Use the existing Codex session context as the source of truth. Do not rebuild long transcript replays unless a workflow explicitly requires it.
- Keep this runtime workspace-local. Do not rely on repo-level templates, docs, or implementation files for normal thread operation.

## Creative Thread Behavior

- Normal thread messages continue the current Codex session.
- For normal thread messages, resolve references like "above", "same format", or "continue that" from the active conversation first.
- Thread images are part of the same thread context.
- When a thread has a pending image batch, the next user text should usually be interpreted as an image-analysis request tied to that batch, not as an unrelated fresh prompt.
- If session continuity breaks, require reconnect instead of silently starting a replacement session.

## Workspace Runtime Contract

- The local wrapper commands in this workspace are:
  - `./bin/build_prompt_config`
  - `./bin/generate_image`
  - `./bin/send_telegram_media`
- Keep these wrapper command names stable.
- Do not remove this section when updating a child `AGENTS.md`.
- Treat this section as self-contained. The wrappers are the public runtime surface; their internal repo-level implementation is not part of the normal chat workflow.

### `./bin/build_prompt_config`

- Use this command when the user asks for `/build_prompt_config`.
- Before running it, decide from the current session whether there is enough information to build prompt artifacts.
- If information is still missing, ask follow-up questions in the thread and do not run the tool.
- If information is sufficient:
  1. Write `tool_requests/build_prompt_config.request.json`.
  2. Run `./bin/build_prompt_config`.
  3. Inspect `tool_results/build_prompt_config.result.json`.

#### Prompt Guide For `instruction`

- Build the final Nanobanana `instruction` from the current session and any workspace-local images.
- For text-to-image requests, structure the instruction around:
  - `Subject + Action + Setting + Style + Composition + Lighting + Key details + Constraints`
- For image-edit requests, structure the instruction around:
  - `Keep + Change + How/Style + Constraints`
- The `instruction` must be a final provider-ready prompt, not a question to the user.
- Do not invent diffusion-style fields or hidden model settings.
- Only reference source images that actually exist in this workspace.

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

### `./bin/generate_image`

- Use this command when the user asks for `/generate_image`.
- By default, use the latest prompt config in `prompts/` unless the session clearly requires another one.
- If the workspace still lacks a usable prompt config or required image inputs, ask follow-up questions in the thread and do not run the tool.
- After a successful run, inspect `tool_results/generate_image.result.json`.
- The wrapper is responsible for repo-level API details; do not surface those implementation details in ordinary chat.

### `./bin/send_telegram_media`

- Use this command only when sending text, images, or files back into the current Telegram thread would materially help the user.
- This capability is optional. Ordinary chat replies still work without it.
- Before running it:
  1. Write `tool_requests/send_telegram_media.request.json`.
  2. Keep all referenced files inside the current workspace.
  3. Prefer concise captions and user-facing filenames that already exist on disk.
- After a successful run, inspect `tool_results/send_telegram_media.result.json`.
- The bot runtime will deliver queued items from the workspace outbox to the current Telegram thread after the Codex turn completes.

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

## Artifact Workflow

- `concept.json` is the workspace brief.
- `prompts/*.json` are Nanobanana request configs derived from the session.
- `tool_results/*.json` stores wrapper result metadata for the bot runtime.
- `tool_results/telegram_outbox.json` stores queued Telegram UI items waiting for bot delivery.
- `images/source/` stores user-provided source images.
- `images/analysis/` stores image-analysis artifacts.
- `images/generated/` stores image-generation runs, request payloads, response payloads, and output images.

## Implementation Discipline

- Keep ordinary thread behavior grounded in the current session and workspace-local artifacts.
- Do not reintroduce diffusion-style placeholder parameters for Nanobanana configs.
- Prefer concise, reusable workflow rules over per-turn chat summaries.
