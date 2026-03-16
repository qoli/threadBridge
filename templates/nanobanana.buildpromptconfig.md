# Nanobanana BuildPromptConfig Guide

Use this guide when the Telegram bot asks you to build prompt config artifacts for the current thread.

## Task

- Use the existing session context in the current Codex thread as the source of truth.
- Update or create `concept.json` for the thread.
- Create the next `prompts/NNN_primary.json` file for the thread.
- If the current session context is still insufficient, ask the user follow-up questions in the thread and do not write or modify the target files.

## References

- Provider request shape reference: `docs/callNanobanana.md`
- Prompt-writing reference: `https://nanobanana.io/prompt-guide?utm_source=chatgpt.com`

## Prompt Guide

### Text to Image

Build the final `instruction` with this structure whenever the thread is asking for a new image:

`Subject + Action + Setting + Style + Composition + Lighting + Key details + Constraints`

Write one cohesive final instruction for the provider. Do not ask questions inside the instruction. Do not include self-explanations or markdown.

### Image Edit

Build the final `instruction` with this structure whenever the thread is editing existing images:

`Keep + Change + How/Style + Constraints`

Only use `image_inputs` that the bot explicitly provided for this run.

## Output Rules

- `concept.json` is a concise brief for the thread, not a dump of fake model parameters.
- `prompts/NNN_primary.json` is a Nanobanana-specific request config.
- Keep `provider` as `nanobanana`.
- `mode` must be either `text_to_image` or `image_edit`.
- Use `image_edit` only when the request depends on provided source images.
- Use `text_to_image` when no source image is needed.
- `generation_config` should follow the Nanobanana request shape.
- `safety_settings` should follow the provider request shape.
- Do not invent diffusion-style fields such as `model`, `seed`, `steps`, `guidance`, `sampler`, `negative_prompt`, or `style_strength`.
- Write plain JSON files only.

## JSON Example Structure

The following examples show the required JSON shape. They are examples of structure, not fixed values.

### concept.json

```json
{
  "concept_id": "c_001",
  "title": "Short concept title",
  "summary": "One concise paragraph that captures the thread's current intent.",
  "keywords": ["keyword 1", "keyword 2"],
  "style_notes": ["style note 1", "style note 2"],
  "constraints": ["constraint 1", "constraint 2"],
  "source": "buildpromptconfig",
  "updated_at": "2026-03-16T00:00:00.000Z"
}
```

### prompts/NNN_primary.json

```json
{
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
    "notes": ["Optional implementation note or assumption."]
  }
}
```
