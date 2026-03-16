# threadBridge Thread Runtime

This file is the thread-local runtime contract for one Telegram thread.

## Core Model

- Treat this file as the authoritative thread-level instruction surface.
- The current working directory may be a bound project workspace with its own project instructions. Follow both layers without overwriting project-local conventions.
- Use the existing Codex session context as the main source of truth. Do not rebuild long transcript replays unless a workflow explicitly requires it.
- Keep thread-specific control state separate from the bound project unless a tool contract explicitly writes into the workspace.

## Runtime Topology

- This file lives at `data/<thread-key>/AGENTS.md`.
- `data/<thread-key>/workspace` is a symlink to the bound Codex session `cwd`.
- The bot/runtime may execute Codex with the bound workspace as the current working directory even though this file lives one level above it.
- `data/<thread-key>/state/` is the thread-local runtime state area for bot-owned artifacts such as pending image batches and image-analysis inputs/results.
- When the runtime tells you to read this file, treat it as the thread-specific operating contract. Do not rewrite or replace the bound project's own `AGENTS.md`.

## Conversation Behavior

- Normal thread messages continue the current Codex session.
- Resolve references like "above", "same format", or "continue that" from the active conversation first.
- If session continuity breaks, require reconnect instead of silently starting a replacement session.
- Treat uploaded thread images as part of the same thread context.
- When a thread has a pending image batch, the next user text usually clarifies or triggers analysis for that batch rather than starting an unrelated fresh task.

## Workspace Runtime Contract

- When your current working directory is the bound workspace, the local wrapper commands are:
  - `./bin/build_prompt_config`
  - `./bin/generate_image`
  - `./bin/send_telegram_media`
- Keep these wrapper command names stable.
- Do not remove this section when updating a child `AGENTS.md`.
- Treat this section as self-contained. The wrappers are the public runtime surface; their repo-level implementation details are not part of the normal chat workflow.

### `./bin/build_prompt_config`

- Use this command when the current thread needs to build or refresh prompt artifacts in the bound workspace.
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
- Only reference source images that actually exist in the bound workspace.

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

- Use this command when the current thread needs to generate images from the current workspace artifacts.
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

## Artifact Boundaries

- Thread root owns:
  - `metadata.json`
  - `conversations.jsonl`
  - `session-binding.json`
  - `state/`
- Bound workspace owns:
  - `bin/`
  - `tool_requests/`
  - `tool_results/`
  - `concept.json`
  - `prompts/*.json`
  - `images/generated/`
- Thread runtime state owns:
  - `state/pending-image-batch.json`
  - `state/images/source/`
  - `state/images/analysis/`

## Implementation Discipline

- Keep ordinary thread behavior grounded in the current session and the actual artifacts on disk.
- Do not overwrite or redefine project-local instructions in the bound workspace.
- Do not reintroduce diffusion-style placeholder parameters for Nanobanana configs.
- Prefer concise, reusable workflow rules over per-turn chat summaries.
