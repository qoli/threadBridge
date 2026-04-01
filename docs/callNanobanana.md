# Nanobanana Request Notes

This file documents the request shape used by `runtime_assets/tools/generate_image.py` without including any live credential, cookie, or captured production payload.

## Minimal Example

```bash
curl "$NANOBANANA_API_ENDPOINT" \
  -X POST \
  -H "Authorization: Bearer $NANOBANANA_API_KEY" \
  -H "Content-Type: application/json" \
  -H "Origin: ${NANOBANANA_API_ORIGIN:-https://example.invalid}" \
  -H "Referer: ${NANOBANANA_API_REFERER:-https://example.invalid/}" \
  -H "New-Api-User: ${NANOBANANA_API_USER:-replace-me}" \
  --data @request.json
```

## Request Shape

The runtime sends a JSON payload of this form:

```json
{
  "contents": [
    {
      "role": "user",
      "parts": [
        {
          "text": "Final provider-ready instruction."
        }
      ]
    }
  ],
  "generationConfig": {
    "responseModalities": ["IMAGE"],
    "imageConfig": {
      "aspectRatio": "1:1",
      "imageSize": "1K"
    }
  },
  "safetySettings": [
    {
      "category": "HARM_CATEGORY_HATE_SPEECH",
      "threshold": "BLOCK_MEDIUM_AND_ABOVE"
    }
  ]
}
```

## Public Repo Rules

- Do not commit real API keys.
- Do not commit session cookies.
- Do not commit raw captured requests that embed user images or base64 payloads.
- If you need to debug a provider issue, store that trace locally outside Git.
