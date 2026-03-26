# App-Server Observer Upstream Capability Audit 草稿

## 目前進度

這份文檔目前仍是「純草稿」。

目前已確認的前提：

- 在切分 `AppServerMirrorObserverManager` 之前，應先盤點 `/Volumes/Data/Github/codex` 內 upstream `codex app-server ws backend` 的實際 observer / replay / thread-status 能力
- upstream protocol 已明確暴露：
  - `thread/unsubscribe`
  - `thread/status/changed`
  - `thread/realtime/closed`
  - `item/tool/requestUserInput`
- upstream 實作面至少已存在：
  - thread-scoped server notification / request intake
  - pending interactive replay
  - request replay / resolved cleanup 與 turn-complete 關聯

目前尚未完成：

- 尚未完成完整的 capability inventory
- 尚未把 protocol 承諾、server 實作、與 TUI app 層額外能力清楚分欄
- 尚未把 findings 轉成正式的 observer substrate / threadBridge projection 邊界建議

## 問題

目前 `threadBridge` 已有 `AppServerMirrorObserverManager`，但它不是單純的 observer substrate。

從 today code 看，它同時混合了兩類責任：

- backend-adjacent 能力
  - thread attach / source registry
  - raw daemon message intake
  - request / notification 解析
- threadBridge-specific projection
  - transcript mirror
  - workspace status 寫回
  - runtime interaction 路由
  - adapter-visible final / preview 組裝

如果在沒有先掌握 upstream `codex app-server ws backend` 能力的情況下直接切分，很容易犯兩種錯：

- 把 upstream 已有的 observer substrate 能力，又在 `threadBridge` 重做一層
- 誤以為 upstream 已承諾某些 replay / status / subscription contract，但其實只是 `threadBridge` 或 `tui_app_server` 自己的組裝

因此這份文檔不是重構方案，而是前置 capability audit。

## 定位

這份文檔是 `app-server-observer` owner 下的 upstream capability audit。

它處理：

- `/Volumes/Data/Github/codex` 內 app-server observer 相關原生能力盤點
- 哪些能力屬於 protocol / server 已承諾的 substrate
- 哪些能力只是 TUI app 或 current client 自己補的 observer glue
- 後續切分 `AppServerMirrorObserverManager` 時，哪些責任有資格往 backend observer substrate 收斂

它不處理：

- 直接給出最終 module 切分方案
- 直接宣告 `AppServerMirrorObserverManager` 哪些程式碼要搬動
- Telegram / management / transcript 的最終產品規格
- `runtime-architecture` 的 actor 重寫

## Audit 範圍

這份 audit 固定檢查 4 類能力。

### 1. subscription lifecycle

要回答：

- upstream attach 是否仍只能靠 `thread/resume`
- 是否已有正式 subscribe / unsubscribe contract
- realtime close / detach / unsubscribe 的語義是否足夠讓 client 正式管理 observer lifecycle

目前已知 evidence：

- protocol 已有 `thread/unsubscribe`
- protocol 已有 `thread/realtime/closed`

### 2. thread status / busy truth

要回答：

- upstream 是否已有 thread-scoped status stream
- `ThreadStatus` / `ThreadActiveFlag` 是否足以承接 observer health 與 native busy truth
- `threadBridge` 目前哪些 busy / running 推導，其實可以回到 upstream truth

目前已知 evidence：

- protocol 已有 `thread/status/changed`
- `ThreadStatus` 已至少區分：
  - `notLoaded`
  - `idle`
  - `systemError`
  - `active`

### 3. interactive replay

要回答：

- pending server request replay 是否屬 upstream 原生承諾
- `request_user_input` / approvals / elicitation replay 的邊界是 protocol、server，還是 TUI app 層附加能力
- turn-complete / request-resolved 後，哪些 replay cleanup 已由 upstream 承接

目前已知 evidence：

- protocol 已有 `item/tool/requestUserInput`
- `tui_app_server` 已存在 `pending_interactive_replay`
- upstream app 層已追蹤 unresolved interactive request 與 replay 過濾

### 4. projection boundary

要回答：

- upstream 只提供 raw typed event，還是已經提供更高層的 observer substrate
- `AppServerMirrorObserverManager` 目前哪些工作屬於真正的 read-side projection
- 哪些工作顯然仍應留在 `threadBridge`，例如 transcript artifact、workspace status、adapter-neutral interaction routing

## 主要 evidence anchors

這份 audit 目前以三類上游來源為主：

- protocol / schema
  - `/Volumes/Data/Github/codex/codex-rs/app-server-protocol/src/protocol/common.rs`
  - `/Volumes/Data/Github/codex/codex-rs/app-server-protocol/src/protocol/v2.rs`
- protocol schema 導出
  - `/Volumes/Data/Github/codex/codex-rs/app-server-protocol/schema/typescript/v2/ThreadStatusChangedNotification.ts`
  - `/Volumes/Data/Github/codex/codex-rs/app-server-protocol/schema/typescript/v2/ThreadRealtimeClosedNotification.ts`
  - `/Volumes/Data/Github/codex/codex-rs/app-server-protocol/schema/typescript/v2/ThreadStatus.ts`
- app / replay 實作
  - `/Volumes/Data/Github/codex/codex-rs/tui_app_server/src/app/pending_interactive_replay.rs`

若後續 audit 需要更深入，才再補 server-side request handling 或其他 app adapter code anchors。

## 預期輸出

這份 audit 最終應輸出兩欄結論，而不是直接下結論說「應該搬哪些檔案」。

### A. 可視為 upstream 已具備的 observer substrate

候選包括：

- thread attach / detach lifecycle contract
- thread status / realtime close 通知
- request replay / unresolved interactive prompt replay
- raw thread-scoped event stream

### B. 仍屬 threadBridge projection / product glue 的能力

候選包括：

- transcript mirror artifact
- workspace status 寫回
- runtime interaction bridge
- adapter-facing final / preview text 組裝
- 與 Telegram / management surface 綁定的 follow-up 語義

## 與其他計劃的關係

- [app-server-ws-mirror-observer.md](app-server-ws-mirror-observer.md)
  - 處理 observer intake boundary 與歷史成因
  - 本文提供它後續切分所需的 upstream capability 前置盤點
- [app-server-ws-backend.md](../desktop-runtime-owner/app-server-ws-backend.md)
  - 定義 owner-managed backend plane 的 today reality
  - 本文只處理 observer 相關 upstream 能力，不重寫 backend plane 全貌
- [runtime-architecture.md](../runtime-control/runtime-architecture.md)
  - 定義 canonical actor boundary
  - 本文不新增新的 actor，只為後續責任切分提供 capability evidence

## 開放問題

- `thread/status/changed` 的實際 active flag 粒度，是否已足夠支撐 threadBridge 所需的 observer / busy truth？
- pending interactive replay 到底應被視為 upstream app-server contract，還是 `tui_app_server` 這個特定 host 的額外能力？
- `AppServerMirrorObserverManager` 裡的 source registry / forwarded-source attach，最終是否有資格下沉到 backend observer substrate？
