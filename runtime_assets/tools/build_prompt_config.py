#!/usr/bin/env python3
import argparse
import json
import re
import sys
from pathlib import Path


def fail(message: str) -> None:
    raise SystemExit(message)


def ensure_object(value, label: str) -> dict:
    if not isinstance(value, dict):
        fail(f"Invalid {label}: expected an object.")
    return value


def ensure_non_empty_string(value, label: str) -> str:
    if not isinstance(value, str) or not value.strip():
        fail(f"Invalid {label}.")
    return value.strip()


def ensure_string_list(value, label: str, allow_empty: bool = True) -> list[str]:
    if not isinstance(value, list):
        fail(f"Invalid {label}.")
    items: list[str] = []
    for entry in value:
        items.append(ensure_non_empty_string(entry, label))
    if not allow_empty and not items:
        fail(f"Invalid {label}.")
    return items


def slugify_variant_id(value: str) -> str:
    slug = re.sub(r"[^a-z0-9]+", "_", value.strip().lower()).strip("_")
    return slug[:32] or "variant"


def validate_concept(value) -> dict:
    concept = ensure_object(value, "request.concept")
    return {
        "concept_id": ensure_non_empty_string(concept.get("concept_id"), "request.concept.concept_id"),
        "title": ensure_non_empty_string(concept.get("title"), "request.concept.title"),
        "summary": ensure_non_empty_string(concept.get("summary"), "request.concept.summary"),
        "keywords": ensure_string_list(concept.get("keywords", []), "request.concept.keywords"),
        "style_notes": ensure_string_list(
            concept.get("style_notes", []),
            "request.concept.style_notes",
        ),
        "constraints": ensure_string_list(
            concept.get("constraints", []),
            "request.concept.constraints",
        ),
        "source": ensure_non_empty_string(concept.get("source"), "request.concept.source"),
        "updated_at": ensure_non_empty_string(
            concept.get("updated_at"),
            "request.concept.updated_at",
        ),
    }


def validate_prompt(value) -> dict:
    prompt = ensure_object(value, "request.prompt")
    provider = ensure_non_empty_string(prompt.get("provider"), "request.prompt.provider")
    if provider != "nanobanana":
        fail("Invalid request.prompt.provider.")
    mode = ensure_non_empty_string(prompt.get("mode"), "request.prompt.mode")
    if mode not in {"text_to_image", "image_edit"}:
        fail("Invalid request.prompt.mode.")

    generation_config = ensure_object(
        prompt.get("generation_config"),
        "request.prompt.generation_config",
    )
    response_modalities = ensure_string_list(
        generation_config.get("response_modalities"),
        "request.prompt.generation_config.response_modalities",
        allow_empty=False,
    )
    if response_modalities != ["IMAGE"]:
        fail("Invalid request.prompt.generation_config.response_modalities.")

    image_config = ensure_object(
        generation_config.get("image_config"),
        "request.prompt.generation_config.image_config",
    )
    safety_settings = prompt.get("safety_settings")
    if not isinstance(safety_settings, list) or not safety_settings:
        fail("Invalid request.prompt.safety_settings.")
    parsed_safety_settings: list[dict] = []
    for index, entry in enumerate(safety_settings):
        setting = ensure_object(entry, f"request.prompt.safety_settings[{index}]")
        parsed_safety_settings.append(
            {
                "category": ensure_non_empty_string(
                    setting.get("category"),
                    f"request.prompt.safety_settings[{index}].category",
                ),
                "threshold": ensure_non_empty_string(
                    setting.get("threshold"),
                    f"request.prompt.safety_settings[{index}].threshold",
                ),
            }
        )

    metadata = ensure_object(prompt.get("metadata"), "request.prompt.metadata")
    return {
        "concept_id": ensure_non_empty_string(prompt.get("concept_id"), "request.prompt.concept_id"),
        "variant_id": ensure_non_empty_string(prompt.get("variant_id"), "request.prompt.variant_id"),
        "provider": "nanobanana",
        "mode": mode,
        "instruction": ensure_non_empty_string(
            prompt.get("instruction"),
            "request.prompt.instruction",
        ),
        "image_inputs": ensure_string_list(
            prompt.get("image_inputs", []),
            "request.prompt.image_inputs",
        ),
        "generation_config": {
            "response_modalities": ["IMAGE"],
            "image_config": {
                "aspect_ratio": ensure_non_empty_string(
                    image_config.get("aspect_ratio"),
                    "request.prompt.generation_config.image_config.aspect_ratio",
                ),
                "image_size": ensure_non_empty_string(
                    image_config.get("image_size"),
                    "request.prompt.generation_config.image_config.image_size",
                ),
            },
        },
        "safety_settings": parsed_safety_settings,
        "metadata": {
            "source": ensure_non_empty_string(metadata.get("source"), "request.prompt.metadata.source"),
            "timestamp": ensure_non_empty_string(
                metadata.get("timestamp"),
                "request.prompt.metadata.timestamp",
            ),
            "notes": ensure_string_list(metadata.get("notes", []), "request.prompt.metadata.notes"),
        },
    }


def next_prompt_sequence(prompts_dir: Path) -> int:
    highest = 0
    for path in prompts_dir.iterdir():
        if not path.is_file():
            continue
        match = re.match(r"^(\d+)_", path.name)
        if match:
            highest = max(highest, int(match.group(1)))
    return highest + 1


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--config-env")
    parser.parse_args()

    workspace_dir = Path.cwd()
    runtime_dir = workspace_dir / ".threadbridge"
    tool_requests_dir = runtime_dir / "tool_requests"
    tool_results_dir = runtime_dir / "tool_results"
    tool_requests_dir.mkdir(parents=True, exist_ok=True)
    tool_results_dir.mkdir(parents=True, exist_ok=True)
    request_path = tool_requests_dir / "build_prompt_config.request.json"
    result_path = tool_results_dir / "build_prompt_config.result.json"

    if not request_path.exists():
        fail(f"Missing {request_path}.")

    request = ensure_object(json.loads(request_path.read_text(encoding="utf-8")), "request")
    concept = validate_concept(request.get("concept"))
    prompt = validate_prompt(request.get("prompt"))

    prompts_dir = workspace_dir / "prompts"
    prompts_dir.mkdir(parents=True, exist_ok=True)

    concept_path = workspace_dir / "concept.json"
    sequence = next_prompt_sequence(prompts_dir)
    prompt_file_name = f"{sequence:03d}_{slugify_variant_id(prompt['variant_id'])}.json"
    prompt_path = prompts_dir / prompt_file_name

    concept_path.write_text(
        json.dumps(concept, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    prompt_path.write_text(
        json.dumps(prompt, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )

    result = {
        "concept_path": concept_path.relative_to(workspace_dir).as_posix(),
        "prompt_path": prompt_path.relative_to(workspace_dir).as_posix(),
        "prompt_file_name": prompt_file_name,
    }
    result_path.write_text(
        json.dumps(result, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )

    print(
        json.dumps(
            {
                "status": "ok",
                **result,
            },
            ensure_ascii=False,
        )
    )


if __name__ == "__main__":
    try:
        main()
    except SystemExit:
        raise
    except Exception as error:
        print(str(error), file=sys.stderr)
        raise
