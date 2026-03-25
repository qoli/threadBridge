# Runtime 協議草稿

## 目前進度

這份文檔已開始部分落地，但仍不是完整的 transport-neutral 主規格；其中一部分 view / action / event 已經掛進本地 management API 和 desktop runtime。

目前代碼狀態：

- Telegram bot 仍是主要聊天入口
- 本地 management API 已開始提供 query / control / SSE：
  - setup
  - runtime health
  - active thread list
  - active thread transcript
  - active thread sessions summary / records
  - managed workspace list
  - archived thread list
  - pick-and-add / repair session binding / open / archive / restore
  - runtime-owner reconcile
  - managed Codex preference / cache refresh / source build / build-defaults
  - workspace launch config
  - adopt / reject pending TUI handoff
  - `launch-hcodex-new` / `launch-hcodex-continue-current` / `launch-hcodex-resume`
- `threadbridge_desktop` 已開始直接依賴這些本地 view / action
- transport-neutral 的正式 view / action 命名仍未完全收斂
- local HTTP + SSE 已成為目前最務實的實驗載體
- 已新增共享的 `runtime_protocol` view builder，開始把 `ThreadStateView` / `ManagedWorkspaceView` / `ArchivedThreadView` / `RuntimeHealthView` / `WorkingSession*View` 從 `management_api` 的 transport 邏輯裡拆出來
- repository write-side 的 canonical mutation 已開始透過共用 transition service 收斂
- runtime health 已開始改成 owner-canonical；`workspace_state` 不再是 primary readiness source
- process transcript 已開始透過 `GET /api/threads/:thread_key/transcript` 對外暴露，且本地 web / Telegram preview 已開始共用同一份摘要來源
- session-first observability 已開始透過 `GET /api/threads/:thread_key/sessions` 與 `GET /api/threads/:thread_key/sessions/:session_id/records` 對外暴露
- `ThreadStateView` 已開始對外暴露 canonical `lifecycle_status`
- `binding_status` / `run_status` 已開始透過 shared resolver 收斂成同一套 wire semantics
- `conflict` 已明確保留為 workspace-view 的獨立欄位，而不是 `binding_status` 的另一個值
- runtime health 的 broken thread count、workspace recovery hint、以及 working session broken error 聚合，已開始從 canonical `binding_status` 派生
- public view 已開始移除 `session_broken` / `last_error` 這類 compatibility alias；workspace/thread 的 canonical 判斷應直接讀 `binding_status` / `run_status` / `session_broken_reason`
- `GET /api/events` 已開始輸出 typed SSE event，而不是每輪都推整包 snapshot
- 目前已落地的 event kind 包括：
  - `setup_changed`
  - `runtime_health_changed`
  - `thread_state_changed`
  - `workspace_state_changed`
  - `archived_thread_changed`
  - `working_session_changed`
  - `transcript_changed`
  - `error`
- web UI 已開始直接套用 `setup` / `runtime_health` / `workspace` / `archived_thread` 的 typed SSE payload，而不是每次事件都重抓整包 snapshot
- transcript / working sessions observability 目前仍維持 query-based；`working_session_changed` / `transcript_changed` 的責任是提供明確 refetch 邊界，而不是 records 級 incremental payload

目前新增記錄的近期方向是：

- Telegram 之後應能透過正式 control action 設定 Codex 工作模型
- Telegram 之後也應能透過正式 control action 設定 execution mode
- 這兩者都不應以 Telegram-only command flag 的形式存在，而應先收斂成 runtime protocol 的控制面

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

這裡要刻意區分兩層：

- `runtime protocol`
  - 對外的 view / action / event naming
- repository transition service
  - 內部 canonical mutation authority

後者不應被文檔寫成新的 transport-facing public API。

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

目前 management API 的 `ThreadStateView` 仍只列 active threads；archived threads 先繼續由 `ArchivedThreadView` 承接。

至少包含：

- `thread_key`
- `title`
- `chat_id`
- `message_thread_id`
- `workspace_cwd`
- `lifecycle_status`
- `binding_status`
- `run_status`
- `current_codex_thread_id`
- `tui_active_codex_thread_id`
- `tui_session_adoption_pending`
- `session_broken_reason`
- `last_verified_at`
- `last_codex_turn_at`
- `archived_at`

目前保留的非 canonical 輔助欄位：

- `last_used_at`

這裡再補一個行為要求：

- `ThreadStateView` 的 consumer 不應繞過 `binding_status` / `run_status` 的 canonical 判定
- `last_used_at`
  - 只保留作 compatibility alias
  - 語義上等同 `last_codex_turn_at`

### 2. `ArchivedThreadView`

至少包含：

- `thread_key`
- `title`
- `workspace_cwd`
- `archived_at`
- `previous_message_thread_ids`

### 3. `ManagedWorkspaceView`

這是 workspace-first 管理頁的主要 view。

這裡要明確固定一個語義：

- `binding_status`
  - 只使用 canonical 值：`unbound` / `healthy` / `broken`
- `conflict`
  - 是 workspace-level 衍生欄位，不是 `binding_status` 的另一個枚舉值
- `run_status`
  - 代表 active Codex turn 是否 busy，不等於 local session claim 是否存在
- broken count、recovery hint、以及其他 workspace-level 衍生判斷，也應以 `binding_status` 為準
- repository 內部的 transition service 不改變這個對外語義；它只是把 write-side state mutation 收斂到同一條內部路徑
- public control naming 應維持 runtime-facing 名稱；不要把 repository 內部的 `BindWorkspace` / `VerifySession` / `MarkBroken` 直接當成對外 action 名稱

至少包含：

- `workspace_cwd`
- `title`
- `thread_key`
- `workspace_execution_mode`
- `current_execution_mode`
- `current_approval_policy`
- `current_sandbox_policy`
- `mode_drift`
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
- `runtime_readiness`
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

### 6. `WorkingSessionSummaryView`

這是 session-first observability v1 的 summary view。

目前已在 management API 對外暴露，至少包含：

- `session_id`
- `thread_key`
- `workspace_cwd`
- `started_at`
- `updated_at`
- `run_status`
- `origins_seen`
- `record_count`
- `tool_use_count`
- `has_final_reply`
- `last_error`

### 7. `WorkingSessionRecordView`

這是單一 session timeline 的 record view。

目前已在 management API 對外暴露，至少包含：

- `timestamp`
- `session_id`
- `kind`
- `origin`
- `role`
- `summary`
- `text`
- `delivery`
- `phase`
- `source_ref`

### 8. `RuntimeHealthView`

至少包含：

- `managed_codex`
- `app_server_status`
- `tui_proxy_status`
- `runtime_readiness`
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

若某個 action 同時有：

- 對外語義名稱
- 目前代碼中的歷史 endpoint / handler 名稱

應優先以語義名稱寫在主規格裡，再在需要時註明目前 wire / endpoint 仍沿用舊名。

v1 至少定義：

- `create_thread`
- `add_workspace`
- `pick_and_add_workspace`
- `repair_session_binding`
- `reconcile_runtime_owner`
- `open_workspace`
- `adopt_tui_session`
- `reject_tui_session`
- `launch_hcodex_new`
- `launch_hcodex_continue_current`
- `launch_hcodex_resume`
- `update_workspace_execution_mode`
- `update_workspace_codex_model`
- `archive_thread`
- `restore_thread`
- `update_managed_codex`
- `refresh_managed_codex_cache`
- `build_managed_codex_source`
- `update_managed_codex_build_defaults`
- `save_telegram_setup`

目前代碼中的 local HTTP / handler 名稱已開始直接切到正式語義，例如：

- `repair_session_binding`
- `archive_thread`

這裡近期要明確固定一個原則：

- Telegram 若未來提供「切模型」或「切模式」能力，應只是這些 control action 的一個 adapter surface
- 不應再新增只服務 Telegram 的私有 runtime 設定語意
- `restore_thread`

但主規格應優先表達它們的語義，而不是把歷史命名直接當成最終 protocol vocabulary。

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
- `POST /api/threads/:thread_key/repair-session-binding`
- `GET /api/workspaces/:thread_key/launch-config`
- `POST /api/workspaces/:thread_key/open`
- `POST /api/workspaces/:thread_key/launch-hcodex-new`
- `POST /api/workspaces/:thread_key/launch-hcodex-continue-current`
- `POST /api/workspaces/:thread_key/launch-hcodex-resume`
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
- `setup_changed`
- `archived_thread_changed`
- `error`

目前已落地的 wire shape 是：

- `kind`
- `op`
- `key`
- `current`
- `message`

其中：

- `op`
  - `upsert`
  - `remove`
- `current`
  - 在 `upsert` 時承載目前的 view payload
- `message`
  - 目前主要用在 `error`

目前已收斂的 v1 wire semantics：

- `setup_changed` / `runtime_health_changed`
  - singleton upsert
  - 不帶 `key`
  - `current` 是完整 replacement payload
- `thread_state_changed` / `workspace_state_changed` / `archived_thread_changed`
  - keyed event
  - `op=upsert` 時 `current` 是完整 replacement payload
  - `op=remove` 時只保留 `key`，不帶 `current`

如果引入 desktop capability bridge，之後也需要考慮：

- `capability_requested`
- `capability_approval_required`
- `capability_completed`
- `capability_denied`

目前新增確認的缺口是：

- mirror / observability 已開始承接更完整的 Codex 過程文本，event model 應收斂成等價的 process transcript 事件，而不是各 adapter 自己拼 `plan_text` / `tool_text`
- `codex plan` 消息流已接入 mirror；目前重點轉為 combined final reply 與 plan snapshot 在 transcript / observability 上的呈現收斂，詳見 [codex-plan-mirror.md](/Volumes/Data/Github/threadBridge/docs/plan/codex-plan-mirror.md)
- `managed_codex_changed` 這類更細的 owner / build event 尚未獨立落地
- event stream 雖然已 typed 化，但目前仍只直接驅動 top-level views；更細的 observability record 仍未走完整增量 payload

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
- [working-session-observability.md](/Volumes/Data/Github/threadBridge/docs/plan/working-session-observability.md)
  - session summary / session record / artifact refs 的 observability view 應由這份文檔定義
- [telegram-webapp-observability.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-webapp-observability.md)
  - 可共用部分 event 與 view，但不是這份 protocol 的唯一需求方

## 開放問題

- `Open in Telegram` 這類 UI 快捷操作，應放在 control action，還是由 adapter 自己實作？
- `managed_codex.version` 與 `revision` 要不要同時作為正式欄位？
- event stream 是否要保留更細的 preview / tool 流式事件，還是先只發聚合狀態變更？

## 建議的下一步

1. 先把上面的 view / action 名稱同步到本地 management API。
2. 讓 tray/web 管理面逐步直接消費 typed event payload，而不是只把它們當 refresh signal。
3. 之後再補 `managed_codex_changed`、更細的 process transcript event，並決定是否需要第二種 transport，例如 WebSocket。
