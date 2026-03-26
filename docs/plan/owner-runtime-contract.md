# Owner Runtime Contract 草稿

## 目前進度

這份文檔目前已進入「部分落地」。

目前代碼中已成立的部分：

- `desktop runtime owner` 已是正式 runtime authority
- workspace app-server 已是 canonical runtime backend
- `hcodex` 已是 owner-managed local entrypoint，而不是自補 runtime 的獨立 owner
- local/TUI mirror intake 已從歷史上的 proxy 路徑拆到獨立 app-server observer
- shared `runtime_control` 已承接 workspace runtime ensure、session bind/new/repair、以及 Telegram-to-live-TUI routing
- management API 已開始透過 shared `runtime_protocol` view / action / event 命名對外暴露 runtime semantics
- adoption 已不是單純 UI 概念，而是持久化 state、`runtime_readiness` 衍生值、以及 control action 的一部分

目前尚未完成的部分：

- observer attach 仍建立在 `thread/resume` attach 語義上，而不是正式的 upstream subscribe API
- Telegram 雖已透過 shared control semantics 與 interaction bridge 工作，但仍未完全退回純 protocol consumer
- `hcodex` ingress、launch contract、與 compatibility shim 的長期保留邊界仍未完全寫死
- adoption 的最終命名與對外呈現仍未拍板
- `runtime protocol` 仍未完全收斂成 transport-neutral 的正式契約

## 問題

`threadBridge` 近期架構演化的核心，不是先抽新的 API，也不是先產品化多 adapter，而是把幾個角色徹底拆開：

- `desktop runtime owner`
- `runtime_control`
- `app-server ws observer`
- `hcodex` / ingress
- Telegram / management surface

現在真正的問題不是功能缺失，而是：

- 某些邊界已經在代碼裡成立，但文檔仍停留在較舊的抽象
- 某些理想分層還沒完全落地，但文檔已經把它寫成現在式
- observer、ingress、adoption state、與 management control 的長期 contract 仍有待收尾

如果不先把 owner/runtime contract 收斂清楚，後續不論是 observer 收尾、`hcodex` launch cleanup、還是 transport abstraction，都很容易重新把 authority、projection、與 adapter UX 黏回一起。

## 定位

這份文檔是 owner/runtime boundary 的總草稿，採用「描述現在 + 指出未來應如何演化」的寫法。

角色與責任邊界現在以 [runtime-architecture.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-architecture.md) 為 current architecture 主文檔；本文件保留高層 owner/runtime contract 的背景、方向、與收斂脈絡。

它處理：

- runtime authority 目前固定在哪一層
- shared control orchestration 目前固定在哪一層
- observer / ingress / adapter 目前各自實際承擔什麼責任
- 哪些邊界已成立，哪些還只是目標方向
- adoption 在 owner/runtime contract 上屬於哪一層語義

它不處理：

- Telegram renderer / callback UX 的完整產品規格
- `codex plan`、preview、delivery 等單一子問題的細節規格
- 完整 transport-neutral protocol 的最終 wire format
- adoption 的完整 state / action / UI 子規格
- 每一個 compatibility shim 的立即移除時程

## 當前代碼狀態

### 1. `desktop runtime owner`

目前 `desktop runtime owner` 已是唯一 runtime authority。

目前已成立：

- ensure / repair workspace runtime
- owner-canonical runtime health
- workspace-scoped control orchestration authority
- ensure workspace app-server 與 `hcodex ingress`

目前沒有承擔：

- Telegram message rendering
- preview / final reply 樣式
- adapter-specific callback UX

### 2. `runtime_control`

shared `runtime_control` 已不是純概念，而是目前 write-side orchestration 的主要內部邊界。

目前已承擔：

- workspace runtime ensure / control-path preparation
- workspace session bind / fresh session / repair
- Telegram input 對 live TUI session 的 routing / adoption 判定
- 供 Telegram 與 management surface 共用的 workspace/session control semantics

目前沒有承擔：

- Telegram message send / edit
- Telegram markup / callback UI
- app-server observer projection

### 3. `app-server ws observer`

observer 已不是純構想，而是已存在的 read-side runtime。

目前已承擔：

- thread-scoped event 訂閱
- preview / final / process projection
- session observability feed
- mirror intake contract
- adapter-neutral runtime interaction event 發送

目前不再直接承擔：

- Telegram `request_user_input` prompt bridging
- resolved request 的 Telegram UI cleanup
- plan mode follow-up prompt 發送

### 4. `hcodex` / ingress

`hcodex` 的 today 形狀，已經不是舊 TUI proxy 模型，但也還沒有窄到只剩一個薄 entrypoint。

目前已承擔：

- `ensure-hcodex-runtime`
- `resolve-hcodex-launch`
- `hcodex_ws_url + launch_ticket` ingress launch path
- local `hcodex-ws-bridge` compatibility boundary
- `run-hcodex-session`
- local session claim / launcher lifecycle 記錄
- live request-response injection

需要明確固定的一條約束是：

- `hcodex` 雖不是 runtime owner，但仍必須是自己啟動之本地 `codex --remote` child 的 process owner / lifecycle supervisor

也就是說，`desktop runtime owner` 擁有 machine-level runtime authority，而 `hcodex` 擁有 local TUI child lifecycle authority。這兩種 ownership 不能混為一談，但也不能把後者錯誤刪薄。

目前仍屬於 ingress / compatibility 邊界的一部分：

- websocket ingress listener / relay
- observer runtime 的掛接
- live request-response injection

這表示 `hcodex` / ingress 的主路徑其實已經很明確，但「哪些能力屬於長期入口契約、哪些只是過渡結構」仍未完全寫死。

### 5. Telegram / management surface

這兩個 surface 在 today 的代碼裡，已共享同一套 runtime semantics，但 transport-neutral contract 仍未完全收尾。

目前已存在：

- management API 已透過 shared `runtime_protocol` view / action / event naming 對外暴露 runtime semantics
- shared view model，例如 `RuntimeHealthView`、`ManagedWorkspaceView`、`ThreadStateView`
- typed SSE event，例如 `RuntimeEventKind`
- management API 上的 query / control / event stream
- Telegram 已依賴 shared transcript / control semantics，且互動式 prompt / follow-up 已移到 adapter-owned interaction bridge

目前尚未完全成立的是：

- Telegram 完全退回純 protocol consumer，而不是仍需少量 adapter-local control plumbing
- `runtime protocol` 尚未收斂成完整 transport-neutral public contract

### 6. adoption

adoption 在 today 的代碼裡已是 runtime state / control 的一部分，而不是單純 `hcodex` UI signal。

目前已成立：

- `tui_session_adoption_pending` 持久化在 binding state
- `runtime_readiness` 會派生出 `pending_adoption`
- Telegram 與 management API 都有 adopt / reject control surface
- `hcodex` session 結束後會標記 adoption pending

這份文檔只固定 adoption 的 ownership 邊界：

- 它屬於 runtime / state / control 模型
- 它不是 `hcodex` 單方擁有的 UI 語義

詳細 state / action / UX 規格不在這份文檔重複定義。

## 目標方向

### 1. `desktop runtime owner`

- 維持唯一 runtime authority
- 維持 owner-canonical runtime health
- 繼續避免 Telegram 或 `hcodex` 重新長回 owner 行為

### 2. `runtime_control`

- 維持 shared write-side orchestration 邊界
- 讓 Telegram / management surface 共用同一套 workspace/session control semantics
- 不把 Telegram UI、markup、callback handling 拉回 control core

### 3. observer

- 收斂成真正純粹的 read-side projection / observability runtime
- 維持由 adapter-neutral interaction event 對外發出互動需求
- 不把 Telegram prompt / markup / follow-up rendering 拉回 observer 主體

### 4. `hcodex` / ingress

- 收斂成受管本地入口
- 保留 binary selection、launch lifecycle、local session claim、必要 compatibility shim
- 正式保留 local `codex --remote` child supervision，而不是退化成單次 launch shim
- 避免再次承擔 mirror canonical projection 或 runtime authority

### 5. Telegram / management surface

- 更完整地透過 shared runtime semantics 工作
- 對 mirror、control、state 的依賴固定在 shared semantic layer / state model
- Telegram-specific 互動 UI 留在 adapter-owned bridge
- 減少對 ingress / observer 內部細節的直接耦合

### 6. adoption

- 保留它作為 runtime state / control 語義的一部分
- 命名與最終對外呈現仍可演化
- 不讓其實作再次退回成一條只存在於某個 adapter 或某個 ingress 路徑的隱性規則

## 與其他計劃的關係

- [app-server-ws-mirror-observer.md](/Volumes/Data/Github/threadBridge/docs/plan/app-server-ws-mirror-observer.md)
  - 處理 observer / mirror intake 的子問題與收尾
- [post-cli-runtime-cleanup.md](/Volumes/Data/Github/threadBridge/docs/plan/post-cli-runtime-cleanup.md)
  - 處理 `hcodex` launch contract、legacy artifact、與 compatibility 命名收尾
- [runtime-protocol.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-protocol.md)
  - 承接 shared event / action / view naming 與 transport-facing contract 細節
- [session-lifecycle.md](/Volumes/Data/Github/threadBridge/docs/plan/session-lifecycle.md)
  - 承接 thread / workspace / Codex thread continuity 與 adoption lifecycle
- [runtime-state-machine.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-state-machine.md)
  - 承接 canonical state axes、`pending_adoption`、與 control-side state semantics
- [runtime-transport-abstraction.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-transport-abstraction.md)
  - 這份文檔是 transport abstraction 之前的前置邊界收斂

這份文檔不重複定義上述子問題的實作細節，而是固定它們共同依附的 owner/runtime boundary。

## 開放問題

- `hcodex` ingress 中哪些 compatibility shim 屬於長期入口能力，哪些應視為過渡結構
- Telegram 何時才算完整退回 protocol consumer，而不再依賴 adapter-local control plumbing
- adoption 最終是否保留這個對外命名，或改成更中性的 continuity switch 語言
- shared runtime semantics 何時才算從 today 的 HTTP / SSE + shared views 收斂成更完整的 transport-neutral 契約

## 建議的下一步

- 把 shared `runtime_control`、observer projection、與 adapter interaction bridge 的分工固定成主文檔語言
- 把 `hcodex` 主路徑已成立的 launch contract 記錄清楚，再把未拍板的 shim 邊界列成 open questions
- 對 adoption 只保留 ownership 與 boundary 描述，將詳細 semantics 收斂回 `session-lifecycle`、`runtime-state-machine`、`runtime-protocol`
- 逐步讓 Telegram / management surface 對 mirror 與 control action 的依賴固定在 shared runtime semantics，而不是 observer / ingress 內部細節
