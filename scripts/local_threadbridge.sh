#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd -P)
ENV_FILE="$REPO_ROOT/.env.local"
LOG_DIR="$REPO_ROOT/logs"
STDOUT_LOG="$LOG_DIR/local-threadbridge.stdout.log"
STDERR_LOG="$LOG_DIR/local-threadbridge.stderr.log"
EVENT_LOG="$REPO_ROOT/data/debug/events.jsonl"
CARGO_HOME_DIR="${CARGO_HOME:-$REPO_ROOT/.cargo}"
CARGO_TARGET_DIR_PATH="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"
BUILD_PROFILE="${BUILD_PROFILE:-dev}"
RUSTUP_HOME_DIR="${RUSTUP_HOME:-$HOME/.rustup}"
RUNTIME_PATH="$HOME/.cargo/bin:$REPO_ROOT/bin:$PATH"
MANAGED_CODEX_DIR="$REPO_ROOT/.threadbridge/codex"
MANAGED_CODEX_BIN="$MANAGED_CODEX_DIR/codex"
MANAGED_CODEX_TAG_FILE="$MANAGED_CODEX_DIR/release-tag.txt"

usage() {
  cat <<'EOF'
Usage: local_threadbridge.sh <command>

Commands:
  start
  stop
  restart
  status
  logs

Environment overrides:
  BUILD_PROFILE=dev|release   Build profile to run. Default: dev
EOF
}

log() {
  printf '[local-threadbridge] %s\n' "$*"
}

managed_codex_asset_name() {
  local system arch
  system=$(uname -s)
  arch=$(uname -m)
  case "$system:$arch" in
    Darwin:arm64)
      printf '%s\n' 'codex-aarch64-apple-darwin.tar.gz'
      ;;
    Darwin:x86_64)
      printf '%s\n' 'codex-x86_64-apple-darwin.tar.gz'
      ;;
    Linux:aarch64|Linux:arm64)
      printf '%s\n' 'codex-aarch64-unknown-linux-gnu.tar.gz'
      ;;
    Linux:x86_64)
      printf '%s\n' 'codex-x86_64-unknown-linux-gnu.tar.gz'
      ;;
    *)
      printf 'Unsupported platform for managed Codex binary: %s %s\n' "$system" "$arch" >&2
      exit 1
      ;;
  esac
}

latest_managed_codex_tag() {
  gh api "repos/openai/codex/releases?per_page=100" | python3 -c '
import json, sys
for release in json.load(sys.stdin):
    if release.get("prerelease") and not release.get("draft"):
        tag = release.get("tag_name")
        if tag:
            print(tag)
            break
'
}

ensure_managed_codex_binary() {
  require_command gh
  require_command tar

  mkdir -p "$MANAGED_CODEX_DIR"

  local tag asset current_tag tmpdir archive extracted_binary
  tag=$(latest_managed_codex_tag)
  if [[ -z "$tag" ]]; then
    printf 'Failed to discover latest pre-release Codex tag.\n' >&2
    exit 1
  fi

  asset=$(managed_codex_asset_name)
  current_tag=""
  if [[ -f "$MANAGED_CODEX_TAG_FILE" ]]; then
    current_tag=$(tr -d '\n' < "$MANAGED_CODEX_TAG_FILE")
  fi

  if [[ -x "$MANAGED_CODEX_BIN" && "$current_tag" == "$tag" ]]; then
    log "managed Codex binary up to date ($tag)"
    return 0
  fi

  log "downloading managed Codex binary ($tag, $asset)"
  tmpdir=$(mktemp -d)

  gh release download "$tag" --repo openai/codex --pattern "$asset" --dir "$tmpdir" --clobber
  archive="$tmpdir/$asset"
  mkdir -p "$tmpdir/extract"
  tar -xzf "$archive" -C "$tmpdir/extract"
  extracted_binary=$(find "$tmpdir/extract" -type f | head -n 1)
  if [[ -z "$extracted_binary" || ! -f "$extracted_binary" ]]; then
    printf 'Failed to locate extracted codex binary inside %s\n' "$asset" >&2
    exit 1
  fi

  install -m 755 "$extracted_binary" "$MANAGED_CODEX_BIN"
  printf '%s\n' "$tag" > "$MANAGED_CODEX_TAG_FILE"
  rm -rf "$tmpdir"
  log "managed Codex binary ready: $MANAGED_CODEX_BIN"
}

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    printf 'Missing required command: %s\n' "$1" >&2
    exit 1
  fi
}

binary_path() {
  case "$BUILD_PROFILE" in
    dev)
      printf '%s\n' "$CARGO_TARGET_DIR_PATH/debug/threadbridge"
      ;;
    release)
      printf '%s\n' "$CARGO_TARGET_DIR_PATH/release/threadbridge"
      ;;
    *)
      printf 'Unsupported BUILD_PROFILE: %s\n' "$BUILD_PROFILE" >&2
      exit 1
      ;;
  esac
}

tmux_session_name() {
  local hash
  hash=$(printf '%s' "$REPO_ROOT" | shasum | awk '{print substr($1, 1, 10)}')
  printf 'threadbridge-%s' "$hash"
}

tmux_session_exists() {
  local session_name=$1
  tmux has-session -t "$session_name" 2>/dev/null
}

tmux_session_pid() {
  local session_name=$1
  tmux list-panes -t "$session_name" -F '#{pane_pid}' 2>/dev/null | head -n 1
}

ensure_layout() {
  mkdir -p "$LOG_DIR" "$REPO_ROOT/data/debug" "$MANAGED_CODEX_DIR"
  touch "$STDOUT_LOG" "$STDERR_LOG" "$EVENT_LOG"
}

ensure_env() {
  if [[ ! -f "$ENV_FILE" ]]; then
    printf 'Missing env file: %s\n' "$ENV_FILE" >&2
    exit 1
  fi

  local token
  token=$(sed -n 's/^TELEGRAM_BOT_TOKEN=//p' "$ENV_FILE" | head -n 1)
  if [[ -z "$token" ]]; then
    printf 'Set TELEGRAM_BOT_TOKEN in %s before starting.\n' "$ENV_FILE" >&2
    exit 1
  fi
}

build_bot() {
  log "building threadbridge binaries ($BUILD_PROFILE)"
  (
    cd "$REPO_ROOT"
    export PATH="$RUNTIME_PATH"
    export CARGO_HOME="$CARGO_HOME_DIR"
    export CARGO_TARGET_DIR="$CARGO_TARGET_DIR_PATH"
    export RUSTUP_HOME="$RUSTUP_HOME_DIR"
    if [[ "$BUILD_PROFILE" == "release" ]]; then
      cargo build --release --bin threadbridge --bin threadbridge_viewer
    else
      cargo build --bin threadbridge --bin threadbridge_viewer
    fi
  )
}

start_bot() {
  ensure_layout
  ensure_env
  require_command cargo
  require_command tmux

  ensure_managed_codex_binary
  build_bot

  local bot_binary
  bot_binary=$(binary_path)
  if [[ ! -x "$bot_binary" ]]; then
    printf 'Missing built binary: %s\n' "$bot_binary" >&2
    exit 1
  fi

  local session_name
  session_name=$(tmux_session_name)
  if tmux_session_exists "$session_name"; then
    log "stopping existing tmux session: $session_name"
    tmux kill-session -t "$session_name"
    sleep 1
  fi

  local launch_command
  launch_command=$(printf 'cd %q && export PATH=%q CARGO_HOME=%q CARGO_TARGET_DIR=%q RUSTUP_HOME=%q && set -a && source %q && set +a && exec %q >>%q 2>>%q' \
    "$REPO_ROOT" \
    "$RUNTIME_PATH" \
    "$CARGO_HOME_DIR" \
    "$CARGO_TARGET_DIR_PATH" \
    "$RUSTUP_HOME_DIR" \
    "$ENV_FILE" \
    "$bot_binary" \
    "$STDOUT_LOG" \
    "$STDERR_LOG")
  tmux new-session -d -s "$session_name" "$(printf 'bash -lc %q' "$launch_command")"

  sleep 3
  if ! tmux_session_exists "$session_name"; then
    log "threadbridge failed to start"
    tail -n 80 "$STDERR_LOG" || true
    exit 1
  fi

  log "threadbridge started in tmux session: $session_name"
  status_bot
}

stop_bot() {
  local session_name
  session_name=$(tmux_session_name)

  if ! tmux_session_exists "$session_name"; then
    log "threadbridge is not running"
    return 0
  fi

  tmux kill-session -t "$session_name"
  log "threadbridge stopped"
}

status_bot() {
  local session_name
  session_name=$(tmux_session_name)

  if ! tmux_session_exists "$session_name"; then
    log "threadbridge is not running"
  else
    local pane_pid
    pane_pid=$(tmux_session_pid "$session_name")
    log "threadbridge running in tmux session: $session_name"
    if [[ -n "$pane_pid" ]]; then
      log "tmux pane PID: $pane_pid"
    fi
  fi

  if [[ -f "$EVENT_LOG" ]]; then
    log "recent events"
    tail -n 20 "$EVENT_LOG" || true
  fi
}

logs_bot() {
  ensure_layout
  local session_name
  session_name=$(tmux_session_name)

  if tmux_session_exists "$session_name"; then
    log "tmux pane"
    tmux capture-pane -p -t "$session_name" -S -40 || true
  fi

  log "stdout"
  tail -n 40 "$STDOUT_LOG" || true
  log "stderr"
  tail -n 40 "$STDERR_LOG" || true
  log "events"
  tail -n 40 "$EVENT_LOG" || true
}

main() {
  local command=${1:-}
  case "$command" in
    start)
      start_bot
      ;;
    stop)
      stop_bot
      ;;
    restart)
      stop_bot
      start_bot
      ;;
    status)
      status_bot
      ;;
    logs)
      logs_bot
      ;;
    *)
      usage
      exit 1
      ;;
  esac
}

main "$@"
