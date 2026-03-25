# Codex Plan Mirror

## 目前進度

這份文檔描述的是一個**已落地主要修正**、但後續仍可再收斂觀測細節的 mirror 子問題。

目前已確認的代碼狀態：

- Codex upstream 已正式支持 plan mode 的結構化輸出：
  - app-server 會發 `item/plan/delta`
  - turn item 會發 `plan`
  - assistant message 內的 `<proposed_plan>` 內容會被剝離，不會作為最終 assistant 文本直接暴露
- `threadBridge` 已有 final / process transcript 分流，也已有 Telegram rolling preview 與 local/TUI mirror
- `threadBridge` 已能消費 `item/plan/delta`，並把 live plan snapshot 送進 process transcript / preview
- direct bot path 與 workspace-local `hcodex` / observer path 都已補上 finalized `plan` item 的 final reply fallback
- Telegram final reply 不再只依賴 upstream assistant final text；plan-only turn 會使用 finalized plan text，mixed case 會組合 assistant text 與 plan markdown

目前仍可後續收斂的部分：

- plan snapshot 是否需要進一步做 transcript compaction / coalescing
- management / observability UI 是否要對 combined final reply 做更明確的 plan 區塊呈現
- Telegram 的 `Questions` / `Implement this plan?` 最小互動面已接上同一路徑；若之後要做更一般的 interrupt / questionnaire surface，應視為 adapter follow-up，而不是這份 plan mirror 子規格的責任

## 問題

`threadBridge` 目前對 `codex plan` 的 mirror 缺口，不是因為 Codex upstream 仍只輸出 raw `<proposed_plan>` 標記，也不是因為 upstream 缺少可消費事件。

真正的問題是：

- upstream 已經把 plan 拆成獨立事件與 item
- `threadBridge` 目前只完整接上了 `agentMessage` delta 與部分 `plan` finalized item
- 過去 live `item/plan/delta` 沒有進入 `threadBridge` 的 normalization / process transcript / Telegram preview 路徑
- 過去 plan-only turn 在 upstream assistant final text 為空時，Telegram adapter 也沒有 fallback final reply

結果就是：

- live plan 已可進入 preview / process transcript
- Telegram 使用者在 plan-only turn 結束後，可收到 finalized plan text
- 若同一輪同時有 assistant final text 與 finalized plan，Telegram 會發送 combined final reply

## 已驗證的上游事實

這裡固定幾個已驗證、之後不應再重複懷疑的事實：

1. app-server protocol 已定義 `item/plan/delta`，它對應 `<proposed_plan>`。
2. Codex core 已驗證 plan mode 會把 plan 從 assistant message 中剝離。
3. 因此目前看到的 gap，主要應歸因於 `threadBridge` consumer / adapter，而不是 upstream source capability。

這代表：

- v1 不需要改 Codex upstream
- `threadBridge` 應直接在現有 mirror pipeline 上補 plan event intake 與 adapter fallback

## v1 目標

v1 要達成三件事：

1. live `item/plan/delta` 需要進入 mirror preview / process transcript 路徑
2. finalized `plan` item 需要穩定落入 process transcript
3. 若 turn 為 plan-only 且 upstream assistant final text 為空，Telegram 應以 finalized plan text 作為 final reply fallback
4. 若同一輪同時有 assistant final text 與 finalized plan，Telegram 應送出 combined final reply：
   - assistant text
   - `## Proposed Plan`
   - plan markdown

v1 明確不做：

- 不要求新增一條 preview-only 專用 event lane
- 不要求改 Codex upstream protocol
- 不改 final/process transcript 的 canonical 分工

## v1 設計

### 1. app-server event intake

在 `rust/src/codex.rs`：

- 為 `item/plan/delta` 新增 notification handling
- 以 `itemId` 為 key 累積完整 plan 文本
- 將每次更新映射為：
  - `CodexThreadEvent::ItemUpdated { item: { "type": "plan", "id": "...", "text": "..." } }`
- 不新增新的 public event enum，沿用既有 `ItemUpdated`

理由：

- 這讓 direct bot path 可以重用既有 preview / transcript pipeline
- 不必為 plan mode 開新的事件樹

### 2. process transcript normalization

在 `rust/src/process_transcript.rs`：

- `process_entry_from_codex_event` 需要接受 `ItemUpdated`
- `plan` 類 `ItemUpdated` 應映射為：
  - `TranscriptMirrorDelivery::Process`
  - `TranscriptMirrorPhase::Plan`
- raw websocket path 在 `hcodex_ingress.rs` 內直接累積 `item/plan/delta`
- 對 `item/plan/delta`，v1 直接使用「累積後的完整 plan snapshot 文本」作為 process transcript 文本

v1 的取捨是：

- 允許 process transcript 收到多個 plan snapshot
- `item/completed` 的 finalized `plan` item 仍是最終 authoritative state
- 暫時不引入額外 coalescing / preview-only 結構

如果之後 observability 覺得 plan snapshot 太噪音，再另外收斂 transcript compaction 規則。

### 3. App-Server Observer / Local Mirror

在 `rust/src/app_server_observer.rs` 這條 app-server observer 路徑：

- 保留既有 assistant preview segmentation reset 規則
- `item/plan/delta` 應能生成 plan process transcript event
- generated plan process transcript event 應繼續沿用既有 `record_hcodex_ingress_process_event(...)` 寫入路徑

這代表 v1 不需要新建另一套 local mirror persistence lane。

### 4. Telegram preview

Telegram rolling preview 仍沿用現在的模型：

- assistant draft text 來自 assistant message delta / completed item
- process preview 來自 process transcript entry

因此 v1 不要求大改 `preview.rs` 的角色模型。

只要：

- live `item/plan/delta` 能變成 plan process transcript entry
- preview controller 繼續消費 process entry

就能讓 plan text 在 assistant draft 不存在時顯示為 preview/status 文本。

### 5. Telegram final reply fallback

在 `rust/src/telegram_runtime/thread_flow.rs` 與圖片 / media 的對應 turn completion 路徑：

- final reply 文本的選擇規則固定為：
  1. 只有 `final_response`：直接送 `final_response`
  2. 只有 finalized plan text：直接送 plan markdown
  3. 兩者同時存在：送 `final_response + ## Proposed Plan + plan markdown`
  4. 兩者都空：不送 final assistant reply

這裡要明確保持一個語義：

- upstream assistant / plan 分工仍不變
- `threadBridge` final transcript 記錄的是 user-visible final assistant delivery text，因此 mixed case 允許 final transcript 與 process transcript 同時包含 plan

## 實作邊界

這份改動主要會落在：

- `rust/src/codex.rs`
- `rust/src/process_transcript.rs`
- `rust/src/hcodex_ingress.rs`
- `rust/src/telegram_runtime/thread_flow.rs`
- `rust/src/telegram_runtime/media.rs`
- `rust/src/telegram_runtime/final_reply.rs`

其中：

- `codex.rs` 解決 direct bot path 的 event intake
- `process_transcript.rs` 收斂 direct bot path 與 raw workspace path 的 plan normalization
- `app_server_observer.rs` 承接 local/TUI mirror 的 process transcript record
- Telegram runtime completion path 解決 plan-only 與 mixed case 的 final reply 組裝

## 測試要求

至少補齊下面幾類測試：

- `codex.rs`
  - `item/plan/delta` 依 `itemId` 累積為 `ItemUpdated(plan)`
- `process_transcript.rs`
  - `CodexThreadEvent::ItemUpdated(plan)` 會映射成 `Process + Plan`
  - raw websocket `item/plan/delta` 也會映射成 `Process + Plan`
- Telegram adapter
  - plan-only turn 在 `final_response` 為空時，會用 finalized plan text 送出 final reply
  - mixed case 會送出 assistant text + `## Proposed Plan` + plan markdown
- local/TUI mirror
  - `item/plan/delta` 會產生 process transcript，而不是被靜默忽略

同時要保證不回歸：

- `agentMessage` delta preview
- tool process transcript
- 非 plan turn 的 final reply 流程

## 與其他文檔的關係

- [session-level-mirror-and-readiness.md](/Volumes/Data/Github/threadBridge/docs/plan/session-level-mirror-and-readiness.md)
  - 保留高層 mirror 模型
  - 這份文檔則作為 `codex plan` mirror 缺口的子規格
- [runtime-protocol.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-protocol.md)
  - 保留 transport-neutral view / event 命名
  - 這份文檔補的是其中一條具體未收斂 event lane
- [codex-app-server-ws-protocol.md](/Volumes/Data/Github/threadBridge/docs/codex-app-server-ws-protocol.md)
  - 保留 upstream wire facts
  - 不在那份文檔裡重複寫設計決策

## 暫定結論

`codex plan` mirror 的核心結論是：

- 上游 source 已存在
- `threadBridge` 目前缺的是 consumer 與 adapter fallback
- v1 應優先把 `item/plan/delta` 接進 mirror pipeline，並補齊 plan-only Telegram final reply fallback

這樣就能在不改動 Codex upstream 的前提下，讓 `threadBridge` 的 plan mode mirror 進入可用狀態。
