#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd -P)
DEFAULT_NOTARY_PROFILE="${THREADBRIDGE_RELEASE_NOTARY_PROFILE:-threadbridge-notary}"
DEFAULT_GITHUB_REPO="${THREADBRIDGE_RELEASE_GITHUB_REPO:-qoli/threadBridge}"
DEFAULT_NOTES_DIR="${THREADBRIDGE_RELEASE_NOTES_DIR:-$REPO_ROOT/docs/releases}"
DEFAULT_FASTLANE_ASC_DIR="${THREADBRIDGE_FASTLANE_ASC_DIR:-$REPO_ROOT/fastlane/threadbridge-asc}"
DEFAULT_FASTLANE_DIR="${THREADBRIDGE_FASTLANE_DIR:-$REPO_ROOT/fastlane}"

usage() {
  cat <<'EOF'
Usage: release_rc.sh <version> [options]

Normal path:
  scripts/release_rc.sh 0.1.0-rc.2

This wrapper creates or reuses docs/releases/<version>.md, infers the common
repo defaults, then runs scripts/release_threadbridge.sh release.

Options:
  --codesign-identity <identity>  Developer ID Application identity name
  --notary-profile <profile>      Default: threadbridge-notary
  --github-repo <owner/repo>      Default: qoli/threadBridge
  --notes-file <path>             Default: docs/releases/<version>.md
  --publish-final                 Also create/push the git tag and publish the draft prerelease
  -h, --help                      Show this help

Environment overrides:
  THREADBRIDGE_RELEASE_CODESIGN_IDENTITY
  THREADBRIDGE_RELEASE_NOTARY_PROFILE
  THREADBRIDGE_RELEASE_GITHUB_REPO
  THREADBRIDGE_RELEASE_NOTES_DIR
  THREADBRIDGE_FASTLANE_ASC_DIR
  THREADBRIDGE_FASTLANE_DIR
EOF
}

log() {
  printf '[release-rc] %s\n' "$*"
}

fail() {
  printf 'release_rc.sh: %s\n' "$*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

has_notary_profile() {
  xcrun notarytool history --keychain-profile "$NOTARY_PROFILE" >/dev/null 2>&1
}

json_value() {
  local key=$1
  local file=$2
  sed -n "s/.*\"$key\"[[:space:]]*:[[:space:]]*\"\\([^\"]*\\)\".*/\\1/p" "$file" | head -n 1
}

parse_args() {
  VERSION="${1:-}"
  [[ -n "$VERSION" ]] || {
    usage
    exit 1
  }
  shift || true

  CODESIGN_IDENTITY="${THREADBRIDGE_RELEASE_CODESIGN_IDENTITY:-}"
  NOTARY_PROFILE="$DEFAULT_NOTARY_PROFILE"
  GITHUB_REPO="$DEFAULT_GITHUB_REPO"
  NOTES_FILE="$DEFAULT_NOTES_DIR/$VERSION.md"
  PUBLISH_FINAL=0

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --codesign-identity)
        shift
        [[ $# -gt 0 ]] || fail "missing value for --codesign-identity"
        CODESIGN_IDENTITY=$1
        ;;
      --notary-profile)
        shift
        [[ $# -gt 0 ]] || fail "missing value for --notary-profile"
        NOTARY_PROFILE=$1
        ;;
      --github-repo)
        shift
        [[ $# -gt 0 ]] || fail "missing value for --github-repo"
        GITHUB_REPO=$1
        ;;
      --notes-file)
        shift
        [[ $# -gt 0 ]] || fail "missing value for --notes-file"
        NOTES_FILE=$1
        ;;
      --publish-final)
        PUBLISH_FINAL=1
        ;;
      -h|--help)
        usage
        exit 0
        ;;
      *)
        fail "unknown argument: $1"
        ;;
    esac
    shift || true
  done
}

detect_codesign_identity() {
  [[ -n "$CODESIGN_IDENTITY" ]] && return 0

  require_command security

  local matches
  matches=$(
    security find-identity -v -p codesigning \
      | sed -n 's/.*"\(Developer ID Application:[^"]*\)".*/\1/p' \
      | awk '!seen[$0]++'
  )

  if [[ -z "$matches" ]]; then
    fail "no Developer ID Application identity found; set THREADBRIDGE_RELEASE_CODESIGN_IDENTITY or pass --codesign-identity"
  fi

  local count
  count=$(printf '%s\n' "$matches" | sed '/^$/d' | wc -l | tr -d ' ')
  if [[ "$count" != "1" ]]; then
    printf 'release_rc.sh: multiple Developer ID Application identities found:\n' >&2
    printf '%s\n' "$matches" >&2
    fail "pass --codesign-identity or set THREADBRIDGE_RELEASE_CODESIGN_IDENTITY"
  fi

  CODESIGN_IDENTITY=$(printf '%s\n' "$matches" | head -n 1)
}

ensure_release_notes() {
  local notes_dir
  notes_dir=$(dirname "$NOTES_FILE")
  mkdir -p "$notes_dir"

  if [[ -f "$NOTES_FILE" ]]; then
    return 0
  fi

  log "creating release notes stub: $NOTES_FILE"
  cat > "$NOTES_FILE" <<EOF
# threadBridge $VERSION

Replacement RC for the previous broken public release.

Highlights:

- fixes bundled runtime path resolution outside the repo
- release runtime data defaults to ~/Library/Application Support/threadBridge

Known limitations:

- Homebrew tap publication is not included in this RC
- release automation is currently local-operator driven
EOF
}

bootstrap_notary_profile_from_asc_key() {
  local api_key_json="$DEFAULT_FASTLANE_ASC_DIR/api_key.json"
  [[ -f "$api_key_json" ]] || return 1

  local key_id issuer_id key_path
  key_id=$(json_value "key_id" "$api_key_json")
  issuer_id=$(json_value "issuer_id" "$api_key_json")
  [[ -n "$key_id" && -n "$issuer_id" ]] || return 1

  key_path="$DEFAULT_FASTLANE_ASC_DIR/AuthKey_${key_id}.p8"
  [[ -f "$key_path" ]] || return 1

  log "bootstrapping notary profile $NOTARY_PROFILE from local ASC API key"
  xcrun notarytool store-credentials "$NOTARY_PROFILE" \
    --key "$key_path" \
    --key-id "$key_id" \
    --issuer "$issuer_id"
}

bootstrap_notary_profile_from_fastlane() {
  [[ -d "$DEFAULT_FASTLANE_DIR" ]] || return 1
  command -v fastlane >/dev/null 2>&1 || return 1

  local app_password="${THREADBRIDGE_NOTARY_APP_SPECIFIC_PASSWORD:-${FASTLANE_APPLE_APPLICATION_SPECIFIC_PASSWORD:-}}"
  [[ -n "$app_password" ]] || return 1

  log "bootstrapping notary profile $NOTARY_PROFILE via fastlane lane"
  (
    cd "$REPO_ROOT"
    THREADBRIDGE_NOTARY_PROFILE="$NOTARY_PROFILE" \
    THREADBRIDGE_NOTARY_APP_SPECIFIC_PASSWORD="$app_password" \
      fastlane bootstrap_notary_profile
  )
}

ensure_notary_profile() {
  require_command xcrun

  if has_notary_profile; then
    return 0
  fi

  bootstrap_notary_profile_from_asc_key || bootstrap_notary_profile_from_fastlane || fail \
    "missing notary profile $NOTARY_PROFILE and could not bootstrap it from local ASC key or fastlane"

  has_notary_profile || fail "failed to create notary profile $NOTARY_PROFILE"
}

run_draft_release() {
  ensure_notary_profile
  log "running draft prerelease pipeline for $VERSION"
  "$SCRIPT_DIR/release_threadbridge.sh" release \
    --version "$VERSION" \
    --notes-file "$NOTES_FILE" \
    --codesign-identity "$CODESIGN_IDENTITY" \
    --notary-profile "$NOTARY_PROFILE" \
    --github-repo "$GITHUB_REPO"
}

tag_exists() {
  git -C "$REPO_ROOT" rev-parse -q --verify "refs/tags/v$VERSION" >/dev/null 2>&1
}

publish_final_release() {
  require_command git
  require_command gh

  if ! tag_exists; then
    log "creating annotated git tag v$VERSION"
    git -C "$REPO_ROOT" tag -a "v$VERSION" -m "threadBridge $VERSION"
  else
    log "reusing existing git tag v$VERSION"
  fi

  log "pushing git tag v$VERSION"
  git -C "$REPO_ROOT" push origin "v$VERSION"

  log "publishing GitHub prerelease v$VERSION"
  gh release edit "v$VERSION" \
    --repo "$GITHUB_REPO" \
    --draft=false \
    --prerelease

  log "verifying final GitHub release state"
  gh release view "v$VERSION" \
    --repo "$GITHUB_REPO" \
    --json tagName,isDraft,isPrerelease,url,publishedAt,assets
}

print_next_steps() {
  cat <<EOF

Draft prerelease is ready.

Notes file:
  $NOTES_FILE

To publish the existing draft later:
  git tag -a v$VERSION -m "threadBridge $VERSION"
  git push origin v$VERSION
  gh release edit v$VERSION --repo $GITHUB_REPO --draft=false --prerelease
EOF
}

main() {
  parse_args "$@"
  detect_codesign_identity
  ensure_release_notes
  run_draft_release

  if [[ "$PUBLISH_FINAL" == "1" ]]; then
    publish_final_release
    return 0
  fi

  print_next_steps
}

main "$@"
