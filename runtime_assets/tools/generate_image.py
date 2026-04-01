#!/usr/bin/env python3
import argparse
import base64
import json
import mimetypes
import os
import re
import sys
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional
from uuid import uuid4


DEFAULT_ENDPOINT = "https://banana.147ai.com/v1beta/models/gemini-3.1-flash-image-preview:generateContent"
DEFAULT_ORIGIN = "https://banana.147ai.com"
DEFAULT_REFERER = "https://banana.147ai.com/"


def fail(message: str) -> None:
    raise SystemExit(message)


def load_env_file(path: Path) -> dict[str, str]:
    if not path.exists():
        return {}
    values: dict[str, str] = {}
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        key = key.strip()
        value = value.strip().strip("\"'")
        if key:
            values[key] = value
    return values


def load_config_env(config_env_path: Optional[str]) -> None:
    if not config_env_path:
        return
    for key, value in load_env_file(Path(config_env_path)).items():
        os.environ.setdefault(key, value)


def ensure_object(value, label: str) -> dict:
    if not isinstance(value, dict):
        fail(f"Invalid {label}: expected an object.")
    return value


def ensure_non_empty_string(value, label: str) -> str:
    if not isinstance(value, str) or not value.strip():
        fail(f"Invalid {label}.")
    return value.strip()


def ensure_string_list(value, label: str) -> list[str]:
    if not isinstance(value, list):
        fail(f"Invalid {label}.")
    result: list[str] = []
    for entry in value:
        result.append(ensure_non_empty_string(entry, label))
    return result


def validate_prompt(prompt_path: Path) -> dict:
    prompt = ensure_object(json.loads(prompt_path.read_text(encoding="utf-8")), "prompt config")
    provider = ensure_non_empty_string(prompt.get("provider"), "prompt.provider")
    if provider != "nanobanana":
        fail("Invalid prompt.provider.")
    mode = ensure_non_empty_string(prompt.get("mode"), "prompt.mode")
    if mode not in {"text_to_image", "image_edit"}:
        fail("Invalid prompt.mode.")

    generation_config = ensure_object(
        prompt.get("generation_config"),
        "prompt.generation_config",
    )
    response_modalities = ensure_string_list(
        generation_config.get("response_modalities"),
        "prompt.generation_config.response_modalities",
    )
    if response_modalities != ["IMAGE"]:
        fail("Invalid prompt.generation_config.response_modalities.")
    image_config = ensure_object(
        generation_config.get("image_config"),
        "prompt.generation_config.image_config",
    )
    safety_settings = prompt.get("safety_settings")
    if not isinstance(safety_settings, list) or not safety_settings:
        fail("Invalid prompt.safety_settings.")

    return {
        "mode": mode,
        "instruction": ensure_non_empty_string(prompt.get("instruction"), "prompt.instruction"),
        "image_inputs": ensure_string_list(prompt.get("image_inputs", []), "prompt.image_inputs"),
        "generation_config": {
            "responseModalities": ["IMAGE"],
            "imageConfig": {
                "aspectRatio": ensure_non_empty_string(
                    image_config.get("aspect_ratio"),
                    "prompt.generation_config.image_config.aspect_ratio",
                ),
                "imageSize": ensure_non_empty_string(
                    image_config.get("image_size"),
                    "prompt.generation_config.image_config.image_size",
                ),
            },
        },
        "safetySettings": [
            {
                "category": ensure_non_empty_string(
                    ensure_object(setting, "prompt.safety_settings").get("category"),
                    "prompt.safety_settings.category",
                ),
                "threshold": ensure_non_empty_string(
                    ensure_object(setting, "prompt.safety_settings").get("threshold"),
                    "prompt.safety_settings.threshold",
                ),
            }
            for setting in safety_settings
        ],
    }


def find_latest_prompt(workspace_dir: Path) -> Path:
    prompts_dir = workspace_dir / "prompts"
    if not prompts_dir.exists():
        fail("This workspace does not contain any prompt configs yet.")
    candidates = sorted(
        [path for path in prompts_dir.iterdir() if path.is_file() and path.suffix == ".json"],
        key=lambda path: path.name,
    )
    if not candidates:
        fail("This workspace does not contain any prompt configs yet.")
    return candidates[-1]


def resolve_prompt_path(workspace_dir: Path, value: Optional[str]) -> Path:
    if not value:
        return find_latest_prompt(workspace_dir)
    candidate = Path(value)
    if not candidate.is_absolute():
        candidate = workspace_dir / candidate
    if not candidate.exists():
        fail(f"Prompt config does not exist: {candidate}")
    return candidate


def mime_type_for_path(path: Path) -> str:
    guessed, _ = mimetypes.guess_type(path.name)
    return guessed or "image/png"


def build_request_payload(workspace_dir: Path, prompt: dict) -> dict:
    parts: list[dict] = [{"text": prompt["instruction"]}]
    for relative_input in prompt["image_inputs"]:
        image_path = Path(relative_input)
        if not image_path.is_absolute():
            image_path = workspace_dir / image_path
        if not image_path.exists():
            fail(f"Missing prompt image input: {image_path}")
        parts.append(
            {
                "inline_data": {
                    "mime_type": mime_type_for_path(image_path),
                    "data": base64.b64encode(image_path.read_bytes()).decode("ascii"),
                }
            }
        )

    return {
        "contents": [
            {
                "role": "user",
                "parts": parts,
            }
        ],
        "generationConfig": prompt["generation_config"],
        "safetySettings": prompt["safetySettings"],
    }


def request_headers() -> dict[str, str]:
    api_key = os.environ.get("NANOBANANA_API_KEY", "").strip()
    if not api_key:
        fail("Missing NANOBANANA_API_KEY.")
    headers = {
        "Authorization": f"Bearer {api_key}",
        "Content-Type": "application/json",
        "Accept": "application/json",
        "Origin": os.environ.get("NANOBANANA_API_ORIGIN", DEFAULT_ORIGIN),
        "Referer": os.environ.get("NANOBANANA_API_REFERER", DEFAULT_REFERER),
    }
    api_user = os.environ.get("NANOBANANA_API_USER", "").strip()
    if api_user:
        headers["New-Api-User"] = api_user
    return headers


def extract_image_parts(response_json: dict) -> list[tuple[str, bytes]]:
    images: list[tuple[str, bytes]] = []
    candidates = response_json.get("candidates")
    if not isinstance(candidates, list):
        return images
    for candidate in candidates:
        candidate_obj = ensure_object(candidate, "response.candidate")
        content = candidate_obj.get("content")
        if not isinstance(content, dict):
            continue
        parts = content.get("parts")
        if not isinstance(parts, list):
            continue
        for part in parts:
            if not isinstance(part, dict):
                continue
            inline_data = part.get("inlineData")
            if not isinstance(inline_data, dict):
                inline_data = part.get("inline_data")
            if not isinstance(inline_data, dict):
                continue
            mime_type = inline_data.get("mimeType")
            if not isinstance(mime_type, str):
                mime_type = inline_data.get("mime_type")
            data = inline_data.get("data")
            if isinstance(mime_type, str) and isinstance(data, str):
                images.append((mime_type, base64.b64decode(data)))
    return images


def extension_for_mime(mime_type: str) -> str:
    guessed = mimetypes.guess_extension(mime_type)
    if guessed == ".jpe":
        return ".jpg"
    return guessed or ".png"


def prompt_sequence(prompt_path: Path) -> str:
    match = re.match(r"^(\d+)_", prompt_path.name)
    if match:
        return match.group(1)
    return "latest"


def now_utc() -> str:
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--config-env")
    parser.add_argument("prompt_path", nargs="?")
    args = parser.parse_args()

    load_config_env(args.config_env)

    workspace_dir = Path.cwd()
    tool_results_dir = workspace_dir / ".threadbridge" / "tool_results"
    tool_results_dir.mkdir(parents=True, exist_ok=True)
    prompt_path = resolve_prompt_path(workspace_dir, args.prompt_path)
    prompt = validate_prompt(prompt_path)
    payload = build_request_payload(workspace_dir, prompt)

    run_id = f"{now_utc()}-{uuid4().hex[:8]}"
    sequence = prompt_sequence(prompt_path)
    run_dir = workspace_dir / "images" / "generated" / sequence / run_id
    run_dir.mkdir(parents=True, exist_ok=True)
    request_path = run_dir / "request.json"
    response_path = run_dir / "response.json"
    result_path = tool_results_dir / "generate_image.result.json"

    request_path.write_text(
        json.dumps(payload, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )

    request = urllib.request.Request(
        os.environ.get("NANOBANANA_API_ENDPOINT", DEFAULT_ENDPOINT),
        data=json.dumps(payload).encode("utf-8"),
        headers=request_headers(),
        method="POST",
    )

    try:
        with urllib.request.urlopen(request) as response:
            response_text = response.read().decode("utf-8")
    except urllib.error.HTTPError as error:
        body = error.read().decode("utf-8", errors="replace")
        response_path.write_text(body, encoding="utf-8")
        fail(f"Nanobanana API request failed with HTTP {error.code}: {body}")

    response_path.write_text(response_text, encoding="utf-8")
    response_json = ensure_object(json.loads(response_text), "API response")
    images = extract_image_parts(response_json)
    if not images:
        fail("Nanobanana API response did not contain any generated images.")

    image_paths: list[str] = []
    for index, (mime_type, data) in enumerate(images, start=1):
        image_path = run_dir / f"{index:04d}{extension_for_mime(mime_type)}"
        image_path.write_bytes(data)
        image_paths.append(image_path.relative_to(workspace_dir).as_posix())

    result = {
        "image_count": len(image_paths),
        "image_paths": image_paths,
        "prompt_path": prompt_path.relative_to(workspace_dir).as_posix(),
        "request_path": request_path.relative_to(workspace_dir).as_posix(),
        "response_path": response_path.relative_to(workspace_dir).as_posix(),
        "run_dir": run_dir.relative_to(workspace_dir).as_posix(),
    }
    result_path.write_text(
        json.dumps(result, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )

    print(json.dumps({"status": "ok", **result}, ensure_ascii=False))


if __name__ == "__main__":
    try:
        main()
    except SystemExit:
        raise
    except Exception as error:
        print(str(error), file=sys.stderr)
        raise
