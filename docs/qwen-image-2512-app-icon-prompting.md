# Qwen-Image-2512 App Icon Prompting

This note records the working prompt style for `Qwen-Image-2512` when the task is `app icon` exploration.

It is not a generic image-prompt guide. It is the repo-local baseline for later icon work once the local `Qwen-Image-2512` download is ready.

## Source Basis

This guidance is based on the current official materials:

- model card: https://huggingface.co/Qwen/Qwen-Image-2512
- official blog: https://qwenlm.github.io/blog/qwen-image/
- official Space prompt-rewrite implementation: https://huggingface.co/spaces/Qwen/Qwen-Image-2512/blob/main/app.py

## Working Assumptions

For `Qwen-Image-2512`, the safest default is:

- write one complete, concrete, continuous description
- prefer visual geometry words over abstract brand concepts
- state the icon container, lighting, material, and composition explicitly
- avoid relying on short slogan-like prompts
- use negative prompts only for a short, high-value artifact filter

The official materials suggest the model responds well to detailed natural-language descriptions and explicit visual constraints. The Space-side prompt rewriting also expands user input into a fuller descriptive paragraph instead of a tag pile.

## App Icon Rules

### 1. Describe Shapes, Not Ideas

Prefer:

- `a single translucent blue folded ribbon symbol with three rounded terminals`
- `a sharp glass paper-plane symbol with a faint luminous trail`
- `a continuous blue band folded into a stable triangular mark`

Avoid:

- `a logo representing intelligent connectivity`
- `a symbol of trust and innovation`
- `a threadBridge-style concept mark`

`threadBridge` is not a useful prompt token for this model. The prompt should encode the required result using visible shape language.

### 2. Use One Continuous Paragraph

Prefer a single coherent paragraph that covers:

- subject
- composition
- tile/background
- material
- lighting
- constraints

Do not reduce the prompt to a bag of short style tags unless a later test proves that a specific branch benefits from it.

### 3. Lock The Container

If the task is an actual app icon, say so concretely:

- `a rounded-square app icon tile`
- `centered composition`
- `single dominant symbol`
- `clean negative space`
- `readable at small sizes`

Do not assume the model will infer tile shape or product-icon framing from `app icon` alone.

### 4. Be Specific About Material

Prefer concrete material cues:

- `translucent acrylic`
- `semi-transparent glass`
- `soft cyan rim light`
- `inner glow`
- `clean specular highlights`
- `subtle volumetric shading`

Avoid stacking incompatible material and style systems in the same prompt.

### 5. Keep The Style Axis Narrow

Choose one primary rendering language per prompt, for example:

- `clean semi-realistic glass product icon`
- `Fluent-like acrylic product icon`
- `minimal polished 3D symbol`

Do not mix several competing systems such as `flat icon`, `photoreal glass`, `cinematic concept art`, and `poster rendering` in one prompt.

### 6. State Text Requirements Explicitly

If text must appear, specify the exact text.

If text must not appear, say:

- `The image contains no recognizable text.`

This is consistent with the official rewrite logic, which treats text presence as an explicit condition rather than something to leave ambiguous.

### 7. Write Negative Prompts Sparingly

Use short negative prompts only for recurring failure modes such as:

- `blurry edges`
- `extra objects`
- `duplicated shapes`
- `distorted geometry`
- `muddy transparency`
- `unreadable silhouette`
- `unreadable text`

Do not turn the negative prompt into a second full prompt.

## App Icon Prompt Template

Use this as the base structure:

```text
A premium app icon with a single centered symbol. [Describe the symbol with concrete geometry.] The symbol uses [material words], with [lighting words] and [surface detail words]. The background is a rounded-square tile in [color and gradient description]. The composition is clean, balanced, highly legible at small sizes, with no extra objects. The image contains no recognizable text.
```

## Direction Templates

### Paper Plane

```text
A premium app icon with a single centered translucent blue paper-plane symbol, sharp and elegant, with a clear readable silhouette. The plane has semi-transparent acrylic material, soft cyan edge glow, layered highlights, subtle internal reflections, and a faint luminous motion trail extending from the upper-right direction. A soft blue energy haze sits beneath the symbol without overpowering the shape. The background is a rounded-square tile in cool desaturated blue-gray with a smooth atmospheric gradient and restrained glow. The composition is clean, balanced, and highly legible at small sizes, with no extra objects. The image contains no recognizable text.
```

### Ribbon / VI

```text
A premium app icon with a single centered translucent blue folded ribbon symbol forming a stable triangular balance with three rounded terminals. The symbol feels like a strong visual identity mark rather than a diagram. Semi-transparent acrylic material, thin cyan rim light, inner glow, smooth highlights, subtle depth, crisp silhouette, no extra objects. The background is a rounded-square tile in deep blue-gray with a soft gradient and restrained ambient glow. The image contains no recognizable text.
```

### Continuous Band

```text
A premium app icon with a single centered symbol made from one continuous translucent blue band folded into a stable triangular mark. The symbol has rounded ends, clean negative space, strong silhouette clarity, soft cyan edge light, subtle inner glow, and polished glass-like shading. The background is a dark blue-gray rounded-square tile with a smooth gradient and minimal atmospheric glow. The composition is clean and highly readable at small sizes. The image contains no recognizable text.
```

## Evaluation Checklist

Reject a result if it does any of the following:

- reads as a generic crypto or network badge
- loses the main silhouette at small size
- introduces extra objects or secondary symbols
- mixes multiple incompatible material languages
- uses text when the prompt asked for none
- replaces geometry with vague glow or effects

Keep a result if it satisfies all of the following:

- the symbol reads immediately at icon size
- the tile and symbol are clearly separated
- the material and lighting support the silhouette instead of obscuring it
- the prompt vocabulary maps directly to visible features
