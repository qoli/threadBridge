# Codex App-Server WebSocket 協議說明

日期：`2026-03-22`

這份文檔的事實來源：

- `/Volumes/Data/Github/codex/codex-rs/app-server-protocol/src/protocol/common.rs`
- `/Volumes/Data/Github/codex/codex-rs/app-server-protocol/src/protocol/v2.rs`
- `/Volumes/Data/Github/codex/codex-rs/app-server-protocol/src/protocol/thread_history.rs`
- `/Volumes/Data/Github/threadBridge/rust/src/codex.rs`

這不是一份 transport-neutral 的 `threadBridge runtime protocol`。
它描述的是 `threadBridge` 今天實際連接的上游 `codex app-server` WebSocket / JSON-RPC 協議面。

## 總覽

`codex app-server` 透過 WebSocket 傳 JSON-RPC。

對 `threadBridge` 來說，核心模型是：

- client 發 JSON-RPC request，例如：
  - `thread/start`
  - `thread/read`
  - `thread/resume`
  - `turn/start`
- server 對這些 request 回傳標準 JSON-RPC response
- turn 執行期間，server 會額外推送 notification，例如：
  - `thread/started`
  - `turn/started`
  - `item/started`
  - `item/completed`
  - `item/agentMessage/delta`
  - `turn/completed`

這裡最容易踩錯的一點是：

- 上游 `ThreadItem` 的 `type` 是 camelCase
- 例如：
  - `agentMessage`
  - `plan`
  - `commandExecution`
  - `mcpToolCall`
  - `webSearch`

它不是 `threadBridge` 內部後續常見的 snake_case 形狀。

## 傳輸封包形狀

協議使用 JSON-RPC envelope。

client request 例子：

```json
{
  "jsonrpc": "2.0",
  "id": 42,
  "method": "turn/start",
  "params": {
    "threadId": "thr_123",
    "input": [
      { "type": "text", "text": "hello" }
    ]
  }
}
```

server response 例子：

```json
{
  "jsonrpc": "2.0",
  "id": 42,
  "result": {
    "...": "..."
  }
}
```

server notification 例子：

```json
{
  "jsonrpc": "2.0",
  "method": "item/started",
  "params": {
    "...": "..."
  }
}
```

補充：

- app-server 也可能在 turn 中主動送出 JSON-RPC server request，而不是只有 notification。
- `threadBridge` 現在已開始正式消費 `item/tool/requestUserInput`，並在 Telegram / TUI proxy 路徑上回送對應 JSON-RPC response。
- `serverRequest/resolved` 會被視為 pending interactive request 的 authoritative cleanup 邊界。

## `threadBridge` 目前實際使用的 request

`threadBridge` 目前只依賴這一小部分：

- `initialize`
- `thread/start`
- `thread/read`
- `thread/resume`
- `turn/start`

相關代碼：

- [codex.rs](/Volumes/Data/Github/threadBridge/rust/src/codex.rs)

### `thread/start`

用途：

- 為某個 workspace 建立新的 app-server thread

`threadBridge` 送出的 params 形狀：

```json
{
  "cwd": "/abs/workspace/path"
}
```

`threadBridge` 依賴 response 中的：

- `result.thread.id`
- `result.thread.cwd`

若 `thread.cwd` 缺席，會 fallback 讀 `result.cwd`。

### `thread/read`

用途：

- 純 continuity / health check

`threadBridge` 送出的 params：

```json
{
  "threadId": "thr_123",
  "includeTurns": false
}
```

### `thread/resume`

用途：

- 在已有 `thread.id` 的情況下，恢復既有 session

`threadBridge` 預期：

- 回來的 thread id 必須和原本一致

### `turn/start`

用途：

- 對既有 thread 送一輪新輸入

`threadBridge` 送出的 params：

```json
{
  "threadId": "thr_123",
  "input": [
    { "type": "text", "text": "..." }
  ]
}
```

注意：

- `turn/start` 的 JSON-RPC response 只代表 request 被接受
- 真正的 turn 過程與結果，靠 notification 串流回來
- `threadBridge` 現在也會在需要時帶 `collaborationMode`
  - direct Telegram thread 會使用 sticky thread-local mode
  - local TUI proxy 也會追蹤 `turn/start.collaborationMode`

## 對 `threadBridge` 重要的 notification

依據上游 `common.rs`，對 `threadBridge` 目前最重要的 methods 有：

- `thread/started`
- `turn/started`
- `turn/completed`
- `item/started`
- `item/completed`
- `item/agentMessage/delta`
- `item/plan/delta`
- `item/commandExecution/outputDelta`
- `item/mcpToolCall/progress`
- `item/reasoning/summaryTextDelta`
- `item/reasoning/textDelta`
- `serverRequest/resolved`

對 `threadBridge` 目前已開始消費的重要 server request：

- `item/tool/requestUserInput`

`threadBridge` 現在直接消費的主要 methods 包括：

- `thread/started`
- `turn/started`
- `turn/completed`
- `item/started`
- `item/completed`
- `item/agentMessage/delta`
- `item/plan/delta`
- `serverRequest/resolved`
- `error`

也就是說，`threadBridge` 已經直接消費 `item/plan/delta`，並把它接進 plan mirror / process transcript / Telegram preview 路徑。
另外要明確記住：`~/.codex/sessions/*.jsonl` 裡的 raw `<proposed_plan>` 只屬於證據層；runtime source of truth 仍然是 app-server 的 wire-level `item/plan/delta` 與 completed `plan` item。

## `ThreadItem` 真實形狀

上游的 `ItemStartedNotification` / `ItemCompletedNotification` 形狀是：

```json
{
  "threadId": "thr_123",
  "turnId": "turn_123",
  "item": {
    "type": "commandExecution",
    "...": "..."
  }
}
```

其中 `item` 是 tagged union，名字叫 `ThreadItem`。

對 `threadBridge` 目前最重要的 variants：

- `agentMessage`
- `plan`
- `commandExecution`
- `mcpToolCall`
- `webSearch`

### `agentMessage`

```json
{
  "type": "agentMessage",
  "id": "item_1",
  "text": "Final reply",
  "phase": "finalAnswer",
  "memoryCitation": null
}
```

### `plan`

```json
{
  "type": "plan",
  "id": "item_2",
  "text": "Inspect latest commits"
}
```

### `commandExecution`

```json
{
  "type": "commandExecution",
  "id": "item_3",
  "command": "git log -2",
  "cwd": "/Volumes/Data/Github/codex",
  "processId": "12345",
  "source": "...",
  "status": "inProgress",
  "commandActions": [],
  "aggregatedOutput": null,
  "exitCode": null,
  "durationMs": null
}
```

### `mcpToolCall`

```json
{
  "type": "mcpToolCall",
  "id": "item_4",
  "server": "server-name",
  "tool": "tool-name",
  "status": "inProgress",
  "arguments": {},
  "result": null,
  "error": null,
  "durationMs": null
}
```

### `webSearch`

```json
{
  "type": "webSearch",
  "id": "item_5",
  "query": "ratatui app-server redraw",
  "action": null
}
```

## assistant 文本的串流方式

assistant 最終輸出不是只在 turn 結束時才出現。
上游至少有兩條相關通道。

### 1. `item/agentMessage/delta`

這是 live streaming 通道。

上游 payload：

```json
{
  "threadId": "thr_123",
  "turnId": "turn_123",
  "itemId": "item_7",
  "delta": "partial text"
}
```

重要特性：

- delta 是掛在 `itemId` 上
- 它是增量，不是完整快照
- 同一個 `itemId` 的所有 delta 串起來，才是那個 agent message item 的完整文本

### 2. `item/completed` 且 `type = "agentMessage"`

這是完成態的 message item。

它帶的是完整 assistant 文本。

### 3. `turn/completed`

這代表整輪 turn 結束。

對 `threadBridge` 來說：

- 這是 turn boundary
- 但它不是 Telegram draft UX 所需的「分段完成信號」

也就是說，不能因為看到 `turn/completed`，就假設 upstream 已經幫你把「工具前說明」「工具後補充」「最終回答」切成了你想要的幾段。

## 工具 / 計劃事件的語義

上游協議本來就把 tool-like 活動和 assistant prose 分開建模。

在 `thread_history.rs` 裡：

- `ExecCommandBegin` / `ExecCommandEnd` 會進 `ThreadItem::CommandExecution`
- `WebSearchBegin` / `WebSearchEnd` 會進 `ThreadItem::WebSearch`
- `McpToolCallBegin` / `McpToolCallEnd` 會進 `ThreadItem::McpToolCall`
- plan 更新走 `ThreadItem::Plan`
- assistant 正文走 `ThreadItem::AgentMessage`

這對 `threadBridge` 很重要，因為：

- 工具邊界應從 tool item 得出
- assistant preview 應從 `agentMessage` delta / completed item 得出
- 不能只靠 `turn_completed.last-assistant-message` 反推 Telegram mirror 的分段

## `threadBridge` 目前怎麼做 normalization

`threadBridge` 內部不直接把 upstream payload 原樣往下傳。

在 [codex.rs](/Volumes/Data/Github/threadBridge/rust/src/codex.rs)：

- `item/started` 會變成 `CodexThreadEvent::ItemStarted`
- `item/completed` 會變成 `CodexThreadEvent::ItemCompleted`
- `item/agentMessage/delta` 會按 `itemId` 累積，然後重新發成 `CodexThreadEvent::ItemUpdated`
- `threadBridge` 內部看到的 item 會被 normalize 成 snake_case，例如：
  - `agent_message`
  - `command_execution`

這一層是 `threadBridge` 自己的內部視圖。
它不是 upstream app-server wire format。

## 對 `threadBridge` 最重要的幾個坑

### 1. upstream `ThreadItem.type` 是 camelCase

真實 wire value 是：

- `agentMessage`
- `commandExecution`
- `mcpToolCall`
- `webSearch`
- `plan`

如果在 WebSocket 邊界直接假設 snake_case，就會靜默漏掉：

- tool 邊界
- plan 更新
- preview segmentation 點

### 2. `item/agentMessage/delta` 是按 `itemId` 分段，不是按 turn

如果只按 turn 去累積 assistant 文本，很容易把：

- 工具前說明
- 工具後補充
- 最終回答

全部合併成一段。

### 3. `turn/completed` 不足以還原 preview phase

`turn/completed` 只能告訴你這輪結束了。
它不能直接回答：

- 現在 draft 應該替換成哪一段 assistant 文本
- 哪裡才是工具邊界之後的新 assistant segment

### 4. tool item 本來就是天然分段點

因為 upstream 已經把 tool / plan item 分出來了，所以 `threadBridge` 在做 TUI -> Telegram draft mirror 時，應優先把這些 item notification 當成 assistant preview 的切段邊界。

## 實務上的排查順序

如果你在 debug mirror / preview：

1. 先確認 raw app-server `ThreadItem.type` 有沒有按 camelCase 正確解析
2. 再確認 `item/agentMessage/delta` 有沒有按 `itemId` 分組
3. 再確認 tool item notification 有沒有拿來當 assistant preview segmentation 邊界
4. 最後才去看 Telegram `sendMessageDraft` 發送層

實務上：

- draft 完全不出來，可能是 preview event 根本沒有生成
- draft 合併成大段，通常是只按 turn 累積 assistant 文本
- process transcript 完全沒有，通常是 `ThreadItem.type` 在 WebSocket 邊界就解析錯了

## 這份文檔的邊界

這份文檔只覆蓋 `threadBridge` 目前實際依賴的 app-server 協議子集。

它沒有完整展開：

- guardian approval review notifications
- realtime conversation notifications
- 全量 MCP progress payload
- account / setup / config 類 notification
- 非 WebSocket 傳輸

如果之後 `threadBridge` 直接消費更多 app-server notification，應該再回到 upstream source 補這份文檔，而不是從 `threadBridge` 內部 normalize 後的資料反推。
