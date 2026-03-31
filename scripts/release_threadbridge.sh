#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd -P)
CARGO_HOME_DIR="${CARGO_HOME:-$REPO_ROOT/.cargo}"
CARGO_TARGET_DIR_PATH="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"
RUSTUP_HOME_DIR="${RUSTUP_HOME:-$HOME/.rustup}"
RUNTIME_PATH="$CARGO_HOME_DIR/bin:$HOME/.cargo/bin:$PATH"
DIST_ROOT="${THREADBRIDGE_RELEASE_DIST_ROOT:-$REPO_ROOT/dist/release}"
APP_NAME="threadBridge"
APP_BUNDLE_NAME="threadBridge.app"
APP_EXECUTABLE_NAME="threadbridge_desktop"
WORKER_BINARY_NAME="app_server_ws_worker"
DEFAULT_GITHUB_REPO="qoli/threadBridge"
DEFAULT_NOTARY_PROFILE="${THREADBRIDGE_NOTARY_PROFILE:-threadbridge-notary}"
APPLE_TARGETS=(
  "aarch64-apple-darwin"
  "x86_64-apple-darwin"
)

usage() {
  cat <<'EOF'
Usage: release_threadbridge.sh <command> --version <version> [options]

Commands:
  build       Build a universal release app bundle in dist/release/<version>/
  sign        Build and codesign the universal release app
  dmg         Build, sign, and package the DMG
  notarize    Build, sign, package, notarize, and staple the DMG
  publish     Publish the existing notarized DMG to a GitHub draft prerelease
  release     Run the full build -> sign -> dmg -> notarize -> publish pipeline
  help        Show this help

Required options:
  --version <version>                    Release version, e.g. 0.1.0-rc.1

Options for sign/dmg/notarize/release:
  --codesign-identity <identity>         Developer ID Application identity name

Options for notarize/release:
  --notary-profile <profile>             Default: threadbridge-notary

Options for publish/release:
  --notes-file <path>                    Release notes markdown file

Optional publication overrides:
  --github-repo <owner/repo>             Default: qoli/threadBridge
EOF
}

log() {
  printf '[release-threadbridge] %s\n' "$*"
}

fail() {
  printf 'release_threadbridge.sh: %s\n' "$*" >&2
  exit 1
}

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    fail "missing required command: $1"
  fi
}

require_macos() {
  [[ "$(uname -s)" == "Darwin" ]] || fail "release_threadbridge.sh is supported on macOS only"
}

ensure_clean_worktree() {
  if [[ -n "$(git -C "$REPO_ROOT" status --porcelain)" ]]; then
    fail "working tree must be clean before publish/release"
  fi
}

ensure_icon_assets() {
  local icon_script="$REPO_ROOT/scripts/build_macos_app_icon.sh"
  local existing_icns="$REPO_ROOT/rust/static/app_icon/threadBridge.icns"
  [[ -x "$icon_script" ]] || fail "missing app icon build script: $icon_script"
  if [[ -f "$existing_icns" ]]; then
    log "using existing macOS app icon: $existing_icns"
    return 0
  fi
  log "building macOS app icon assets"
  (
    cd "$REPO_ROOT"
    "$icon_script"
  )
}

ensure_cargo_bundle_installed() {
  require_command cargo
  if [[ -x "$CARGO_HOME_DIR/bin/cargo-bundle" ]] || command -v cargo-bundle >/dev/null 2>&1; then
    return 0
  fi
  log "installing cargo-bundle into $CARGO_HOME_DIR/bin"
  (
    cd "$REPO_ROOT"
    export PATH="$RUNTIME_PATH"
    export CARGO_HOME="$CARGO_HOME_DIR"
    export CARGO_TARGET_DIR="$CARGO_TARGET_DIR_PATH"
    export RUSTUP_HOME="$RUSTUP_HOME_DIR"
    cargo install cargo-bundle
  )
}

ensure_target_installed() {
  local target=$1
  require_command rustup
  if rustup target list --installed | grep -Fx "$target" >/dev/null 2>&1; then
    return 0
  fi
  log "installing Rust target: $target"
  rustup target add "$target"
}

release_root() {
  printf '%s/%s\n' "$DIST_ROOT" "$VERSION"
}

universal_app_path() {
  printf '%s/%s\n' "$(release_root)" "$APP_BUNDLE_NAME"
}

dmg_path() {
  printf '%s/%s-%s-macos-universal.dmg\n' "$(release_root)" "$APP_NAME" "$VERSION"
}

checksum_path() {
  printf '%s/%s-%s-macos-universal.sha256\n' "$(release_root)" "$APP_NAME" "$VERSION"
}

bundle_path_for_target() {
  local target=$1
  printf '%s/%s/release/bundle/osx/%s\n' "$CARGO_TARGET_DIR_PATH" "$target" "$APP_BUNDLE_NAME"
}

bundle_binary_path_for_target() {
  local target=$1
  printf '%s/Contents/MacOS/%s\n' "$(bundle_path_for_target "$target")" "$APP_EXECUTABLE_NAME"
}

worker_binary_path_for_target() {
  local target=$1
  printf '%s/%s/release/%s\n' "$CARGO_TARGET_DIR_PATH" "$target" "$WORKER_BINARY_NAME"
}

universal_main_binary_path() {
  printf '%s/Contents/MacOS/%s\n' "$(universal_app_path)" "$APP_EXECUTABLE_NAME"
}

universal_worker_binary_path() {
  printf '%s/Contents/MacOS/%s\n' "$(universal_app_path)" "$WORKER_BINARY_NAME"
}

ensure_release_layout() {
  mkdir -p "$(release_root)"
}

require_release_build_prerequisites() {
  require_command cargo
  require_command rustup
  require_command lipo
}

build_arch_release_artifacts() {
  local target=$1

  ensure_target_installed "$target"
  log "building bundle for $target"
  (
    cd "$REPO_ROOT"
    export PATH="$RUNTIME_PATH"
    export CARGO_HOME="$CARGO_HOME_DIR"
    export CARGO_TARGET_DIR="$CARGO_TARGET_DIR_PATH"
    export RUSTUP_HOME="$RUSTUP_HOME_DIR"
    cargo bundle --release --target "$target" --bin "$APP_EXECUTABLE_NAME"
    cargo build --release --target "$target" --bin "$WORKER_BINARY_NAME"
  )

  [[ -d "$(bundle_path_for_target "$target")" ]] || fail "missing bundled app for $target"
  [[ -x "$(worker_binary_path_for_target "$target")" ]] || fail "missing worker binary for $target"
}

build_universal_release_bundle() {
  require_release_build_prerequisites
  ensure_release_layout
  ensure_icon_assets
  ensure_cargo_bundle_installed

  local target
  for target in "${APPLE_TARGETS[@]}"; do
    build_arch_release_artifacts "$target"
  done

  rm -rf "$(universal_app_path)"
  cp -R "$(bundle_path_for_target "${APPLE_TARGETS[0]}")" "$(universal_app_path)"

  log "creating universal desktop binary"
  lipo -create \
    "$(bundle_binary_path_for_target "${APPLE_TARGETS[0]}")" \
    "$(bundle_binary_path_for_target "${APPLE_TARGETS[1]}")" \
    -output "$(universal_main_binary_path)"

  log "creating universal worker binary"
  lipo -create \
    "$(worker_binary_path_for_target "${APPLE_TARGETS[0]}")" \
    "$(worker_binary_path_for_target "${APPLE_TARGETS[1]}")" \
    -output "$(universal_worker_binary_path)"

  chmod +x "$(universal_main_binary_path)" "$(universal_worker_binary_path)"
  log "universal app ready: $(universal_app_path)"
}

require_codesign_identity() {
  [[ -n "${CODESIGN_IDENTITY:-}" ]] || fail "--codesign-identity is required for $COMMAND"
}

require_notes_file() {
  [[ -n "${NOTES_FILE:-}" ]] || fail "--notes-file is required for $COMMAND"
  [[ -f "$NOTES_FILE" ]] || fail "missing release notes file: $NOTES_FILE"
}

verify_codesign_identity_available() {
  require_command security
  [[ "$CODESIGN_IDENTITY" == *"Developer ID Application:"* ]] \
    || fail "--codesign-identity must be a Developer ID Application identity"
  security find-identity -v -p codesigning | grep -F "$CODESIGN_IDENTITY" >/dev/null 2>&1 \
    || fail "codesign identity not found in keychain: $CODESIGN_IDENTITY"
}

verify_notary_profile_available() {
  require_command xcrun
  xcrun notarytool history --keychain-profile "$NOTARY_PROFILE" >/dev/null 2>&1 \
    || fail "notarytool keychain profile is unavailable: $NOTARY_PROFILE"
}

sign_release_bundle() {
  require_codesign_identity
  require_command codesign
  require_command spctl
  verify_codesign_identity_available
  [[ -d "$(universal_app_path)" ]] || build_universal_release_bundle

  log "codesigning nested worker binary"
  codesign --force --timestamp --options runtime --sign "$CODESIGN_IDENTITY" "$(universal_worker_binary_path)"

  log "codesigning app bundle"
  codesign --force --timestamp --options runtime --sign "$CODESIGN_IDENTITY" "$(universal_app_path)"

  log "verifying codesign state"
  codesign --verify --deep --strict --verbose=2 "$(universal_app_path)"
  spctl -a -vv --type exec "$(universal_app_path)"
}

create_release_dmg() {
  require_command hdiutil
  sign_release_bundle
  rm -f "$(dmg_path)" "$(checksum_path)"

  local staging_dir
  staging_dir=$(mktemp -d "${TMPDIR:-/tmp}/threadbridge-dmg-stage.XXXXXX")

  cp -R "$(universal_app_path)" "$staging_dir/$APP_BUNDLE_NAME"
  log "creating DMG: $(dmg_path)"
  hdiutil create \
    -volname "$APP_NAME" \
    -srcfolder "$staging_dir" \
    -ov \
    -format UDZO \
    "$(dmg_path)" >/dev/null

  rm -rf "$staging_dir"
}

notarize_release_dmg() {
  require_command spctl
  verify_notary_profile_available
  [[ -f "$(dmg_path)" ]] || create_release_dmg

  log "submitting DMG for notarization"
  xcrun notarytool submit "$(dmg_path)" --keychain-profile "$NOTARY_PROFILE" --wait
  xcrun stapler staple "$(dmg_path)"
  xcrun stapler validate "$(dmg_path)"
  spctl -a -vv --type open "$(dmg_path)"
}

write_checksum() {
  local checksum
  checksum=$(shasum -a 256 "$(dmg_path)" | awk '{print $1}')
  printf '%s  %s\n' "$checksum" "$(basename "$(dmg_path)")" > "$(checksum_path)"
}

release_tag() {
  printf 'v%s\n' "$VERSION"
}

ensure_gh_authenticated() {
  require_command gh
  gh auth status >/dev/null 2>&1 || fail "gh auth status failed; authenticate GitHub CLI first"
}

ensure_release_assets_exist() {
  [[ -f "$(dmg_path)" ]] || fail "missing DMG: $(dmg_path)"
  [[ -f "$(checksum_path)" ]] || write_checksum
}

publish_github_release() {
  ensure_gh_authenticated

  if gh release view "$(release_tag)" --repo "$GITHUB_REPO" >/dev/null 2>&1; then
    log "updating existing GitHub draft prerelease $(release_tag)"
    gh release upload "$(release_tag)" "$(dmg_path)" "$(checksum_path)" --repo "$GITHUB_REPO" --clobber
    gh release edit "$(release_tag)" \
      --repo "$GITHUB_REPO" \
      --title "$APP_NAME $VERSION" \
      --notes-file "$NOTES_FILE" \
      --draft \
      --prerelease
  else
    log "creating GitHub draft prerelease $(release_tag)"
    gh release create "$(release_tag)" \
      "$(dmg_path)" \
      "$(checksum_path)" \
      --repo "$GITHUB_REPO" \
      --title "$APP_NAME $VERSION" \
      --notes-file "$NOTES_FILE" \
      --draft \
      --prerelease
  fi
}

publish_release() {
  require_notes_file
  ensure_clean_worktree
  ensure_release_assets_exist
  publish_github_release
}

run_release() {
  require_codesign_identity
  require_notes_file
  ensure_clean_worktree
  build_universal_release_bundle
  create_release_dmg
  notarize_release_dmg
  write_checksum
  publish_release
}

parse_args() {
  COMMAND="${1:-help}"
  shift || true

  VERSION=""
  NOTES_FILE=""
  CODESIGN_IDENTITY=""
  NOTARY_PROFILE="$DEFAULT_NOTARY_PROFILE"
  GITHUB_REPO="$DEFAULT_GITHUB_REPO"

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --version)
        shift
        [[ $# -gt 0 ]] || fail "missing value for --version"
        VERSION=$1
        ;;
      --notes-file)
        shift
        [[ $# -gt 0 ]] || fail "missing value for --notes-file"
        NOTES_FILE=$1
        ;;
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
      help|-h|--help)
        COMMAND="help"
        ;;
      *)
        fail "unknown argument: $1"
        ;;
    esac
    shift || true
  done
}

require_version() {
  [[ -n "${VERSION:-}" ]] || fail "--version is required for $COMMAND"
}

main() {
  require_macos
  parse_args "$@"

  case "$COMMAND" in
    help)
      usage
      ;;
    build)
      require_version
      build_universal_release_bundle
      ;;
    sign)
      require_version
      sign_release_bundle
      ;;
    dmg)
      require_version
      create_release_dmg
      ;;
    notarize)
      require_version
      notarize_release_dmg
      write_checksum
      ;;
    publish)
      require_version
      publish_release
      ;;
    release)
      require_version
      run_release
      ;;
    *)
      usage
      exit 1
      ;;
  esac
}

main "$@"
