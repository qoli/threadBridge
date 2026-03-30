# Z-Image-Turbo App Icon Exploration

This repo now includes a local exploration kit for evolving the macOS app icon with Ollama's `x/z-image-turbo:latest` image endpoint.

The exploration strategy still centers on direction exploration and refinement, but prompt storage is now CSV-driven:

1. prompts live in one global registry
2. runs execute exactly one `prompt_id`
3. iterative work appends new prompt rows instead of replaying an entire folder

The brand anchor is the current tray icon topology, not the current app icon surface finish. The tray icon is the three-point connected triangle symbol embedded in `rust/static/tray/point_3_filled_connected_trianglepath_dotted.rgba`, while the current app icon source remains `icon/round-04-no-tile-dark-bg-v1.png`.

## Why This Structure

The prompt pack follows the current `z-image-turbo` guidance:

- use long, specific prompts
- keep the icon centered and highly legible
- avoid negative-prompt driven iteration
- change the prompt meaningfully across directions rather than hoping seed variation will do the exploration
- when the model struggles with app-icon constraints, step back and solve the problem as `symbol design` first, then translate the winning symbol into a macOS app icon

References:

- https://huggingface.co/Tongyi-MAI/Z-Image-Turbo/discussions/8
- https://huggingface.co/spaces/Tongyi-MAI/Z-Image-Turbo/blob/main/pe.py
- https://ollama.com/x/z-image-turbo
- https://docs.ollama.com/api/openai-compatibility

## Adjacent Guidance

For later prompt work that moves from `z-image-turbo` exploration into `Qwen-Image-2512` app-icon generation, use the repo-local guidance here:

- `docs/qwen-image-2512-app-icon-prompting.md`

That document records the current working assumptions for `Qwen-Image-2512`, including:

- continuous paragraph prompts over tag piles
- concrete geometry language over abstract concept words
- explicit tile, material, lighting, and text constraints
- short negative prompts only for high-value artifact filtering

## Files

- `icon/exploration/z-image-turbo/prompts.csv`
  - canonical prompt registry used by the runner
- `icon/exploration/z-image-turbo/**/*.txt`
  - legacy prompt snapshots kept for reference during the migration
- `icon/exploration/z-image-turbo/scorecard-template.csv`
  - evaluation template for selecting the winner
- `scripts/run_z_image_turbo_icon_exploration.py`
  - single-prompt runner that calls Ollama's `/v1/images/generations`

## Inspect Prompt IDs

List the available prompt ids:

```bash
python3 scripts/run_z_image_turbo_icon_exploration.py --list-prompts
```

## Run One Prompt

Run a specific prompt by id:

```bash
python3 scripts/run_z_image_turbo_icon_exploration.py --prompt-id round4_p2_brand_loop
```

Artifacts are written under `tmp/z-image-turbo-icon-exploration/<timestamp>/`.

Each run gets:

- `prompt.txt`
- `prompt_row.csv`
- `prompt_row.json`
- `request.json`
- `response.json`
- `0001.png`, `0002.png`, ...

## Select The Winner

Use `icon/exploration/z-image-turbo/scorecard-template.csv` and score each candidate on:

- `tray_icon_relation`
- `shelf_impact`
- `small_size_legibility`
- `extensibility`

The intended weighting is:

- `40%` tray-icon relation
- `30%` shelf impact
- `20%` small-size legibility
- `10%` extensibility

Reject any candidate that reads as a generic Telegram clone, a generic paper-plane app, or a generic crypto badge.

## Add A New Iteration

Append a new row to `icon/exploration/z-image-turbo/prompts.csv` with:

- a new `prompt_id`
- the target `series`
- `round` and `parent_prompt_id` for iterative branches
- the final `prompt_text`

Then run that exact id:

```bash
python3 scripts/run_z_image_turbo_icon_exploration.py --prompt-id new_prompt_id
```

## Symbol-First Rebuild

When the app-icon prompts become too constrained, prefer a symbol-first branch before continuing icon rendering. The current active symbol-first branch lives in the same CSV under these ids:

- `symbol_band_dna_01`
- `symbol_band_dna_02`
- `symbol_band_dna_03`
- `symbol_band_reduce_01`

The older `symbol_mobius_*` ids remain in the CSV as archived history. Do not continue iterating on them.

The active band-system prompts intentionally remove:

- app-icon tile constraints
- macOS badge framing
- heavy material rendering requirements
- node, ring, or terminal language
- brand-story and product-story wording

The goal is to first discover a strong mother symbol built from band geometry only, then adapt that symbol into an app icon later.

### Concrete Prompt Rules

At the symbol stage, use concrete geometry words only. Prefer:

- `band`
- `fold`
- `overlap`
- `twist`
- `span`
- `void`
- `silhouette`
- `band width`
- `corner radius`
- `curvature`
- `tension`
- `symmetry`
- `asymmetry`
- `negative space`

Avoid conceptual or mixed-stage words such as:

- `threadBridge`
- `routing`
- `trust`
- `productivity`
- `Fluent`
- `acrylic`
- `glass`
- `app icon`
- `tile`
- `scene`
- `brand story`

### Symbol Evaluation

Use `icon/exploration/z-image-turbo/symbol-scorecard-template.csv` for the symbol-first branch and reject anything that:

- reads as a graph or diagram instead of one coherent mark
- depends on nodes, rings, or UI framing
- breaks into detached inner fragments
- loses silhouette clarity at small size

## Promote A Final Image

Once a final candidate is chosen:

1. copy it somewhere stable under `icon/`
2. inspect it at `32x32`, `64x64`, `256x256`, and `1024x1024`
3. rebuild the macOS bundle icon:

```bash
scripts/build_macos_app_icon.sh path/to/final-image.png
```
