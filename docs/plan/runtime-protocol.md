# Runtime 協議草稿

## 目前進度

這份文檔目前仍是草稿，但已不只是命名建議；其中一部分 view / action 已經掛進本地 management API 和 desktop runtime。

目前代碼狀態：

- Telegram bot 仍是主要聊天入口
- 本地 management API 已開始提供 query / control / SSE：
  - setup
  - runtime health
  - active thread list
  - managed workspace list
  - archived thread list
  - create / bind / reconnect / archive / restore
  - `launch_hcodex_new` / `launch_hcodex_resume`
- `threadbridge_desktop` 已開始直接依賴這些本地 view / action
- transport-neutral 的正式 view / action 命名仍未完全收斂
- local HTTP + SSE 已成為目前最務實的實驗載體

## 問題

如果 `threadBridge` 要同時支撐：

- Telegram adapter
- 本地 tray / web 管理面
- 未來的 observability 面

那就不能再只靠內部 callback 與隱性資料模型耦合。

還需要一份清楚的協議定義，回答：

- 外部客戶端如何查詢 thread / workspace / runtime 狀態
- runtime 如何回報執行中的事件
- control action 如何以平台無關的語義表示
- machine-level runtime 與 managed Codex 狀態如何正式對外暴露

## 方向

先定一套 `threadBridge runtime protocol`。

這份 protocol 的第一步不是定傳輸細節，而是先固定：

- view model
- event type
- control action
- 資料來源與 source of truth

傳輸層在 v1 先用：

- local HTTP
- SSE

## 協議目標

### 主要目標

- 讓 Telegram、tray/web 管理面、未來 observability 共用同一套 runtime 語意
- 讓 thread / workspace / runtime owner / managed Codex 有一致的 view model
- 讓 query、control、event stream 三條線清楚分離

### 次要目標

- 讓 transport 替換時不需要重寫核心流程
- 讓 UI 不需要直接讀 `data/*.json`

## 建議的 view model

### 1. `ThreadStateView`

至少包含：

- `thread_key`
- `title`
- `workspace_cwd`
- `binding_status`
- `run_status`
- `current_codex_thread_id`
- `tui_active_codex_thread_id`
- `tui_session_adoption_pending`
- `archived_at`
- `last_used_at`
- `last_error`

### 2. `ArchivedThreadView`

至少包含：

- `thread_key`
- `title`
- `workspace_cwd`
- `archived_at`
- `previous_message_thread_ids`

### 3. `ManagedWorkspaceView`

至少包含：

- `workspace_cwd`
- `title`
- `thread_key`
- `binding_status`
- `run_status`
- `current_codex_thread_id`
- `tui_active_codex_thread_id`
- `tui_session_adoption_pending`
- `recent_codex_sessions`
- `conflict`
- `last_used_at`

### 4. `RecentCodexSessionView`

至少包含：

- `session_id`
- `updated_at`

### 5. `ManagedCodexView`

至少包含：

- `binary_path`
- `binary_ready`
- `source`
- `version` 或 `revision`
- `handoff_supported`

### 6. `RuntimeHealthView`

至少包含：

- `managed_codex`
- `app_server_status`
- `tui_proxy_status`
- `handoff_readiness`
- `broken_threads`
- `running_workspaces`
- `conflicted_workspaces`

### 7. `SetupStateView`

至少包含：

- `telegram_configured`
- `authorized_user_count`
- `telegram_polling_state`
- `restart_required_after_setup_save`

## 建議的 control action

這一層應避免 Telegram 或 UI 專屬命名。

v1 至少定義：

- `create_thread`
- `bind_workspace`
- `reconnect_codex`
- `open_workspace`
- `adopt_tui_session`
- `reject_tui_session`
- `launch_hcodex_new`
- `launch_hcodex_resume`
- `archive_thread`
- `restore_thread`
- `update_managed_codex`
- `build_managed_codex_source`
- `save_telegram_setup`

目前已部分對應到代碼中的 local HTTP endpoint：

- `POST /api/threads`
- `POST /api/threads/create-and-bind`
- `POST /api/threads/:thread_key/bind-workspace`
- `POST /api/threads/:thread_key/adopt-tui`
- `POST /api/threads/:thread_key/reject-tui`
- `POST /api/workspaces/:thread_key/reconnect`
- `POST /api/workspaces/:thread_key/open`
- `POST /api/workspaces/:thread_key/launch-new`
- `POST /api/workspaces/:thread_key/launch-resume`
- `POST /api/workspaces/:thread_key/repair-runtime`
- `POST /api/threads/:thread_key/archive`
- `POST /api/threads/:thread_key/restore`
- `PUT /api/setup/telegram`
- `POST /api/managed-codex/preference`
- `POST /api/managed-codex/refresh-cache`
- `POST /api/managed-codex/build-source`
- `GET /api/threads`

## 建議的 event model

event 仍以 runtime 事件優先，而不是同步 RPC 優先。

v1 至少保留：

- `thread_state_changed`
- `workspace_state_changed`
- `runtime_health_changed`
- `managed_codex_changed`
- `assistant_final`
- `error`

## Query / Control / Stream 分離

建議明確切開：

- `query`
  - 讀取 thread / workspace / runtime / managed Codex 狀態
- `control`
  - bind / reconnect / archive / restore / launch / setup / update
- `event stream`
  - UI 與 adapter 追蹤即時狀態變更

這樣 custom app、tray/web 管理面、observability UI 都比較清楚。

## 傳輸層選項

這篇先收斂 v1 實驗載體：

- local HTTP 提供 query / control
- SSE 提供 event stream

這一層現在已經是實際代碼路徑，不再只是預期方向。

後續若需要：

- 可再補 WebSocket
- 但不應推翻這份語意層

## 與現有資料模型的關係

現有資料不必推翻，但要重新掛接到 protocol：

- `session-binding.json`
  - 主要餵給 `ThreadStateView`
- `conversations.jsonl`
  - 可補充 thread / session 歷史
- `.threadbridge/state/shared-runtime/*`
  - 餵給 `run_status`、owner / runtime health
- workspace recent session history
  - 餵給 `RecentCodexSessionView`

## 與其他計劃的關係

- [macos-menubar-thread-manager.md](/Volumes/Data/Github/threadBridge/docs/plan/macos-menubar-thread-manager.md)
  - 使用這份協議作為本地管理面的正式 view / action 命名來源
- [session-level-cli-telegram-sync.md](/Volumes/Data/Github/threadBridge/docs/plan/session-level-cli-telegram-sync.md)
  - 提供 shared runtime、`hcodex`、TUI proxy、adoption 的現實模型
- [telegram-webapp-observability.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-webapp-observability.md)
  - 可共用部分 event 與 view，但不是這份 protocol 的唯一需求方

## 開放問題

- `Open in Telegram` 這類 UI 快捷操作，應放在 control action，還是由 adapter 自己實作？
- `managed_codex.version` 與 `revision` 要不要同時作為正式欄位？
- event stream 是否要保留更細的 preview / tool 流式事件，還是先只發聚合狀態變更？

## 建議的下一步

1. 先把上面的 view / action 名稱同步到本地 management API。
2. 讓 tray/web 管理面先只依賴這份 protocol，不直接讀 repository 檔案。
3. 之後再決定是否需要第二種 transport，例如 WebSocket。
