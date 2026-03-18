# Runtime 協議草稿

## 目前進度

這份文檔目前仍是純草稿，尚未成為代碼中的正式協議層。

目前代碼狀態：

- 仍以 Telegram bot 為主要入口
- 仍沒有 transport-neutral 的正式 runtime protocol
- 只有部分 runtime 語義已經隱含在現有 Rust 模組內

## 問題

如果 `threadBridge` 要從「Telegram bot」轉成「透明協議層 + 多前端 adapter」，只說抽象分層還不夠。

還需要一份清楚的協議定義，回答下面幾件事：

- 外部客戶端如何把輸入送進 runtime
- runtime 如何回報執行中的事件
- thread 狀態如何被查詢
- 工具執行、錯誤與 preview 如何被標準化表示

如果沒有這層協議，未來很容易發生：

- Telegram adapter 直接綁死一套內部 callback
- custom app 只能複製 Telegram 的流程
- core 與 adapter 之間仍然透過隱性假設耦合

## 方向

先定義一套 `threadBridge runtime protocol`。

這個 protocol 先不用急著定成公開網路標準，也不用急著承諾 gRPC、HTTP、WebSocket、stdio 哪一種傳輸。

第一步應該先固定：

- 語意模型
- 事件型別
- request / response shape
- thread state view

也就是：

- 先定「說什麼」
- 再定「怎麼傳」

## 協議目標

### 主要目標

- 讓 Telegram 與 custom app 共用同一套 runtime 語意
- 讓 preview、tool 執行、error、busy 狀態有一致事件模型
- 讓 observability 與 control surface 可以使用同一份核心資料

### 次要目標

- 讓 transport 實作更換時，不需要重寫核心流程
- 為之後的本地 HTTP API 或 WebSocket stream 留出穩定基礎

## 建議的協議物件

### 1. Thread Handle

代表外部客戶端眼中的 conversation 容器。

至少需要：

- `thread_key`
- `client_kind`
- `client_thread_ref`

其中：

- `thread_key`
  - bot-local / runtime-local 的穩定主鍵
- `client_kind`
  - `telegram`、`custom_app`、`cli`
- `client_thread_ref`
  - 該客戶端自己的 thread / conversation identifier

### 2. Input Event

代表外部送入 runtime 的使用者輸入。

建議型別：

- `text`
- `image`
- `command`
- `control_action`

建議共同欄位：

- `thread_key`
- `event_id`
- `timestamp`
- `sender`
- `kind`

文字事件額外欄位：

- `text`

圖片事件額外欄位：

- `image_paths`
- `caption`

命令事件額外欄位：

- `command_name`
- `args`

控制事件額外欄位：

- `action`
  - 例如 `new`、`reconnect_codex`

### 3. Runtime Event

代表 runtime 主動發出的執行事件。

建議型別：

- `thread_busy`
- `turn_started`
- `preview_delta`
- `preview_replaced`
- `tool_started`
- `tool_completed`
- `assistant_message`
- `assistant_final`
- `thread_state_changed`
- `error`

這一層應該避免平台詞彙，例如：

- 不要直接叫 `telegram_message_edited`
- 不要直接叫 `topic_title_updated`

因為那是 adapter 的呈現細節。

### 4. Thread State View

代表 thread 目前狀態的快照。

至少應包含：

- `thread_key`
- `workspace_cwd`
- `codex_thread_id`
- `binding_status`
  - `unbound` / `healthy` / `broken`
- `run_status`
  - `idle` / `running`
- `last_error`
- `last_used_at`

這個 view 未來可以同時供：

- Telegram `/status`
- custom app 狀態頁
- Web App observability

## 協議風格建議

### 事件優先，而不是同步 RPC 優先

`threadBridge` 的核心工作本來就偏事件流：

- preview 是流式的
- tool 執行是階段性的
- final response 是收尾事件

所以比較自然的模型是：

- `submit input`
- `subscribe runtime events`
- `query thread state`

而不是全部塞進單次同步 response。

### Query 與 Control 分離

建議明確區分：

- `query`
  - 讀取 thread / turn / artifact 狀態
- `control`
  - reset / reconnect / bind workspace

這樣 custom app 與 observability UI 都會比較清楚。

## 傳輸層選項

這篇先不定案，但可以先記錄適合的載體：

- process-local Rust API
- local HTTP + SSE
- local HTTP + WebSocket
- stdio JSON stream

短期最務實的路線可能是：

- 先有 process-local core API
- 再包一層 local HTTP / WebSocket adapter

## 與現有資料模型的關係

現有資料不必推翻，但要明確重新掛接到 protocol：

- `session-binding.json`
  - 主要餵給 `ThreadStateView`
- `conversations.jsonl`
  - 可重放為 turn / message 歷史
- `events.jsonl`
  - 可重建部分 `RuntimeEvent`
- `.threadbridge/tool_results/*`
  - 可補充 `tool_completed` 與 artifact 摘要

## 風險

- 若一開始把協議做太細，會造成實作成本過高
- 若協議只是照抄 Telegram 事件名稱，抽象將沒有價值
- 若 query / control / event stream 沒有切開，custom app 很快會變難做

## 開放問題

- busy gate 被拒絕的輸入，是否也應作為明確事件寫入協議？
- 圖片上傳後但延後分析的狀態，應該是 `accepted_pending` 還是另一種 artifact event？
- preview delta 是否要保留平台無關的 markdown / rich text 中間表示？
- tool output 是否需要標準欄位，還是只保留 generic payload？

## 建議的下一步

1. 先定義最小 `InputEvent`、`RuntimeEvent`、`ThreadStateView`。
2. 讓 Telegram adapter 先改用這套語意，而不是直接碰 runtime 細節。
3. 再選一個最小 transport 載體做實驗，例如 local HTTP + SSE。
