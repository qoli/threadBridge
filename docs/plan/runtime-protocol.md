# Runtime 協議草稿

## 目前進度

這份文檔目前仍是草稿，但已不只是命名建議；其中一部分 view / action 已經掛進本地 management API 和 desktop runtime。

目前代碼狀態：

- Telegram bot 仍是主要聊天入口
- 本地 management API 已開始提供 query / control / SSE：
  - setup
  - runtime health
  - active thread list
  - active thread transcript
  - managed workspace list
  - archived thread list
  - pick-and-add / reconnect / open / archive / restore
  - runtime-owner reconcile
  - managed Codex preference / cache refresh / source build / build-defaults
  - workspace launch config
  - adopt / reject pending TUI handoff
  - `launch_hcodex_new` / `launch_hcodex_continue_current` / `launch_hcodex_resume`
- `threadbridge_desktop` 已開始直接依賴這些本地 view / action
- transport-neutral 的正式 view / action 命名仍未完全收斂
- local HTTP + SSE 已成為目前最務實的實驗載體
- runtime health 已開始改成 owner-canonical；`workspace_state` 不再是 primary readiness source
- process transcript 已開始透過 `GET /api/threads/:thread_key/transcript` 對外暴露，且本地 web / Telegram preview 已開始共用同一份摘要來源

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

這個 view 保留給 runtime / debug / advanced maintenance，不作為普通用戶主列表。

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

這是 workspace-first 管理頁的主要 view。

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
- `app_server_status`
- `tui_proxy_status`
- `handoff_readiness`
- `runtime_health_source`
  - `owner_heartbeat`
  - `owner_pending`
  - `owner_required`
- `heartbeat_last_checked_at`
- `heartbeat_last_error`
- `session_broken_reason`

這裡目前要明確承認一件事：

- `owner_heartbeat` 與 workspace observation 不是同一層訊號

比較合理的語義應是：

- `owner_heartbeat`
  - machine / owner 對 workspace runtime health 的正式判斷
- `owner_pending` / `owner_required`
  - owner 尚未提供 heartbeat，或 desktop owner 根本不存在

workspace 內現有 artifact / endpoint state 可以作為 debug observation，但不再作 primary health fallback。

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
- `build_config_file_path`
- `build_defaults`
- `build_info_file_path`
- `build_info`

### 6. `RuntimeHealthView`

至少包含：

- `managed_codex`
- `app_server_status`
- `tui_proxy_status`
- `handoff_readiness`
  - `ready`
  - `pending_adoption`
  - `degraded`
  - `unavailable`
- `runtime_owner`
  - `state`
  - `last_reconcile_started_at`
  - `last_reconcile_finished_at`
  - `last_successful_reconcile_at`
  - `last_error`
  - `last_report`
- `broken_threads`
- `running_workspaces`
- `conflicted_workspaces`
- `ready_workspaces`
- `degraded_workspaces`
- `unavailable_workspaces`

這個 view 的 canonical authority 應固定為 owner；若沒有 owner heartbeat，回傳的應是 owner 缺席/待就緒語義，而不是 fallback runtime health。

### 7. `SetupStateView`

至少包含：

- `telegram_token_configured`
- `authorized_user_ids`
- `authorized_user_count`
- `telegram_polling_state`
- `management_base_url`
- `restart_required_after_setup_save`
- `control_chat_ready`
- `control_chat_id`
- `native_workspace_picker_available`

## 建議的 control action

這一層應避免 Telegram 或 UI 專屬命名。

v1 至少定義：

- `create_thread`
- `bind_workspace`
- `pick_and_add_workspace`
- `reconnect_codex`
- `reconcile_runtime_owner`
- `open_workspace`
- `adopt_tui_session`
- `reject_tui_session`
- `launch_hcodex_new`
- `launch_hcodex_continue_current`
- `launch_hcodex_resume`
- `archive_thread`
- `restore_thread`
- `update_managed_codex`
- `refresh_managed_codex_cache`
- `build_managed_codex_source`
- `update_managed_codex_build_defaults`
- `save_telegram_setup`

如果之後引入 desktop runtime capability bridge，這一層還需要回答：

- capability request
- capability approval
- capability deny
- capability result

尤其是跨沙盒 capability，v1 應預設需要 desktop runtime 的 machine-local 授權確認，而不是把它當成普通 control action 一樣自動執行。

目前已部分對應到代碼中的 local HTTP endpoint：

- `POST /api/workspaces/pick-and-add`
- `POST /api/runtime-owner/reconcile`
- `POST /api/threads/:thread_key/adopt-tui`
- `POST /api/threads/:thread_key/reject-tui`
- `GET /api/workspaces/:thread_key/launch-config`
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
- `POST /api/managed-codex/build-defaults`
- `GET /api/threads`
- `GET /api/workspaces`
- `GET /api/archived-threads`
- `GET /api/runtime-health`
- `GET /api/setup`
- `GET /api/events`

## 建議的 event model

event 仍以 runtime 事件優先，而不是同步 RPC 優先。

v1 至少保留：

- `thread_state_changed`
- `workspace_state_changed`
- `runtime_health_changed`
- `managed_codex_changed`
- `assistant_final`
- `error`

如果引入 desktop capability bridge，之後也需要考慮：

- `capability_requested`
- `capability_approval_required`
- `capability_completed`
- `capability_denied`

目前新增確認的缺口是：

- mirror / observability 已開始承接更完整的 Codex 過程文本，event model 應收斂成等價的 process transcript 事件，而不是各 adapter 自己拼 `plan_text` / `tool_text`

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
- [session-level-mirror-and-readiness.md](/Volumes/Data/Github/threadBridge/docs/plan/session-level-mirror-and-readiness.md)
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
