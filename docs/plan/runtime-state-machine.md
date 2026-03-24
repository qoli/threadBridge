# Runtime State Machine 草稿

## 目前進度

這份文檔已開始部分落地，且已進一步成為狀態語義與 canonical transition 的主規格，但仍不是完整唯一 source of truth。

目前部分已在代碼中可見的狀態語義：

- `active` / `archived`
- `healthy` / `broken` / `unbound` 的 session 綁定語義
- shared thread-state resolver 已開始成為 `lifecycle_status` / `binding_status` / `run_status` 的共同判定來源
- ordinary Telegram command / text / image gate 已開始改走 shared resolver，而不是各自直接讀 `Archived` / `session_broken`
- management API 的 `ThreadStateView` 已開始直接暴露 canonical `lifecycle_status` / `binding_status` / `run_status`
- management API 的 `ThreadStateView` / `ManagedWorkspaceView` / `ArchivedThreadView` / `RuntimeHealthView` 已開始透過共享的 protocol/view builder 收斂，而不是各自在 handler 內重組狀態
- `/api/events` 已開始從 canonical view diff 輸出 typed SSE event，而不是每輪都推整包 snapshot
- web 管理面已開始直接套用 top-level typed SSE payload，而不是每次事件都重抓整包 snapshot
- topic title 的 `broken` suffix 已開始從 canonical binding state 派生；`busy` 已退出 title 語義
- `binding_status=conflict`、`run_status=unbound` 這類過渡值已從 canonical state axes 中移除
- `session_broken` 目前仍保留在部分 view 作為 compatibility/debug 欄位，但不再應被視為和 `binding_status` 平行的另一條 canonical state axis
- repository 內部的 canonical write path 已開始透過共用 transition service 收斂
  - `bind_workspace`
  - `mark_session_binding_verified`
  - `select_session_binding_session`
  - `mark_session_binding_broken`
  - `archive_thread`
  - `restore_thread`

目前尚未完成的部分：

- 讓更多 surface 在呈現 thread / workspace state 時完全只引用同一套 canonical axes
- 把 canonical view 的欄位命名完全收斂到現行模型，例如 `current_codex_thread_id`
- 把更多非 repository surface 的控制語義也完全收斂到同一套 transition vocabulary，而不是只在 repository 內部收口
- 讓 `/api/events` 的 typed payload coverage 與 observability 層再進一步收斂，而不只停在目前的 change-event + refresh trigger 模型

## 問題

`threadBridge` 現在已經有多份跟 thread 狀態有關的草稿：

- `session-lifecycle`
- `codex-busy-input-gate`
- `topic-title-status`
- `runtime-protocol`

但這幾份文件目前還沒有一套唯一、穩定、可被實作直接引用的狀態語言。

如果不先把 state machine 收斂成主規格，後續會持續出現：

- 不同文件各自定義自己的 `idle / running / broken`
- Telegram UX、Web App、restore UI、busy gate 各自讀不同訊號
- adapter 層與 repository 層對「thread 現在是什麼狀態」理解不一致

## 定位

這份文件是 `threadBridge` 狀態語義的唯一主規格。

後續這些文件都應該引用這份文件，而不是再各自定義另一套狀態名稱：

- `session-lifecycle.md`
- `codex-busy-input-gate.md`
- `topic-title-status.md`
- `runtime-protocol.md`

## 核心原則

- `threadBridge` 的 thread 狀態不是單一 enum，而是多條正交狀態軸的組合。
- persistent state 與 ephemeral runtime state 必須分開描述。
- archive / restore 是 Telegram thread lifecycle，不等於 Codex continuity lifecycle。
- image pending batch、preview 草稿、message delivery 狀態不是 thread state 本體。

## Canonical 狀態軸

### 1. `lifecycle_status`

代表這個 thread 在 Telegram / bot-local lifecycle 上是否可互動。

合法值：

- `active`
- `archived`

source of truth：

- `metadata.json.status`

語義：

- `active`
  - 目前可接受一般 thread 互動
- `archived`
  - 已歸檔，不應接受一般對話或圖片分析流程

### 2. `binding_status`

代表這個 thread 跟 workspace / Codex thread 的綁定是否可用。

合法值：

- `unbound`
- `healthy`
- `broken`

source of truth：

- canonical 判定以 usable workspace binding + `session_broken` flag 為準
- `session-binding.json`
- `session_broken`
- `session_broken_reason`

判定規則：

- `unbound`
  - `session-binding.json` 不存在，或缺少可用的 `workspace_cwd`
- `healthy`
  - binding 存在，且 `session_broken = false`
- `broken`
  - binding 存在，且 `session_broken = true`

相容性備註：

- `session_broken`
  - 目前仍可能出現在部分 workspace/thread view 中
  - 但它應被理解成 `binding_status=broken` 的 compatibility/debug 映射，而不是新的對外主狀態欄位
- `mark_session_binding_broken`
  - 現行實作應理解成「既有 workspace continuity 斷裂」
  - 若 thread 連 usable workspace binding 都沒有，應維持 `unbound`，而不是創造一個沒有 binding 的 `broken`

### 3. `run_status`

代表這個 thread 是否有一個 active Codex turn 正在執行。

合法值：

- `idle`
- `running`

source of truth：

- 不是 repository 的 long-term persistent artifact
- 目前實作主要從目前採用的 Codex session snapshot 推導，必要時才 fallback 到 `tui_active_codex_thread_id`
- 後續仍應收斂成更清楚的 canonical runtime view，而不是讓各 surface 各自讀檔

初版規則：

- v1 不定義 `queued`
- v1 不把 `run_status` 持久化到 `data/`

## Canonical ThreadStateView

之後凡是要對外呈現 thread 狀態的 surface，都應該能對應到下面這個 view：

- `thread_key`
- `chat_id`
- `message_thread_id`
- `lifecycle_status`
- `binding_status`
- `run_status`
- `workspace_cwd`
- `current_codex_thread_id`
- `tui_active_codex_thread_id`
- `session_broken_reason`
- `last_verified_at`
- `last_codex_turn_at`
- `archived_at`

這不是要求目前從零新增 API。

目前代碼中已存在名稱相近的 `ThreadStateView` / `ManagedWorkspaceView`，但欄位與來源仍是過渡狀態；後續應往這份文檔收斂，而不是把現在的回傳 shape 直接當成最終主規格。

`runtime-protocol` 若暫時保留 `title`、`last_used_at`、`last_error` 這類 compatibility / debug 欄位，可以接受；但它們不應取代這裡列出的 canonical state / identity / timeline 欄位。

這裡要明確固定一個命名原則：

- 若描述 Telegram thread 目前正式採用的 Codex continuity，應直接寫 `current_codex_thread_id`
- 不應在主規格裡再退回泛稱 `codex_thread_id`

## 狀態轉移

### 新 thread

`/add_workspace` 的 thread 建立部分完成後：

- `lifecycle_status = active`
- `binding_status = unbound`
- `run_status = idle`

在目前正式產品流裡，普通使用者主要不是先手動 `/new_thread`，而是走 `/add_workspace` 或本地 management API 的 create-bind 流程。

### 綁定 workspace

`/add_workspace <absolute-path>` 或等價 create-bind control 成功後：

- `lifecycle_status = active`
- `binding_status = healthy`
- `run_status = idle`
- `workspace_cwd` 與 `current_codex_thread_id` 成為可用值

### 一般 turn 開始

在 `active + healthy + idle` 的 thread 上開始一個正常 Codex turn：

- `run_status` 進入 `running`

### 一般 turn 成功

Codex turn 成功完成後：

- `run_status = idle`
- `binding_status` 維持 `healthy`
- 更新 `last_codex_turn_at`
- 更新 `last_verified_at`

### 一般 turn 失敗

如果 Codex resume 失敗、thread continuity 無效、或 `cwd` 驗證失敗：

- `run_status = idle`
- `binding_status = broken`
- 保留 `workspace_cwd`
- 記錄 `session_broken_reason`

### `/repair_session` / reconnect control

成功：

- `binding_status = healthy`
- `run_status = idle`
- 更新 `last_verified_at`

失敗：

- `binding_status = broken`
- `run_status = idle`

### `/new_session`

這個操作是 reset 目前 Codex continuity，但保留同一個 workspace binding。

成功後：

- `binding_status = healthy`
- `run_status = idle`
- `workspace_cwd` 保持不變
- `current_codex_thread_id` 換成新的值

### archive

archive 是 Telegram / bot-local lifecycle 變化，不是 Codex thread 變化。

archive 後：

- `lifecycle_status = archived`
- `binding_status` 原樣保留
- 在 idle 情況下，`run_status` 維持 `idle`

目前已收斂的一個現況是：

- archive / restore 的 canonical metadata mutation 已開始透過共用 transition path 寫回
- 若之後要嚴格禁止 archived thread 保持 `running`，仍需要更集中化的 transition authority 或明確 gate

### restore

restore 只恢復 Telegram topic，不自動恢復 Codex continuity。

restore 後：

- `lifecycle_status = active`
- `binding_status` 保持 restore 前的值
- `session_broken` 相關欄位保持原樣
- `message_thread_id` 換成新的 topic id
- `previous_message_thread_ids` 累積舊 topic id

## Command Gate 語義

### 一般文字與圖片輸入

- `archived`
  - 拒絕一般 thread 對話
- `unbound`
  - 拒絕啟動 Codex turn，提示先走 `/add_workspace` 或等價 create-bind flow
- `broken`
  - 拒絕一般 turn，提示 `/repair_session` 或 `/new_session`
- `running`
  - 初版規劃由 busy gate 文檔定義為硬阻止，不在這份文件再延伸成 queue

### `/add_workspace` / create-bind control

- 只允許在 `active` thread 上執行
- 可以從 `unbound` 進入 `healthy`
- 若已綁定，屬於顯式替換 binding 的控制操作，不是一般訊息

### `/new_session`

- 只處理 Codex continuity reset
- 不改變 `lifecycle_status`
- 不把 `archived` thread 隱式帶回 `active`

### `/repair_session` / reconnect control

- 只驗證與修復 binding 狀態
- 不是 restore
- 不是重建 workspace

## 不屬於這份狀態機的東西

下面這些不應混入 thread state enum：

- pending image batch 是否存在
- preview draft 當前文字
- Telegram message queue / delivery lane
- final reply 是 HTML、plain text、還是 attachment

這些屬於 artifact state 或 delivery state，不是 thread lifecycle state。

## 與其他計劃的關係

- `session-lifecycle`
  - 改由這份文件提供狀態名稱與 reset / reconnect 語義
- `codex-busy-input-gate`
  - 改由這份文件提供 `run_status` 的唯一語義
- `topic-title-status`
  - 只能從這份文件定義的狀態軸取值
- `runtime-protocol`
  - 對外 view / action naming 應對齊這份文件；內部 write-side transition 也不應自創另一套 state 語義

## 暫定結論

`threadBridge` 的 thread 狀態應固定理解為：

- 一條 lifecycle 軸
- 一條 binding 軸
- 一條 run 軸

而不是把所有狀態壓成單一 enum。
