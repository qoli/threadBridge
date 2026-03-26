#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd -P)

REMOTE_HOST=${REMOTE_HOST:-}
REMOTE_DIR=${REMOTE_DIR:-}
BUILD_TARGET=${BUILD_TARGET:-x86_64-apple-darwin}
AUTHORIZED_TELEGRAM_USER_IDS=${AUTHORIZED_TELEGRAM_USER_IDS:-}
TELEGRAM_BOT_TOKEN=${TELEGRAM_BOT_TOKEN:-}
REMOTE_PROXY_URL=${REMOTE_PROXY_URL:-http://127.0.0.1:7890}
NANOBANANA_API_ENDPOINT=${NANOBANANA_API_ENDPOINT:-https://banana.147ai.com/v1beta/models/gemini-3.1-flash-image-preview:generateContent}
NANOBANANA_API_KEY=${NANOBANANA_API_KEY:-}
NANOBANANA_API_USER=${NANOBANANA_API_USER:-replace-me}
NANOBANANA_API_ORIGIN=${NANOBANANA_API_ORIGIN:-https://banana.147ai.com}
NANOBANANA_API_REFERER=${NANOBANANA_API_REFERER:-https://banana.147ai.com/}
CARGO_HOME_DIR=${CARGO_HOME:-$REPO_ROOT/.cargo}
CARGO_TARGET_DIR_PATH=${CARGO_TARGET_DIR:-$REPO_ROOT/target}
RUSTUP_HOME_DIR=${RUSTUP_HOME:-$HOME/.rustup}
LOCAL_RUNTIME_PATH="$HOME/.cargo/bin:$PATH"

usage() {
  cat <<'EOF'
Usage: build_and_deploy_threadbridge.sh <command>

Commands:
  deploy   Build locally, sync to remote, write env, stop old bot processes, start new bot
  start    Start the remote bot without rebuilding
  stop     Stop the remote bot
  restart  Restart the remote bot without rebuilding
  status   Show remote bot status and recent events
  logs     Show remote bot stdout, stderr, and event logs
  wipe-data Stop the remote bot, delete remote data/, and recreate an empty data/debug/

Default command: deploy
EOF
}

log() {
  printf '[threadBridge] %s\n' "$*"
}

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    printf 'Missing required command: %s\n' "$1" >&2
    exit 1
  fi
}

require_non_empty_env() {
  local name=${1:?missing env name}
  local value=${2-}
  if [[ -z "$value" ]]; then
    printf 'Set %s before running this script.\n' "$name" >&2
    exit 1
  fi
}

ensure_deploy_config() {
  require_non_empty_env "REMOTE_HOST" "$REMOTE_HOST"
  require_non_empty_env "REMOTE_DIR" "$REMOTE_DIR"
  require_non_empty_env "TELEGRAM_BOT_TOKEN" "$TELEGRAM_BOT_TOKEN"
  require_non_empty_env "AUTHORIZED_TELEGRAM_USER_IDS" "$AUTHORIZED_TELEGRAM_USER_IDS"
  require_non_empty_env "NANOBANANA_API_KEY" "$NANOBANANA_API_KEY"
}

ensure_remote_layout() {
  ssh "$REMOTE_HOST" \
    "mkdir -p '$REMOTE_DIR' '$REMOTE_DIR/bin' '$REMOTE_DIR/data/debug' '$REMOTE_DIR/logs' '$REMOTE_DIR/templates' '$REMOTE_DIR/tools'"
}

write_remote_env_file() {
  ssh "$REMOTE_HOST" "cat > '$REMOTE_DIR/.env.local' <<'EOF'
TELEGRAM_BOT_TOKEN=$TELEGRAM_BOT_TOKEN
AUTHORIZED_TELEGRAM_USER_IDS=$AUTHORIZED_TELEGRAM_USER_IDS
DATA_ROOT=./data
DEBUG_LOG_PATH=./data/debug/events.jsonl
CODEX_WORKING_DIRECTORY=.
CODEX_MODEL=
STREAM_EDIT_INTERVAL_MS=750
STREAM_MESSAGE_MAX_CHARS=3500
COMMAND_OUTPUT_TAIL_CHARS=800
NANOBANANA_API_ENDPOINT=$NANOBANANA_API_ENDPOINT
NANOBANANA_API_KEY=$NANOBANANA_API_KEY
NANOBANANA_API_USER=$NANOBANANA_API_USER
NANOBANANA_API_ORIGIN=$NANOBANANA_API_ORIGIN
NANOBANANA_API_REFERER=$NANOBANANA_API_REFERER
REMOTE_PROXY_URL=$REMOTE_PROXY_URL
EOF"
}

remote_manage() {
  local command=${1:?missing remote command}
  ssh "$REMOTE_HOST" /bin/bash -s -- "$REMOTE_DIR" "$command" "$REMOTE_PROXY_URL" <<'EOF'
set -euo pipefail

APP_ROOT=$(mkdir -p "$1" && cd "$1" && pwd -P)
COMMAND=$2
DEFAULT_PROXY_URL=$3
BOT_BINARY="$APP_ROOT/threadbridge"
ENV_FILE="$APP_ROOT/.env.local"
LOG_DIR="$APP_ROOT/logs"
STDOUT_LOG="$LOG_DIR/threadbridge.stdout.log"
STDERR_LOG="$LOG_DIR/threadbridge.stderr.log"
EVENT_LOG_DIR="$APP_ROOT/data/debug"
RUNTIME_PATH="$APP_ROOT/bin:$HOME/.local/mamba-envs/codex-tools/bin:$HOME/.local/bin:$PATH"

log() {
  printf '[remote-threadBridge] %s\n' "$*"
}

latest_event_log() {
  find "$EVENT_LOG_DIR" -maxdepth 1 -type f -name 'events-*.jsonl' -print 2>/dev/null | sort | tail -n 1
}

process_cwd() {
  local pid=$1
  local cwd
  cwd=$(lsof -a -p "$pid" -d cwd 2>/dev/null | awk 'NR==2 {print $NF}')
  if [[ -n "$cwd" && -d "$cwd" ]]; then
    (cd "$cwd" && pwd -P)
  else
    printf '%s\n' "$cwd"
  fi
}

bot_pids() {
  (pgrep -x threadbridge 2>/dev/null || true) | while IFS= read -r pid; do
    [[ -n "$pid" ]] || continue
    if [[ "$(process_cwd "$pid")" == "$APP_ROOT" ]]; then
      printf '%s\n' "$pid"
    fi
  done
}

kill_bot_processes() {
  local pids
  pids=$(bot_pids)
  if [[ -z "$pids" ]]; then
    return 0
  fi

  log "stopping existing bot process(es): $(echo "$pids" | tr '\n' ' ')"
  while IFS= read -r pid; do
    [[ -n "$pid" ]] || continue
    kill "$pid" || true
  done <<< "$pids"
  sleep 2

  local remaining
  remaining=$(bot_pids)
  if [[ -n "$remaining" ]]; then
    log "force killing lingering bot process(es): $(echo "$remaining" | tr '\n' ' ')"
    while IFS= read -r pid; do
      [[ -n "$pid" ]] || continue
      kill -9 "$pid" || true
    done <<< "$remaining"
    sleep 1
  fi
}

ensure_layout() {
  mkdir -p "$APP_ROOT/data/debug" "$LOG_DIR"
  touch "$STDOUT_LOG" "$STDERR_LOG"
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

start_bot() {
  ensure_layout
  ensure_env
  kill_bot_processes

  local proxy_url
  proxy_url=$(sed -n 's/^REMOTE_PROXY_URL=//p' "$ENV_FILE" | head -n 1)
  proxy_url=${proxy_url:-$DEFAULT_PROXY_URL}

  nohup env \
    PATH="$RUNTIME_PATH" \
    ALL_PROXY="$proxy_url" \
    all_proxy="$proxy_url" \
    HTTP_PROXY="$proxy_url" \
    http_proxy="$proxy_url" \
    HTTPS_PROXY="$proxy_url" \
    https_proxy="$proxy_url" \
    NO_PROXY="localhost,127.0.0.1,::1" \
    no_proxy="localhost,127.0.0.1,::1" \
    bash -lc "cd '$APP_ROOT' && set -a && source '$ENV_FILE' && set +a && exec '$BOT_BINARY'" \
    >"$STDOUT_LOG" 2>"$STDERR_LOG" < /dev/null &

  sleep 5
  if [[ -z "$(bot_pids)" ]]; then
    log "threadbridge failed to start"
    tail -n 80 "$STDERR_LOG" || true
    exit 1
  fi

  log "threadbridge started"
  status_bot
}

stop_bot() {
  if [[ -z "$(bot_pids)" ]]; then
    log "threadbridge is not running"
    return 0
  fi

  kill_bot_processes
  log "threadbridge stopped"
}

status_bot() {
  local pids
  pids=$(bot_pids)
  if [[ -z "$pids" ]]; then
    log "threadbridge is not running"
  else
    log "threadbridge running with PID(s): $(echo "$pids" | tr '\n' ' ')"
  fi

  local event_log
  event_log=$(latest_event_log)
  if [[ -n "$event_log" ]]; then
    log "recent events"
    tail -n 20 "$event_log" || true
  fi
}

logs_bot() {
  ensure_layout
  log "stdout"
  tail -n 40 "$STDOUT_LOG" || true
  log "stderr"
  tail -n 40 "$STDERR_LOG" || true
  local event_log
  event_log=$(latest_event_log)
  if [[ -n "$event_log" ]]; then
    log "events"
    tail -n 40 "$event_log" || true
  fi
}

wipe_data() {
  stop_bot
  rm -rf "$APP_ROOT/data"
  mkdir -p "$APP_ROOT/data/debug"
  log "remote data directory wiped: $APP_ROOT/data"
}

case "$COMMAND" in
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
  wipe-data)
    wipe_data
    ;;
  *)
    printf 'Unsupported remote command: %s\n' "$COMMAND" >&2
    exit 1
    ;;
esac
EOF
}

build_local_binary() {
  require_command cargo
  log "building threadbridge for $BUILD_TARGET"
  (
    cd "$REPO_ROOT"
    export PATH="$LOCAL_RUNTIME_PATH"
    export CARGO_HOME="$CARGO_HOME_DIR"
    export CARGO_TARGET_DIR="$CARGO_TARGET_DIR_PATH"
    export RUSTUP_HOME="$RUSTUP_HOME_DIR"
    cargo build --release --target "$BUILD_TARGET" --bin threadbridge
  )
}

sync_remote_runtime() {
  ensure_remote_layout
  log "syncing runtime files to $REMOTE_HOST:$REMOTE_DIR"
  rsync -az --delete \
    "$REPO_ROOT/target/$BUILD_TARGET/release/threadbridge" \
    "$REPO_ROOT/.env.example" \
    "$REPO_ROOT/README.md" \
    "$REMOTE_HOST:$REMOTE_DIR/"
  rsync -az --delete \
    --exclude '__pycache__' \
    "$REPO_ROOT/templates/" \
    "$REMOTE_HOST:$REMOTE_DIR/templates/"
  rsync -az --delete \
    --exclude '__pycache__' \
    "$REPO_ROOT/tools/" \
    "$REMOTE_HOST:$REMOTE_DIR/tools/"
  rsync -az \
    "$REPO_ROOT/scripts/remote_codex_wrapper.sh" \
    "$REMOTE_HOST:$REMOTE_DIR/bin/codex"
  ssh "$REMOTE_HOST" "chmod +x '$REMOTE_DIR/threadbridge' '$REMOTE_DIR/bin/codex'"
}

deploy() {
  require_command ssh
  require_command rsync
  build_local_binary
  sync_remote_runtime
  log "writing remote .env.local"
  write_remote_env_file
  log "starting remote bot"
  remote_manage restart
  log "deploy complete"
}

main() {
  local command=${1:-deploy}
  case "$command" in
    deploy)
      ensure_deploy_config
      deploy
      ;;
    start|stop|restart|status|logs|wipe-data)
      require_command ssh
      remote_manage "$command"
      ;;
    *)
      usage
      exit 1
      ;;
  esac
}

main "$@"
