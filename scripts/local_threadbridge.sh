#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd -P)
ENV_FILE="$REPO_ROOT/data/config.env.local"
LOG_DIR="$REPO_ROOT/logs"
CARGO_HOME_DIR="${CARGO_HOME:-$REPO_ROOT/.cargo}"
CARGO_TARGET_DIR_PATH="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"
BUILD_PROFILE="${BUILD_PROFILE:-dev}"
RUSTUP_HOME_DIR="${RUSTUP_HOME:-$HOME/.rustup}"
RUNTIME_PATH="$CARGO_HOME_DIR/bin:$HOME/.cargo/bin:$REPO_ROOT/bin:$PATH"
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
  build
  bundle
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
  DATA_ROOT=/abs/path            Override the runtime data root for either build profile
  DEBUG_LOG_PATH=/abs/path       Override the runtime event log path for either build profile
EOF
}

log() {
  printf '[local-threadbridge] %s\n' "$*"
}

managed_codex_dir() {
  printf '%s/.threadbridge/codex\n' "$(runtime_data_root)"
}

managed_codex_bin() {
  printf '%s/codex\n' "$(managed_codex_dir)"
}

managed_codex_source_file() {
  printf '%s/source.txt\n' "$(managed_codex_dir)"
}

managed_codex_build_info_file() {
  printf '%s/build-info.txt\n' "$(managed_codex_dir)"
}

read_codex_source_preference() {
  local source_file
  source_file=$(managed_codex_source_file)
  if [[ -f "$source_file" ]]; then
    tr -d '\n' < "$source_file"
    return 0
  fi
  printf '%s\n' 'brew'
}

write_codex_source_preference() {
  local source=$1
  local dir source_file
  dir=$(managed_codex_dir)
  source_file=$(managed_codex_source_file)
  mkdir -p "$dir"
  printf '%s\n' "$source" > "$source_file"
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

  local managed_dir managed_bin build_info_file
  managed_dir=$(managed_codex_dir)
  managed_bin=$(managed_codex_bin)
  build_info_file=$(managed_codex_build_info_file)
  mkdir -p "$managed_dir"

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

  install -m 755 "$source_binary" "$managed_bin"
  git_rev=$(git -C "$CODEX_SOURCE_REPO" rev-parse --short HEAD 2>/dev/null || printf 'unknown')
  build_info=$(cat <<EOF
source_repo=$CODEX_SOURCE_REPO
source_rs_dir=$CODEX_SOURCE_RS_DIR
build_profile=$CODEX_BUILD_PROFILE
git_rev=$git_rev
binary=$source_binary
EOF
)
  printf '%s\n' "$build_info" > "$build_info_file"
  log "source-built Codex binary ready: $managed_bin"
}

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    printf 'Missing required command: %s\n' "$1" >&2
    exit 1
  fi
}

profile_output_dir() {
  case "$BUILD_PROFILE" in
    dev)
      printf '%s\n' "$CARGO_TARGET_DIR_PATH/debug"
      ;;
    release)
      printf '%s\n' "$CARGO_TARGET_DIR_PATH/release"
      ;;
    *)
      printf 'Unsupported BUILD_PROFILE: %s\n' "$BUILD_PROFILE" >&2
      exit 1
      ;;
  esac
}

binary_path() {
  local bin_name=${1:?missing bin name}
  printf '%s/%s\n' "$(profile_output_dir)" "$bin_name"
}

should_build_desktop() {
  [[ "$(uname -s)" == "Darwin" ]]
}

require_desktop_runtime() {
  if ! should_build_desktop; then
    printf 'threadbridge now requires the macOS desktop runtime; desktop is only available on macOS\n' >&2
    exit 1
  fi
}

runtime_binary_name() {
  printf '%s\n' 'threadbridge_desktop'
}

bundle_app_path() {
  printf '%s/bundle/osx/threadBridge.app\n' "$(profile_output_dir)"
}

bundle_runtime_binary_path() {
  printf '%s/Contents/MacOS/%s\n' "$(bundle_app_path)" "$(runtime_binary_name)"
}

runtime_binary_names() {
  printf '%s\n' \
    'threadbridge_desktop' \
    'app_server_ws_worker'
}

ensure_runtime_binaries_built() {
  local missing=0
  local runtime_bin runtime_path
  while IFS= read -r runtime_bin; do
    [[ -n "$runtime_bin" ]] || continue
    runtime_path=$(binary_path "$runtime_bin")
    if [[ ! -x "$runtime_path" ]]; then
      printf 'Missing built runtime binary: %s\n' "$runtime_path" >&2
      missing=1
    fi
  done < <(runtime_binary_names)

  if [[ $missing -ne 0 ]]; then
    printf 'Run scripts/local_threadbridge.sh build to compile runtime binaries.\n' >&2
    exit 1
  fi
}

stdout_log_path() {
  printf '%s/local-threadbridge-desktop.stdout.log\n' "$LOG_DIR"
}

stderr_log_path() {
  printf '%s/local-threadbridge-desktop.stderr.log\n' "$LOG_DIR"
}

tmux_session_name() {
  local hash
  hash=$(printf '%s' "$REPO_ROOT" | shasum | awk '{print substr($1, 1, 10)}')
  printf 'threadbridge-%s-desktop' "$hash"
}

latest_event_log() {
  local event_log_dir
  event_log_dir=$(runtime_event_log_dir)
  find "$event_log_dir" -maxdepth 1 -type f -name 'events-*.jsonl' -print 2>/dev/null | sort | tail -n 1
}

default_runtime_data_root() {
  if [[ "$BUILD_PROFILE" == "release" ]]; then
    printf '%s/Library/Application Support/threadBridge/data\n' "$HOME"
  else
    printf '%s/data\n' "$REPO_ROOT"
  fi
}

default_runtime_debug_log_path() {
  printf '%s/debug/events.jsonl\n' "$(default_runtime_data_root)"
}

resolve_runtime_path() {
  local raw_path=$1
  case "$raw_path" in
    /*)
      printf '%s\n' "$raw_path"
      ;;
    "~/"*)
      printf '%s/%s\n' "$HOME" "${raw_path#~/}"
      ;;
    ./*)
      printf '%s/%s\n' "$REPO_ROOT" "${raw_path#./}"
      ;;
    *)
      printf '%s/%s\n' "$REPO_ROOT" "$raw_path"
      ;;
  esac
}

runtime_env_override() {
  local name=$1
  if [[ -n "${!name:-}" ]]; then
    printf '%s\n' "${!name}"
    return 0
  fi
  if [[ ! -f "$ENV_FILE" ]]; then
    return 0
  fi
  /bin/bash -lc 'set -a; source "$1" >/dev/null 2>&1; printf "%s" "${!2:-}"' _ "$ENV_FILE" "$name"
}

runtime_data_root() {
  local override
  override=$(runtime_env_override DATA_ROOT)
  if [[ -n "$override" ]]; then
    resolve_runtime_path "$override"
    return 0
  fi
  default_runtime_data_root
}

runtime_debug_log_path() {
  local override
  override=$(runtime_env_override DEBUG_LOG_PATH)
  if [[ -n "$override" ]]; then
    resolve_runtime_path "$override"
    return 0
  fi
  default_runtime_debug_log_path
}

runtime_event_log_dir() {
  dirname "$(runtime_debug_log_path)"
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
  mkdir -p "$LOG_DIR" "$(runtime_event_log_dir)" "$(managed_codex_dir)" "$(dirname "$ENV_FILE")"
  touch \
    "$(stdout_log_path)" \
    "$(stderr_log_path)"
}

build_runtime_binaries() {
  require_desktop_runtime
  local build_args=(build)
  if [[ "$BUILD_PROFILE" == "release" ]]; then
    build_args+=(--release)
  fi
  build_args+=(--bins)

  log "building threadbridge runtime binaries ($BUILD_PROFILE)"
  (
    cd "$REPO_ROOT"
    export PATH="$RUNTIME_PATH"
    export CARGO_HOME="$CARGO_HOME_DIR"
    export CARGO_TARGET_DIR="$CARGO_TARGET_DIR_PATH"
    export RUSTUP_HOME="$RUSTUP_HOME_DIR"
    cargo "${build_args[@]}"
  )

  ensure_runtime_binaries_built
  local runtime_bin
  while IFS= read -r runtime_bin; do
    [[ -n "$runtime_bin" ]] || continue
    log "built binary: $(binary_path "$runtime_bin")"
  done < <(runtime_binary_names)
}

build_app_icon_assets() {
  require_desktop_runtime
  local icon_script="$REPO_ROOT/scripts/build_macos_app_icon.sh"
  if [[ ! -x "$icon_script" ]]; then
    printf 'Missing app icon build script: %s\n' "$icon_script" >&2
    exit 1
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

build_runtime_bundle() {
  require_desktop_runtime
  ensure_cargo_bundle_installed

  log "bundling threadBridge app ($BUILD_PROFILE)"
  (
    cd "$REPO_ROOT"
    export PATH="$RUNTIME_PATH"
    export CARGO_HOME="$CARGO_HOME_DIR"
    export CARGO_TARGET_DIR="$CARGO_TARGET_DIR_PATH"
    export RUSTUP_HOME="$RUSTUP_HOME_DIR"
    if [[ "$BUILD_PROFILE" == "release" ]]; then
      cargo bundle --release --bin threadbridge_desktop
    else
      cargo bundle --bin threadbridge_desktop
    fi
  )

  local app_path
  app_path=$(bundle_app_path)
  if [[ ! -d "$app_path" ]]; then
    printf 'Expected app bundle at %s\n' "$app_path" >&2
    exit 1
  fi
  sync_runtime_assets_into_bundle "$app_path"
  local worker_binary worker_dest
  worker_binary=$(binary_path "app_server_ws_worker")
  worker_dest="$app_path/Contents/MacOS/app_server_ws_worker"
  install -m 755 "$worker_binary" "$worker_dest"
  log "bundle ready: $app_path"
}

sync_runtime_assets_into_bundle() {
  local app_path=$1
  local resources_dir runtime_assets_dest
  resources_dir="$app_path/Contents/Resources"
  runtime_assets_dest="$resources_dir/runtime_assets"
  mkdir -p "$resources_dir"
  rm -rf "$runtime_assets_dest"
  cp -R "$REPO_ROOT/runtime_assets" "$runtime_assets_dest"
}

build_local() {
  local codex_source=${1:-}
  ensure_layout
  require_command cargo

  codex_source=$(resolve_codex_source "$codex_source")
  write_codex_source_preference "$codex_source"

  if [[ "$codex_source" == "source" ]]; then
    ensure_source_codex_binary
  else
    log "using brew/system codex as primary local CLI source"
  fi

  build_app_icon_assets
  build_runtime_binaries
}

bundle_local() {
  local codex_source=${1:-}
  ensure_layout
  require_command cargo
  require_desktop_runtime

  codex_source=$(resolve_codex_source "$codex_source")
  write_codex_source_preference "$codex_source"

  if [[ "$codex_source" == "source" ]]; then
    ensure_source_codex_binary
  else
    log "using brew/system codex as primary local CLI source"
  fi

  build_app_icon_assets
  build_runtime_binaries
  build_runtime_bundle
}

start_runtime() {
  local codex_source=${1:-}
  ensure_layout
  require_command cargo
  require_command tmux
  require_desktop_runtime

  codex_source=$(resolve_codex_source "$codex_source")
  write_codex_source_preference "$codex_source"

  if [[ "$codex_source" == "source" ]]; then
    ensure_source_codex_binary
  else
    log "using brew/system codex as primary local CLI source"
  fi
  build_app_icon_assets
  build_runtime_binaries
  build_runtime_bundle

  local runtime_binary_name_value runtime_binary stdout_log stderr_log
  runtime_binary_name_value=$(runtime_binary_name)
  runtime_binary=$(bundle_runtime_binary_path)
  stdout_log=$(stdout_log_path)
  stderr_log=$(stderr_log_path)

  local session_name
  session_name=$(tmux_session_name)
  if tmux_session_exists "$session_name"; then
    log "stopping existing tmux session: $session_name"
    tmux kill-session -t "$session_name"
    sleep 1
  fi

  local launch_command
  launch_command=$(printf 'cd %q && export PATH=%q CARGO_HOME=%q CARGO_TARGET_DIR=%q RUSTUP_HOME=%q && if [[ -f %q ]]; then set -a && source %q && set +a; fi && export DATA_ROOT="${DATA_ROOT:-%q}" DEBUG_LOG_PATH="${DEBUG_LOG_PATH:-%q}" && exec %q >>%q 2>>%q' \
    "$REPO_ROOT" \
    "$RUNTIME_PATH" \
    "$CARGO_HOME_DIR" \
    "$CARGO_TARGET_DIR_PATH" \
    "$RUSTUP_HOME_DIR" \
    "$ENV_FILE" \
    "$ENV_FILE" \
    "$(default_runtime_data_root)" \
    "$(default_runtime_debug_log_path)" \
    "$runtime_binary" \
    "$stdout_log" \
    "$stderr_log")
  tmux new-session -d -s "$session_name" "$(printf 'bash -lc %q' "$launch_command")"

  sleep 3
  if ! tmux_session_exists "$session_name"; then
    log "threadbridge failed to start"
    tail -n 80 "$stderr_log" || true
    exit 1
  fi

  log "desktop runtime started in tmux session: $session_name"
  log "codex source preference: $codex_source"
  status_runtime
}

stop_runtime() {
  local session_name
  session_name=$(tmux_session_name)

  if ! tmux_session_exists "$session_name"; then
    log "desktop runtime is not running"
    return 0
  fi

  tmux kill-session -t "$session_name"
  log "desktop runtime stopped"
}

status_runtime() {
  require_desktop_runtime
  local session_name
  session_name=$(tmux_session_name)
  local codex_source
  codex_source=$(resolve_codex_source "")

  if ! tmux_session_exists "$session_name"; then
    log "desktop runtime is not running"
  else
    local pane_pid
    pane_pid=$(tmux_session_pid "$session_name")
    log "desktop runtime running in tmux session: $session_name"
    if [[ -n "$pane_pid" ]]; then
      log "tmux pane PID: $pane_pid"
    fi
  fi
  log "codex source preference: $codex_source"
  local build_info_file
  build_info_file=$(managed_codex_build_info_file)
  if [[ "$codex_source" == "source" && -f "$build_info_file" ]]; then
    while IFS= read -r line; do
      [[ -n "$line" ]] && log "managed Codex $line"
    done < "$build_info_file"
  fi

  local event_log
  event_log=$(latest_event_log)
  if [[ -n "$event_log" ]]; then
    log "recent events"
    tail -n 20 "$event_log" || true
  fi
}

logs_runtime() {
  require_desktop_runtime
  ensure_layout
  local session_name
  session_name=$(tmux_session_name)
  local stdout_log stderr_log
  stdout_log=$(stdout_log_path)
  stderr_log=$(stderr_log_path)

  if tmux_session_exists "$session_name"; then
    log "tmux pane"
    tmux capture-pane -p -t "$session_name" -S -40 || true
  fi

  log "stdout"
  tail -n 40 "$stdout_log" || true
  log "stderr"
  tail -n 40 "$stderr_log" || true
  local event_log
  event_log=$(latest_event_log)
  if [[ -n "$event_log" ]]; then
    log "events"
    tail -n 40 "$event_log" || true
  fi
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
    build)
      build_local "$codex_source"
      ;;
    bundle)
      bundle_local "$codex_source"
      ;;
    start)
      start_runtime "$codex_source"
      ;;
    stop)
      stop_runtime
      ;;
    restart)
      stop_runtime
      start_runtime "$codex_source"
      ;;
    status)
      status_runtime
      ;;
    logs)
      logs_runtime
      ;;
    *)
      usage
      exit 1
      ;;
  esac
}

main "$@"
