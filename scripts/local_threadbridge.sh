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
MANAGED_CODEX_SOURCE_FILE="$MANAGED_CODEX_DIR/source.txt"
MANAGED_CODEX_BUILD_INFO_FILE="$MANAGED_CODEX_DIR/build-info.txt"
CODEX_SOURCE_REPO="${CODEX_SOURCE_REPO:-/Volumes/Data/Github/codex}"
CODEX_SOURCE_RS_DIR="${CODEX_SOURCE_RS_DIR:-$CODEX_SOURCE_REPO/codex-rs}"
CODEX_BUILD_PROFILE="${CODEX_BUILD_PROFILE:-$BUILD_PROFILE}"
CODEX_CARGO_HOME_DIR="${CODEX_CARGO_HOME:-$HOME/.cargo}"
CODEX_CARGO_TARGET_DIR_PATH="${CODEX_CARGO_TARGET_DIR:-$CODEX_SOURCE_RS_DIR/target}"
CODEX_RUSTUP_HOME_DIR="${CODEX_RUSTUP_HOME:-$RUSTUP_HOME_DIR}"

usage() {
  cat <<'EOF'
Usage: local_threadbridge.sh <command> [--codex-source brew|source]

Commands:
  start
  stop
  restart
  status
  logs

Options:
  --codex-source brew|source  Choose which local codex binary hcodex should prefer.
                              The choice is persisted in .threadbridge/codex/source.txt.

Environment overrides:
  BUILD_PROFILE=dev|release      Build profile to run threadBridge. Default: dev
  CODEX_BUILD_PROFILE=dev|release
                                 Build profile for source-built Codex. Default: BUILD_PROFILE
  CODEX_SOURCE_REPO=/abs/path    Codex repo root. Default: /Volumes/Data/Github/codex
  CODEX_SOURCE_RS_DIR=/abs/path  Codex Rust workspace. Default: $CODEX_SOURCE_REPO/codex-rs
EOF
}

log() {
  printf '[local-threadbridge] %s\n' "$*"
}

read_codex_source_preference() {
  if [[ -f "$MANAGED_CODEX_SOURCE_FILE" ]]; then
    tr -d '\n' < "$MANAGED_CODEX_SOURCE_FILE"
    return 0
  fi
  printf '%s\n' 'brew'
}

write_codex_source_preference() {
  local source=$1
  mkdir -p "$MANAGED_CODEX_DIR"
  printf '%s\n' "$source" > "$MANAGED_CODEX_SOURCE_FILE"
}

resolve_codex_source() {
  local requested=${1:-}
  if [[ -n "$requested" ]]; then
    case "$requested" in
      brew|source)
        printf '%s\n' "$requested"
        return 0
        ;;
      alpha)
        log "codex source 'alpha' is deprecated; using 'source' instead"
        printf '%s\n' 'source'
        return 0
        ;;
      *)
        printf 'Unsupported codex source: %s\n' "$requested" >&2
        exit 1
        ;;
    esac
  fi

  local persisted
  persisted=$(read_codex_source_preference)
  case "$persisted" in
    brew|source)
      printf '%s\n' "$persisted"
      ;;
    alpha)
      printf '%s\n' 'source'
      ;;
    *)
      printf '%s\n' 'brew'
      ;;
  esac
}

ensure_source_codex_binary() {
  require_command cargo

  if [[ ! -d "$CODEX_SOURCE_RS_DIR" ]]; then
    printf 'Missing Codex source workspace: %s\n' "$CODEX_SOURCE_RS_DIR" >&2
    exit 1
  fi

  if [[ ! -f "$CODEX_SOURCE_RS_DIR/Cargo.toml" ]]; then
    printf 'Missing Codex Cargo.toml: %s\n' "$CODEX_SOURCE_RS_DIR/Cargo.toml" >&2
    exit 1
  fi

  mkdir -p "$MANAGED_CODEX_DIR"

  local source_binary profile_flag build_info git_rev
  case "$CODEX_BUILD_PROFILE" in
    dev)
      profile_flag=""
      source_binary="$CODEX_CARGO_TARGET_DIR_PATH/debug/codex"
      ;;
    release)
      profile_flag="--release"
      source_binary="$CODEX_CARGO_TARGET_DIR_PATH/release/codex"
      ;;
    *)
      printf 'Unsupported CODEX_BUILD_PROFILE: %s\n' "$CODEX_BUILD_PROFILE" >&2
      exit 1
      ;;
  esac

  log "building Codex from source ($CODEX_BUILD_PROFILE): $CODEX_SOURCE_RS_DIR"
  (
    cd "$CODEX_SOURCE_RS_DIR"
    export CARGO_HOME="$CODEX_CARGO_HOME_DIR"
    export CARGO_TARGET_DIR="$CODEX_CARGO_TARGET_DIR_PATH"
    export RUSTUP_HOME="$CODEX_RUSTUP_HOME_DIR"
    if [[ -n "$profile_flag" ]]; then
      cargo build "$profile_flag" -p codex-cli
    else
      cargo build -p codex-cli
    fi
  )

  if [[ ! -x "$source_binary" ]]; then
    printf 'Expected built Codex binary at %s\n' "$source_binary" >&2
    exit 1
  fi

  install -m 755 "$source_binary" "$MANAGED_CODEX_BIN"
  git_rev=$(git -C "$CODEX_SOURCE_REPO" rev-parse --short HEAD 2>/dev/null || printf 'unknown')
  build_info=$(cat <<EOF
source_repo=$CODEX_SOURCE_REPO
source_rs_dir=$CODEX_SOURCE_RS_DIR
build_profile=$CODEX_BUILD_PROFILE
git_rev=$git_rev
binary=$source_binary
EOF
)
  printf '%s\n' "$build_info" > "$MANAGED_CODEX_BUILD_INFO_FILE"
  log "source-built Codex binary ready: $MANAGED_CODEX_BIN"
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
      cargo build --release --bin threadbridge
    else
      cargo build --bin threadbridge
    fi
  )
}

start_bot() {
  local codex_source=${1:-}
  ensure_layout
  ensure_env
  require_command cargo
  require_command tmux

  codex_source=$(resolve_codex_source "$codex_source")
  write_codex_source_preference "$codex_source"

  if [[ "$codex_source" == "source" ]]; then
    ensure_source_codex_binary
  else
    log "using brew/system codex as primary local CLI source"
  fi
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
  log "codex source preference: $codex_source"
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
  local codex_source
  codex_source=$(resolve_codex_source "")

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
  log "codex source preference: $codex_source"
  if [[ "$codex_source" == "source" && -f "$MANAGED_CODEX_BUILD_INFO_FILE" ]]; then
    while IFS= read -r line; do
      [[ -n "$line" ]] && log "managed Codex $line"
    done < "$MANAGED_CODEX_BUILD_INFO_FILE"
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
  local codex_source=""
  shift || true
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --codex-source)
        shift
        if [[ $# -eq 0 ]]; then
          printf 'Missing value for --codex-source\n' >&2
          exit 1
        fi
        codex_source=$1
        ;;
      *)
        printf 'Unknown argument: %s\n' "$1" >&2
        usage
        exit 1
        ;;
    esac
    shift
  done
  case "$command" in
    start)
      start_bot "$codex_source"
      ;;
    stop)
      stop_bot
      ;;
    restart)
      stop_bot
      start_bot "$codex_source"
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
