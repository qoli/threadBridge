# Qwen-Image-2512 Fluent Office f1 + f2 Icon Exploration

This exploration pack uses `Qwen-Image-2512` to discover a Fluent-style Office product icon based on:

- `icon/candidates/f2.png` as the dominant main body reference
- `icon/candidates/f1.png` as a lower-right corner badge reference

## Goal

Produce a Microsoft 365 / Fluent-like product icon where:

- the first read is a large `f2`-derived central product body
- the second read is a smaller `f1`-derived lower-right badge
- the result reads as one Office-family product icon, not as two unrelated logos combined

## Locked Direction

- icon family: `product icon`
- `f1` role: `corner badge`
- `f1` position: `lower-right`
- `f2` fidelity: `large reconstruction allowed`
- `Qwen-Image-2512` usage: textual shape translation, not direct image conditioning

## Files

- `icon/exploration/qwen-image-2512/fluent-office/prompts.csv`
  - canonical prompt registry for this exploration
- `icon/exploration/qwen-image-2512/fluent-office/scorecard-template.csv`
  - lightweight evaluation sheet
- `scripts/run_qwen_image_2512_icon_exploration.py`
  - local runner for `QwenImageCLI`

## Runner Defaults

The runner assumes the local environment already has:

- model: `/Volumes/Data/qwen-image/Qwen-Image-2512`
- Lightning LoRA: `/Volumes/Data/qwen-image/Qwen-Image-2512-Lightning/Qwen-Image-2512-Lightning-4steps-V1.0-bf16.safetensors`
- CLI: `/Volumes/Data/Github/qwen.image.swift/.build/xcode/Build/Products/Release/QwenImageCLI`

For fast exploration, the runner uses the `lightning` profile:

- `4` steps
- guidance `1.0`
- true CFG scale `1.0`
- `1024x1024`
- `--gpu-cache-limit 16gb`
- `--clear-cache-between-stages`

## Usage

List prompt ids:

```bash
python3 scripts/run_qwen_image_2512_icon_exploration.py --list-prompts
```

Run the full first round:

```bash
python3 scripts/run_qwen_image_2512_icon_exploration.py \
  --series fluent_office_f1_f2 \
  --seeds 41,42,43
```

Run one prompt only:

```bash
python3 scripts/run_qwen_image_2512_icon_exploration.py \
  --prompt-id round1_detached_corner_badge \
  --seeds 41,42,43
```

Artifacts are written under `tmp/qwen-image-2512-icon-exploration/<timestamp>/`.

Each prompt run includes:

- `prompt.txt`
- `prompt_row.csv`
- `prompt_row.json`
- one subdirectory per seed
- `contact_sheet.png`
- `index.json`

## Evaluation

Reject a result if it:

- reads as two equal logos instead of main body + badge
- loses `f2` as the first read
- turns `f1` into a sticker or pasted overlay
- stops looking like a Fluent / Office product icon
- introduces text or extra symbols

Keep a result if it:

- makes the central `f2`-derived body dominant
- makes the lower-right `f1` badge clearly secondary
- preserves clean product-icon hierarchy at `64x64` and `32x32`
- feels like one Office-family icon rather than a collage
