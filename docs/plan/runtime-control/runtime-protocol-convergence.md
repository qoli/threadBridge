# Runtime Protocol 收斂草稿

## 目前進度

這份文檔目前已進入部分落地；Phase 1（shared control action 最小切片）、Phase 2 的最小 interaction vocabulary、以及 Phase 3 的第一批 surface parity 已落地。

目前已實作：

- `runtime_protocol` 已有一套可用的 read-side view model：
  - `RuntimeHealthView`
  - `ManagedWorkspaceView`
  - `ThreadStateView`
  - `ArchivedThreadView`
  - `WorkingSessionSummaryView`
  - `WorkingSessionRecordView`
- management API 已對外提供這些 query surface 與 typed SSE event：
  - `GET /api/setup`
  - `GET /api/runtime-health`
  - `GET /api/threads`
  - `GET /api/threads/:thread_key/transcript`
  - `GET /api/threads/:thread_key/sessions`
  - `GET /api/threads/:thread_key/sessions/:session_id/records`
  - `GET /api/workspaces`
  - `GET /api/archived-threads`
  - `GET /api/events`
- management API 已有一批實際 control route：
  - workspace pick/add
  - unified runtime action route（`POST /api/threads/:thread_key/actions`）
  - open workspace
  - repair runtime
  - archive / restore
  - managed Codex preference / refresh / build
- Telegram adapter 已有一批實際 command surface：
  - `/start_fresh_session`
  - `/repair_session_binding`
  - `/launch_local_session ...`
  - `/get_workspace_execution_mode`
  - `/set_workspace_execution_mode ...`
  - `/sessions`
  - `/session_log`
  - `/stop`
  - `/plan_mode`
  - `/default_mode`
- shared protocol 已新增最小 control action 型別：
  - `RuntimeControlActionRequest`
  - `RuntimeControlActionResult`
  - `RuntimeControlActionEnvelope`
  - `LaunchLocalSessionTarget`
- shared protocol 的 action request 已開始覆蓋：
  - `set_thread_collaboration_mode`
  - `interrupt_running_turn`
- management / protocol public view 已開始同步暴露：
  - `ManagedWorkspaceView.current_collaboration_mode`
  - `ThreadStateView.current_collaboration_mode`
- observer / interaction 已有一條 shared event lane：
  - `RuntimeInteractionEvent::RequestUserInput`
  - `RuntimeInteractionEvent::RequestResolved`
  - `RuntimeInteractionEvent::TurnCompleted`
- `runtime_protocol.rs` 已新增 `RuntimeInteractionKind`，開始把 interaction vocabulary 明確掛回 shared protocol family

目前尚未完成：

- interaction event 雖已有 `RuntimeInteractionKind`，但仍是平行 typed family，尚未和 `RuntimeEventKind` 形成同一條 public stream contract
- adopt/reject/archive/restore、managed Codex setup、以及部分 owner control 仍未完全收進 unified action route
- control action result 目前仍主要靠 view diff / targeted refetch 體現，尚未成為獨立 protocol event family
- 部分 capability 仍只存在 local app API 或單一 adapter surface，尚未成為一致的 transport-facing public contract

## 問題

現在 `threadBridge` 的主要缺口不是缺 view，也不是缺路由，而是：

- view / query 已開始 protocol 化
- 但 control / interaction 仍有相當一部分停留在 surface-driven

具體表現是：

- management API route 名、Telegram slash command 名、shared service method 名，仍常常代表同一件事
- `runtime_protocol.rs` 已開始承載 control action vocabulary，但 interaction / stream vocabulary 仍未完全收斂
- `RuntimeInteractionEvent` 已是 shared event，但仍平行存在於 `runtime_protocol` 之外
- adopt/reject/archive/restore、managed Codex owner control、以及 interaction stream 仍未形成完整 public protocol surface

結果就是：

- 文檔要同時描述 route、command、handler 三套語言
- 新增一個 surface 時，容易再複製一套 naming
- 很難明確回答「這是一個 Telegram 功能，還是一個 runtime capability」

## 定位

這份文檔不是新的主規格。

它的角色是：

- `runtime-protocol.md` 的實施 / 收斂草稿
- 描述如何把既有代碼中的 route、slash command、shared service、interaction event 收斂到同一份 protocol 語義

它處理的是：

- rollout phase
- workstream 切分
- 哪些 capability 先收斂
- 哪些 Rust 模組要改
- 怎麼判定這個 protocol 收斂任務完成

它明確不處理：

- `binding_status` / `run_status` 等 canonical state semantics 本身
- Telegram delivery 主規格
- 第二個 adapter 的產品化
- capability bridge 的長期設計細節

## 核心原則

- 先收斂 naming，再收斂 transport。
- 先收斂 control / interaction，再補更多 query。
- 先讓既有 capability 對齊到單一 protocol action，不先擴更多功能面。
- protocol 的 source of truth 應先落在 shared Rust 型別，而不是只留在 plan 文檔。
- management API 與 Telegram 都應視為 adapter surface，而不是 capability owner。

## 主體規格

### 1. 先承認目前其實有三層語言

目前同一個 capability 常同時有三種表示：

- protocol / plan 裡的語義名稱
- HTTP route 名稱
- Telegram slash command 名稱

例如：

- execution mode
  - protocol: `set_workspace_execution_mode`
  - HTTP: `POST /api/threads/:thread_key/actions` + action payload
  - Telegram: `/set_workspace_execution_mode`
- local launch
  - protocol: `launch_local_session`
  - HTTP: `POST /api/threads/:thread_key/actions` + `target=new|continue_current|resume`
  - Telegram: `/launch_local_session new|continue_current|resume`

這代表目前真正缺的不是更多 route，而是少一層被代碼承認的 canonical action model。

### 2. 收斂工作分四條 workstream

#### Workstream A: Control Action Model

先在 shared Rust 層新增正式的 control action vocabulary。

v1 應至少覆蓋：

- `add_workspace`
- `pick_workspace_and_add_binding`
- `start_fresh_session`
- `repair_session_binding`
- `set_workspace_execution_mode`
- `set_thread_collaboration_mode`
- `launch_local_session`
- `interrupt_running_turn`
- `adopt_tui_session`
- `reject_tui_session`
- `archive_thread`
- `restore_thread`
- `repair_workspace_runtime`
- `reconcile_runtime_owner`
- `set_managed_codex_preference`
- `refresh_managed_codex_cache`
- `build_managed_codex_source`
- `set_managed_codex_build_defaults`

目前已落地的 shared 型別包括：

- `RuntimeControlAction`
- `RuntimeControlActionRequest`
- `RuntimeControlActionResult`
- `RuntimeControlActionEnvelope`
- `LaunchLocalSessionTarget`

這一層的目標不是立刻換 transport，而是讓 management API 與 Telegram 都呼叫同一個 canonical action vocabulary。

#### Workstream B: Interaction Protocol

把目前平行存在的 `RuntimeInteractionEvent` 收回 protocol 主線。

v1 至少應固定：

- `request_user_input_requested`
- `request_user_input_resolved`
- `plan_follow_up_requested`

這裡不一定要硬併進 SSE，但至少要在 protocol 文檔與 shared Rust 型別上，和 `RuntimeEventKind` 屬於同一個 vocabulary family。

近期最重要的是先把：

- `RequestUserInput`
- `RequestResolved`
- `TurnCompleted(has_plan=true)`

這三種事件重新表達成 protocol-facing 名稱，而不是繼續只留在 Telegram interaction bridge 內部語言。

#### Workstream C: Surface Parity

把已存在能力分成三類：

- 已經有 management API + Telegram + shared protocol
- 已經有 shared protocol + 單一 adapter
- 只有 adapter surface，尚未 protocol 化

近期應優先補齊這幾個 capability：

- `start_fresh_session`
  - 已有 management API + Telegram + shared protocol
- `set_thread_collaboration_mode`
  - 已有 management API + Telegram + shared protocol
  - `current_collaboration_mode` 也已進入 public view
- `interrupt_running_turn`
  - 已有 management API + Telegram + shared protocol
  - control action result 與後續 observability stream 仍未完全 formalize

目前狀態更新：

- `start_fresh_session`、`set_thread_collaboration_mode`、`interrupt_running_turn` 都已進入 shared action route
- `current_collaboration_mode` 已進入 `ManagedWorkspaceView` / `ThreadStateView`
- 下一步優先應轉為 adopt/reject/archive/restore 等剩餘 capability 的 action-route 收斂，以及 interaction stream 邊界

#### Workstream D: Public Vocabulary Cleanup

把 user-facing 與 docs-facing 名稱固定下來。

至少要收斂：

- `new_session` vs `start_fresh_session`
- `launch current` vs `continue_current`
- `plan_mode/default_mode` vs `set_thread_collaboration_mode`
- `stop` vs `interrupt_running_turn`

原則是：

- protocol 名可以和 slash command 不同
- 但 mapping 必須明確、穩定、可文檔化
- 不應讓不同 surface 各自延伸出新的 capability 名稱

### 3. 分階段落地

#### Phase 1: 補齊 shared control 型別

目標：

- 在 shared Rust 層新增 canonical control action model
- 不先變更外部行為

建議落點：

- 新增或擴充 `rust/src/runtime_protocol.rs`
- 視情況新增 `rust/src/runtime_actions.rs`
- management API 與 Telegram 先做最薄 mapping

完成標誌：

- 至少三個 action 已不再直接以 route/command 為主語義
  - `set_workspace_execution_mode`
  - `launch_local_session`
  - `repair_session_binding`

#### Phase 2: 補 interaction protocol

目標：

- 把 `RuntimeInteractionEvent` 明確掛回 protocol vocabulary
- 固定 interaction event naming

建議落點：

- `rust/src/runtime_interaction.rs`
- `rust/src/runtime_protocol.rs`
- `rust/src/app_server_observer.rs`
- `rust/src/telegram_runtime/interaction_bridge.rs`

完成標誌：

- interaction event 不再只是 Telegram bridge 專用語言
- 文檔可直接回答哪個 interaction event 屬於 public runtime contract

#### Phase 3: 補 surface parity

目標：

- 補齊目前仍缺 unified action route 或 public protocol naming 的 shared capability

優先順序：

1. `adopt_tui_session` / `reject_tui_session`
2. `archive_thread` / `restore_thread`
3. owner / managed Codex control 的 vocabulary 對齊

完成標誌：

- 這批 capability 都有：
  - canonical action 名
  - shared code path
  - 至少一個 transport-facing public surface
  - 清楚的 README / plan mapping

#### Phase 4: 補 event / observability coverage

目標：

- 決定 control action 結果是否也應進 SSE / observability payload
- 決定 interaction event 是否需要 public stream surface

這一階段不一定要做更細的增量 event，但要把責任邊界固定。

### 4. 建議的代碼切面

這個任務主要會碰到：

- [runtime_protocol.rs](../../../rust/src/runtime_protocol.rs)
  - canonical view / event / action vocabulary
- [management_api.rs](../../../rust/src/management_api.rs)
  - HTTP route -> canonical action mapping
- [runtime_control.rs](../../../rust/src/runtime_control.rs)
  - shared service / action execution
- [telegram_runtime/thread_flow.rs](../../../rust/src/telegram_runtime/thread_flow.rs)
  - slash command -> canonical action mapping
- [runtime_interaction.rs](../../../rust/src/runtime_interaction.rs)
  - interaction vocabulary
- [app_server_observer.rs](../../../rust/src/app_server_observer.rs)
  - observer -> interaction event mapping

### 5. 驗收標準

這個任務至少要達到下面幾件事，才算 protocol 收斂開始成立：

- 可以為每個重要 capability 先說出 canonical action 名，再說出各 adapter surface
- `runtime_protocol.rs` 不再只有 read model，也開始承載 control / interaction vocabulary
- `start_fresh_session`、`stop`、`plan_mode/default_mode` 不再只是 Telegram-first 功能
- management API、Telegram、plan 文檔不再各自重複發明同一能力的名字

## 與其他計劃的關係

- [runtime-protocol.md](runtime-protocol.md)
  - 這份文檔是它的 rollout / convergence 草稿，不取代它的主規格地位
- [telegram-adapter-migration.md](../telegram-adapter/telegram-adapter-migration.md)
  - Telegram 何時才算退回 protocol consumer，會直接依賴這份收斂計畫
- [session-lifecycle.md](session-lifecycle.md)
  - `start_fresh_session` / `repair_session_binding` / launch surface 的 protocol naming 需要和它對齊
- [codex-busy-input-gate.md](codex-busy-input-gate.md)
  - `/stop` 是否正式收斂成 `interrupt_running_turn`，會影響這份 plan
- [owner-runtime-contract.md](../desktop-runtime-owner/owner-runtime-contract.md)
  - owner / adapter / shared control 的邊界，會決定哪些 action 應屬於 protocol 主線

## 開放問題

- `RuntimeControlAction` 應直接放進 `runtime_protocol.rs`，還是拆成獨立模組？
- interaction event 應和 `RuntimeEventKind` 共用同一個 enum family，還是保持另一條 typed stream？
- unified action route 是否應繼續擴到 adopt/reject/archive/restore 與更多 owner control？
- interaction event 是否要併入現有 SSE，還是維持獨立 typed family 但共享 vocabulary？
- control action result 是否需要 public stream payload，還是目前仍以 view diff / refetch 邊界為主？

## 建議的下一步

1. 先在 `runtime-protocol` 主規格確認這份 rollout 草稿採用的 canonical action 名稱。
2. 把 shared control action 切片從 execution mode / launch / repair / collaboration / interrupt，再擴到 adopt/reject/archive/restore 與 owner control。
3. 再決定 interaction vocabulary 是要併入 `RuntimeEventKind`，還是保持獨立但同級的 protocol 型別。
4. 補一輪文檔同步，把 `telegram-adapter-migration`、`session-lifecycle`、`codex-busy-input-gate` 的 action naming 引到同一套語言。
5. 最後再決定 control action result / interaction event 是否需要 public stream，而不是先擴更多新功能。
