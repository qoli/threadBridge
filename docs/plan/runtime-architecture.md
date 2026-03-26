# Runtime Architecture 主文檔

## 目前進度

這份文檔目前已進入「部分落地」。

目前代碼中已成立的部分：

- `desktop runtime owner` 已是 machine-level runtime authority
- shared `runtime_control` 已承接 workspace runtime、session bind/new/repair、與 Telegram-to-live-TUI routing
- `app-server observer` 已承接 read-side projection 與 adapter-neutral interaction event
- `hcodex` 已是 owner-managed local entrypoint，而不是獨立 runtime owner
- Telegram adapter 與本地 management / desktop surface 已形成兩條不同的 adapter / surface 路徑

目前尚未完成的部分：

- 少量跨層捷徑仍存在，尚未完全退出 current code path
- `runtime_protocol` 雖已形成共享 view / event 語言，但仍未成為所有 control surface 的完整唯一 vocabulary
- 舊 CLI / hook 時代遺留的止血式修法心智，仍可能在 bugfix 時重新長回 codebase

## 問題

`threadBridge` 是從本地 CLI / hook / wrapper 模型，逐步演化到 `desktop owner + shared app-server ws + observer + adapter` 的模型。

真正的風險不是「角色太多」，而是：

- 新角色已經出現
- 舊模型的隱性責任沒有完整退場
- 中間又長出過渡性的 compatibility shortcut

如果沒有一份 current-state 主文檔，後續遇到回歸時，很容易再次用舊模型的方式止血：

- 把 adapter-specific 邏輯塞回 core
- 把 transport helper 當成 canonical control surface
- 把本地入口或 Telegram path 當成 runtime authority

## 定位

這份文檔是 `threadBridge` **當前架構**的角色與責任主文檔。

它處理：

- 哪些 actor 才是 canonical role
- 每個 role 目前負責什麼、不負責什麼
- role 之間允許如何協作
- 目前已知的跨層例外與退出方向
- 修 bug / 補 feature 時，應先如何判定 owner role

它不處理：

- `runtime_protocol` 的完整 view / action / event wire semantics
- `runtime-state-machine` 的完整 canonical state axes
- `session-lifecycle` 的 continuity / adoption 細節
- transport abstraction 的遠期 target architecture
- 歷史遷移過程的完整回顧

## Canonical 角色

### 1. `desktop runtime owner`

負責：

- machine-level runtime authority
- workspace runtime ensure / repair / reconcile
- owner-canonical runtime health
- workspace app-server 與 `hcodex` ingress 的 owner-side supervision

不負責：

- Telegram renderer / callback UI
- preview / final reply 呈現
- local `codex --remote` child lifecycle

常見誤用：

- 把 Telegram command path 或 `hcodex` local path 當成 owner
- 讓 workspace-local observation surface 取代 owner heartbeat 成為 authority

### 2. shared `runtime_control`

負責：

- write-side orchestration
- workspace runtime control-path preparation
- session bind / verify / new / repair
- Telegram 對 live TUI session 的 routing
- 供 Telegram 與 management surface 共用的 workspace/session control semantics

不負責：

- Telegram message send / edit / markup
- management UI 呈現細節
- read-side observability projection

常見誤用：

- 把 Telegram-specific UX、callback、或 renderer 邏輯塞回 control core

### 3. `app-server observer`

負責：

- thread-scoped event consumption
- preview / final / process projection
- session observability feed
- adapter-neutral interaction event emission

不負責：

- Telegram prompt / callback / follow-up UI
- Telegram final reply renderer
- runtime authority 或 canonical continuity mutation

常見誤用：

- 把 adapter glue 重新拉回 observer
- 把 observer 當成新的 control core

### 4. `hcodex` local ingress / launcher

負責：

- owner-managed local TUI entrypoint
- `launch_ws_url + launch_ticket` compatibility boundary
- local websocket bridge
- local session claim / launcher lifecycle 記錄
- 自己啟動的本地 `codex --remote` child lifecycle supervision
- live request-response injection

不負責：

- machine-level runtime authority
- canonical continuity ownership
- shared read-side projection authority

常見誤用：

- 把 `hcodex` 路徑重新當成 self-healing runtime owner
- 把 mirror / projection / adoption 完全綁死在 ingress 內部

### 5. Telegram adapter

負責：

- Telegram command / message / media input
- Telegram preview / final reply / delivery surface
- Telegram interaction UI
- 把 Telegram 輸入與 UI 映射到 shared runtime semantics

不負責：

- runtime authority
- 重新定義 session / workspace / state semantics
- 擁有自己的 canonical control vocabulary

常見誤用：

- 直接依賴 observer / ingress 內部細節
- 為了止血把共享模型退化成 Telegram-only control path

### 6. management / desktop surface

負責：

- local HTTP management API
- tray / web management UI
- desktop launch control surface
- owner-facing runtime / workspace / session query and control

不負責：

- 擁有 runtime semantics 本身
- 重新定義 canonical role boundary
- 取代 shared runtime semantics 成為另一套內部模型

常見誤用：

- 把 transport-facing route / helper 當成 canonical shared model

## 不是角色的東西

下面這些會被頻繁引用，但不應被畫成與上面平級的 actor：

- `runtime_protocol`
  - 共享語言，不是 actor
- `repository` / `workspace_status`
  - state surface / persistence lane，不是 actor
- workspace app-server、`.threadbridge/`、tool executors
  - managed backend / runtime surface，不是 threadBridge 內部角色

## Allowed Collaboration

- `desktop runtime owner` 擁有 machine-level runtime health authority；其他 surface 可以觀測它，不應替代它。
- `runtime_control` 是 shared write-side 邊界；adapter / surface 應優先透過它表達 control semantics，而不是各自直連底層細節。
- `app-server observer` 是 shared read-side projection 邊界；它可以發出 adapter-neutral interaction event，但不應承擔平台專屬呈現。
- `hcodex` 只擁有 local entrypoint / child lifecycle authority；任何 canonical continuity mutation 仍應回到 shared runtime semantics。
- Telegram adapter 與 management / desktop surface 都是 consumer / presenter；它們可消費共享 view / event，也可觸發 control action，但不是 capability owner。

## 禁止事項

- 不把 Telegram renderer、callback handling、delivery policy 拉回 observer 或 control core。
- 不把 management API transport helper 或 surface view 型別當成 Telegram adapter 的內部共享模型。
- 不把 `hcodex` ingress、launch bridge、或 local reconnect path 當成 runtime authority。
- 不以「先修再說」的理由，把 bug 修在跨層捷徑上，而不先判定 owner role。
- 不在新 plan 或新實作中重複發明另一套角色名稱，與本文件平行競爭。

## 暫時例外

下面這些是目前已知存在、但不應被複製的新常態：

### 1. observer 仍直接依賴 Telegram final reply 組裝

- 現況：
  - `rust/src/app_server_observer.rs` 直接使用 `telegram_runtime::final_reply::compose_visible_final_reply`
- 問題：
  - observer 仍碰到 adapter-specific final reply 組裝語言
- 退出方向：
  - 將 final text composition 收斂到 shared projection helper，或讓 adapter 在消費 observer 結果時自行組裝

### 2. Telegram thread flow 仍直接依賴 management API 的 launch/view 型別與 helper

- 現況：
  - `rust/src/telegram_runtime/thread_flow.rs` 直接使用 `management_api::{HcodexLaunchConfigView, WorkspaceExecutionModeView, hcodex_launch_command, launch_hcodex_via_terminal}`
- 問題：
  - Telegram adapter 直接吃 management surface 的 transport-facing model 與 desktop helper
- 退出方向：
  - 將共享 launch / mode model 抽回 shared runtime semantics，讓 Telegram 與 management surface 各自做最薄 mapping

### 3. local control 仍直接依賴 Telegram runtime helper

- 現況：
  - `rust/src/local_control.rs` 直接使用 `telegram_runtime::{AppState, send_scoped_message, status_sync, thread_id_to_i32}`
- 問題：
  - local management path 仍透過 Telegram adapter helper 完成部分 side effect
- 退出方向：
  - 將 Telegram side effect 收斂為明確 bridge / adapter interface，避免 local control 直接拿 Telegram runtime 當內部工具箱

## 與其他計劃的關係

- `runtime-state-machine`
  - 定義 canonical state axes，不在本文件重複定義
- `runtime-protocol`
  - 定義 view / action / event naming，不在本文件重複定義
- `session-lifecycle`
  - 定義 continuity、adoption、bind/new/repair 的語義
- `owner-runtime-contract`
  - 作為 owner/runtime boundary 的高層背景與收斂歷程，不再作為角色邊界的唯一主文檔
- `runtime-transport-abstraction`
  - 記錄遠期 core / adapter 抽象化方向，不取代 current architecture
- `app-server-ws-mirror-observer`
  - 處理 observer / mirror intake 的子問題與歷史背景

## 修 bug 的決策規則

當後續出現回歸、兼容問題、或功能缺口時，先做下面三步：

1. 先判定 bug 屬於哪個 canonical role 的責任。
2. 若修法跨層，先判定它是：
   - 應回到 owning role 修正
   - 還是 temporary exception 的必要擴張
3. 若不得不依賴 temporary exception，必須同時更新本文件，不允許只把理由藏在 commit 訊息裡。

預設規則：

- 不複製 temporary exception
- 不把 compatibility shim 誤升格成 canonical architecture
- 不以歷史上 CLI 模型曾經有效，作為現在 cross-layer fix 的正當性

## 開放問題

- Telegram 何時才算完整退回 protocol consumer，而不再依賴 adapter-local control plumbing
- `hcodex` launch / bridge / supervise 中，哪些仍是長期 core contract，哪些只是過渡結構
- 哪些 management / Telegram 共享 model 應抽成 shared runtime semantics，而不是留在單一 surface 模組

## 建議的下一步

- 讓 `docs/plan/README.md` 與 `authoring-guide.md` 將這份文檔提升為角色邊界主文檔
- 讓後續 role-related plan 明確引用這份文檔，而不是重複定義角色
- 依本文件列出的 temporary exception，拆出後續代碼收斂工作
