# Codex Home-Dir Session Mirror 草稿

## 目前進度

這份文檔是純草稿，但 v1 實作已開始。

目前代碼已開始新增第二條 Telegram mirror 線路：

- 從 `${CODEX_HOME:-$HOME/.codex}` 讀 Codex 自己的 session log
- 只用 `session-binding.json` 的 `current_codex_thread_id` 關聯外部 Codex Desktop / Codex TUI 對話
- 不使用 `tui_active_codex_thread_id`
- 不回放任何內容給 app-server
- 不觸發 `turn/start` / `turn/steer`
- 不取代 app-server WSS observer

這條線路的定位是 Telegram read-side projection，不是 runtime control。

## 問題

app-server WSS observer 只能觀測 threadBridge 管理的 ingress / subscribe 線。

如果使用者在 Codex Desktop 或 Codex 自己的 TUI 中直接對同一個 Codex thread 對話，threadBridge 的 app-server observer 不一定能看到這些訊息。但 Codex 本身會把 session log 寫入 user home，例如：

- `${CODEX_HOME:-$HOME/.codex}/session_index.jsonl`
- `${CODEX_HOME:-$HOME/.codex}/sessions/**/rollout-*.jsonl`
- `${CODEX_HOME:-$HOME/.codex}/archived_sessions/**/rollout-*.jsonl`

因此需要第二條 mirror 線路：讀 user home 中的 Codex session log，將新出現的外部 user / assistant final message 投影回 Telegram topic。

## 邊界

這條 mirror 線路必須遵守以下硬邊界：

- 只處理 `current_codex_thread_id`
- 不讀 `tui_active_codex_thread_id`
- 不掃 same-cwd 的所有 Codex sessions
- 不自動 adopt 未 bound session
- 不把讀到的 user message 送回 app-server
- 不參與 `request_user_input` / approval / turn steering
- 不把 raw Codex JSONL log 複製進 repo 或 Telegram

`tui_active_codex_thread_id` 代表 threadBridge 管理的 app-server-launched Codex TUI session；它不是外部 Codex Desktop / Codex TUI 的關聯點。

## Case Findings

以 `macOSAgentBot` workspace 的真實案例確認：

- `thread_key`: `a977bb18-c410-467c-8da9-c18f6c0780a4`
- `workspace`: `/Volumes/Data/Github/macOSAgentBot`
- `current_codex_thread_id`: `019dff0f-ccf1-7852-924d-bb8a1b986ee9`
- `tui_active_codex_thread_id`: `none`

觀察結果：

- `session-binding.json` 的 workspace cwd 與 Codex home log 的 `session_meta.cwd` 一致
- `session_index.jsonl` 沒有該 id，但 `sessions/**/rollout-*019dff0f*.jsonl` 存在，所以 finder 不能只依賴 index
- `response_item.message` 本身沒有 `turn_id`
- 同一 turn 前方的 `event_msg task_started` / `turn_context` 含有 turn id
- threadBridge 的 `delivery.sqlite3` 已有相同 turn id 的 `user_echo` / `assistant_final` delivery claim

因此 parser 必須讀 turn lifecycle，而不是只掃 message rows。

## 設計

### 查找

查找順序：

1. 讀 `session_index.jsonl` 作索引確認
2. 掃 `sessions/**/rollout-*<current_codex_thread_id>*.jsonl`
3. 必要時掃 `archived_sessions/**/rollout-*<current_codex_thread_id>*.jsonl`

找到 log 後必須驗證：

- `session_meta.id` 對應 current session
- `session_meta.cwd` 等於 binding workspace cwd

cwd 不一致時，拒絕 mirror 並記錄 diagnostic。

### Parser

parser 的 turn 規則：

- `event_msg task_started` / `turn_context` 建立 current turn scope
- turn 內第一個非 bootstrap user message 是 user echo candidate
- turn 內 assistant message 只保留最後一個
- 只有看到 `task_complete` 才產生 assistant final candidate
- bootstrap prompt 例如 `# AGENTS.md instructions...` 與 `Read and follow...READY` 不投影

這避免把 assistant commentary / progress updates 當成多個 final reply。

### Cursor

每個 Telegram thread 的 bot-local state 保存：

`state/codex-home-session-mirror.json`

cursor key 使用 `current_codex_thread_id`，內容至少包括：

- `thread_key`
- `session_id`
- `log_path`
- `last_offset`
- `last_line`
- `last_turn_id`
- `updated_at`
- `last_error`

首次啟用時不回填歷史，只 mark observed 到目前 EOF。歷史 backfill 需要獨立 maintenance action，不是 v1 預設。

### 去重

Delivery bus 是 primary dedupe ledger。

主鍵語義：

- `thread_key`
- `current_codex_thread_id`
- `codex_turn_id`
- `DeliveryKind::UserEcho` 或 `DeliveryKind::AssistantFinal`
- `DeliveryChannel::Telegram`

如果 delivery claim 已存在：

- advance cursor
- 不發 Telegram
- 不追加 transcript mirror

如果 delivery claim 不存在：

- append transcript mirror
- user message 用 `› {text}` 投影到 Telegram
- assistant final 走現有 final assistant renderer
- commit delivery attempt

文字 hash / offset 只能作 secondary guard 或 diagnostic，不作 primary 去重主鍵，避免相同文字在不同 turn 中被誤判。

## 與既有計劃關係

- [session-level-mirror-and-readiness.md](../runtime-control/session-level-mirror-and-readiness.md)
  - 保留高層 mirror / readiness 語義
- [session-lifecycle.md](../runtime-control/session-lifecycle.md)
  - 固定 `current_codex_thread_id` 與 `tui_active_codex_thread_id` 的角色區分
- [app-server-ws-mirror-observer.md](../app-server-observer/app-server-ws-mirror-observer.md)
  - 只描述 app-server WSS observer；home-dir mirror 是另一條 Telegram adapter projection
- [message-queue-and-status-delivery.md](message-queue-and-status-delivery.md)
  - home-dir mirror 的 Telegram 發送與去重必須接入 delivery bus
- [runtime-protocol.md](../runtime-control/runtime-protocol.md)
  - v1 不新增 runtime control action；後續可將 `source_ref=codex_home_dir` 暴露到 observability

## 測試要求

- fake Codex home log 能從 `task_started` / `turn_context` 取得 turn id
- 多個 assistant commentary 只產生最後 assistant final
- `session_index` miss 但 filename fallback 成功
- bootstrap user message 被跳過
- repeated identical text in different turns 不互相去重
- existing delivery claim 不重發 Telegram、不追加 transcript
- first enable 不 backfill old history
