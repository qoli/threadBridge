# App-Server WS Backend 草稿

## 目前進度

這份文檔目前已進入「部分落地」。

目前代碼中已成立的部分：

- `app-server-ws-backend` 已是 `threadBridge` today runtime 實際依賴的核心 backend plane
- `desktop runtime owner` 已擁有它的 lifecycle authority，並負責 workspace-scoped ensure / repair / reconcile
- shared workspace app-server daemon、workspace-local state file、與 `hcodex ingress` 上游 backend 已落地
- `CodexRunner`、observer、`hcodex ingress`、session verify / repair 都已實際依賴同一條 app-server ws backend contract
- `thread/start`、`thread/resume`、`thread/read`、`turn/start`、interrupt、server request / notification intake 都已走 app-server JSON-RPC / ws 路徑

目前尚未完成的部分：

- 這個 backend plane 仍未在架構文檔與代碼分層中被完整當成單一子系統承認
- today code 中與它相關的責任仍散落在 `codex.rs`、`app_server_runtime.rs`、`app_server_observer.rs`、`hcodex_ingress.rs`、`runtime_owner.rs`、`runtime_control.rs`
- `observer` attach 仍建立在 `thread/resume` attach 語義上，而不是正式 upstream subscribe API
- backend plane 與 shared runtime semantics 的長期 API 形狀仍未收斂成獨立 contract

## 問題

`threadBridge` 目前已經不是「Telegram bot 控制一個附帶的 Codex helper」的形狀。

今天真正的 runtime 執行底座其實是：

- workspace-scoped `codex app-server` daemon
- 它暴露的 websocket / JSON-RPC contract
- 圍繞這個 backend 的 thread continuity、turn execution、event stream、observer attach、與 ingress relay

問題在於，這個 backend reality 雖然已經成立，但它在文檔與代碼裡仍常被拆成幾個零件分別描述：

- 有時被看成 `desktop runtime owner` 的內部實作
- 有時被看成 `runtime_control` 依賴的 transport helper
- 有時被看成 observer / ingress 的上游事件來源
- 有時又只被稱為 shared app-server daemon

如果不把這個 backend plane 的 today reality 寫清楚，後續很容易出現兩種錯誤收斂：

- 把 backend plane 本身錯誤吞進 `desktop runtime owner`，讓 owner 與 backend 本體混在一起
- 把與 backend 相關的散落責任繼續掛在 observer / ingress / adapter 旁邊，誤以為它們只是局部 helper

## 定位

這份文檔是 `desktop runtime owner` 下面的 backend 子系統說明。

它處理：

- `app-server-ws-backend` 在 today code 中實際承擔哪些 runtime 能力
- 為什麼它是 `threadBridge` today runtime 的核心 backend plane
- 為什麼它的 owner 是 `desktop runtime owner`
- 它與 `runtime_control`、observer、`hcodex ingress`、Telegram / management surface 的邊界

它不處理：

- 新增一個平級 canonical actor
- 直接宣告新的 transport-neutral public API
- Telegram / management UI 的完整產品規格
- `session-lifecycle`、`runtime-state-machine`、`runtime-protocol` 的完整語義重寫
- 立即展開 backend process 抽離或跨進程重構步驟

## 當前代碼狀態

### 1. backend process / runtime instance

目前 `app-server-ws-backend` 作為 workspace-scoped backend instance，已由：

- [app_server_runtime.rs](../../../rust/src/app_server_runtime.rs)

承擔下列能力：

- spawn shared `codex app-server`
- health check 與 endpoint liveness probe
- workspace-local state file `./.threadbridge/state/app-server/current.json`
- `daemon_ws_url` 與 `hcodex_ws_url` 的 runtime surface
- launch ticket 發放 / 消耗

這一層是 backend plane 的 process / endpoint substrate，不是 adapter。

### 2. backend protocol client / thread-turn contract

目前 `app-server-ws-backend` 的上游 RPC / ws contract，已由：

- [codex.rs](../../../rust/src/codex.rs)

承擔主要 client 側能力，包括：

- stdio / websocket app-server transport
- `initialize` / `initialized`
- `thread/start` / `thread/read` / `thread/resume`
- `turn/start`
- notification / server request mapping
- thread cwd continuity 驗證
- observer attach 前的 `thread/resume`

這一層不是 owner，也不是 surface；它是 backend contract 的主要 client-facing 入口。

### 3. backend event stream consumption

目前 `app-server-ws-backend` 的 thread-scoped event stream，已由：

- [app_server_observer.rs](../../../rust/src/app_server_observer.rs)

承擔 read-side consumption 與 projection，包括：

- `thread/resume` attach
- preview / process / final mirror intake
- adapter-neutral interaction event emission
- runtime interaction resolved / turn completed follow-up

observer 消費 backend，但不是 backend owner。

### 4. backend ingress / local TUI path

目前 `hcodex` 本地路徑會透過：

- [hcodex_ingress.rs](../../../rust/src/hcodex_ingress.rs)

接到同一個 backend plane，並承擔：

- local websocket ingress listener
- client <-> daemon relay
- launch ticket / thread identity sideband 配合
- live request-response injection
- TUI session / turn metadata 追蹤

這一層是 backend 的接入與 relay path，不是 runtime authority。

### 5. backend lifecycle authority

目前 `desktop runtime owner` 透過：

- [runtime_owner.rs](../../../rust/src/runtime_owner.rs)

擁有 machine-level lifecycle authority，包括：

- workspace runtime reconcile
- ensure shared app-server daemon
- ensure `hcodex ingress`
- publish owner-canonical runtime heartbeat / health

這表示 owner 擁有 backend plane 的生命週期 authority，但 owner 不等於 backend plane 本體。

### 6. backend 上層 shared semantics

目前 shared `runtime_control` 透過：

- [runtime_control.rs](../../../rust/src/runtime_control.rs)

消費 backend plane 來表達 `threadBridge` 自己的 shared runtime semantics，包括：

- workspace runtime control-path preparation
- workspace session bind / fresh session / repair
- owner-managed runtime state 讀取與驗證
- Telegram-to-live-TUI routing

這一層是在 backend plane 之上表達 `threadBridge` product/runtime semantics，不是 backend plane 本身。

## 責任邊界

### `app-server-ws-backend` today 應被理解成什麼

- `threadBridge` 當前實際依賴的核心 Codex runtime backend plane
- 所有 Codex client communication 共同匯入的 backend contract
- workspace-scoped runtime substrate，而不是單次 turn helper

### `desktop runtime owner` 與它的關係

- `desktop runtime owner` 是 lifecycle authority
- 它決定 ensure / repair / reconcile / publish health
- 但它不是 backend plane 本體，也不應吞掉所有 backend-facing protocol 細節

### `runtime_control` 與它的關係

- `runtime_control` 不擁有 backend plane
- 它消費 backend plane，來承接 `threadBridge` 自己的 shared control semantics
- session bind / verify / repair 屬於 shared runtime semantics，不等於 backend plane 本體

### observer / ingress 與它的關係

- observer 消費 backend plane 的 event stream
- ingress 提供本地 TUI 連到 backend plane 的接入與 relay path
- 兩者都不應被誤讀成 backend authority

### adapter / surface 與它的關係

- Telegram / management surface 都只是 consumer / presenter
- 它們可以觸發 control action、觀測 backend-driven state，但不是 backend owner

## 與其他計劃的關係

- [owner-runtime-contract.md](owner-runtime-contract.md)
  - 提供 owner/runtime boundary 的高層總草稿
  - 本文承接其中「owner 所管理的 backend plane 究竟是什麼」這個子問題
- [runtime-architecture.md](../runtime-control/runtime-architecture.md)
  - 定義 canonical actor boundary
  - 本文不新增新的平級 actor，只補充 backend plane 的 today reality
- [app-server-ws-mirror-observer.md](../app-server-observer/app-server-ws-mirror-observer.md)
  - 處理 observer / mirror intake 的子問題
  - 本文只界定 observer 與 backend plane 的關係
- [session-lifecycle.md](../runtime-control/session-lifecycle.md)
  - 處理 workspace / Codex thread continuity 與 session control
  - 本文不重寫 session lifecycle，只說明這些控制目前如何依賴 backend plane
- [runtime-protocol.md](../runtime-control/runtime-protocol.md)
  - 處理 shared runtime views / actions / events naming
  - 本文不把 backend plane 直接等同於 transport-neutral runtime protocol

## 開放問題

- 長期來看，`app-server-ws-backend` 是否應進一步被抽象成更獨立的 backend API plane，而不是仍以 today code 的分散形狀存在？
- 如果未來要做這種抽象，哪些能力仍應留在 `threadBridge` shared runtime semantics，而不應下沉進 backend plane？
- `thread/resume` attach 語義在 observer 路徑上的長期 contract，是否應等 upstream subscribe API 更明確後再收斂？
