# Message Queue And Status Delivery 草稿

## 問題

`threadBridge` 現在已經有多條 Telegram 送信 surface：

- preview draft
- final assistant reply
- plain system / control message
- restore page message edit
- media batch control message edit
- workspace outbox deliver

但目前還沒有一份專門描述 outbound delivery 的主規格。結果是：

- preview、final、status、edit 的關係只能從程式碼猜
- 哪些訊息應該 FIFO、哪些可以 coalesce，沒有清楚語義
- 之後要做 busy gate、history、Web App observability 時，很難知道「送信層」的責任邊界

## 定位

這份文件只規範 Telegram adapter 的 outbound delivery v1。

明確不處理：

- transport-neutral runtime protocol
- inbound user input queue
- history / unread pagination
- cross-machine delivery

## 核心原則

- `threadBridge` 的 delivery lane 以 `thread_key` 為分區，而不是 per-chat 或 per-user。
- content 與 status 不是同一種 payload，應該明確分開。
- preview 仍然是 Telegram draft surface，不應和 final reply 混成同一套 parse/render 行為。
- v1 不引入 persistent outbound queue。

## Queue Partition

每個 `thread_key` 一條 outbound delivery lane。

原因：

- `threadBridge` 的主要互動單位是 Telegram topic / thread
- 不同 topic 不應互相阻塞
- `coco` 的 per-user queue 適合它自己的 session 模型，但不適合 `threadBridge` 現在的 topic-bound runtime

## Delivery Item 類型

### `content`

真正送給使用者閱讀或下載的內容。

包括：

- final assistant text reply
- plain fallback reply
- overflow notice
- `reply.md` attachment
- workspace outbox text
- workspace outbox media / document

### `draft`

preview draft surface。

包括：

- `sendMessageDraft`
- draft heartbeat 更新
- draft 狀態文字更新

### `status`

短期狀態提示，但不是最終內容。

包括：

- busy / unavailable 類 plain text 提示
- restore 成功提示
- media batch 狀態提示

### `edit`

對已存在 Telegram message 的更新。

包括：

- restore page `editMessageText`
- media control message update

## 現有 Telegram Surfaces

### Preview Draft

- 使用 `sendMessageDraft`
- 保持 plain text
- 不做 Telegram HTML render
- heartbeat 與狀態更新屬於 `draft`

### Final Assistant Reply

- 使用 final reply renderer
- 優先 Telegram HTML
- 失敗時退回 plain text
- 過長時改成 notice + `reply.md`
- 屬於 `content`

### Plain Control / System Message

- 經由 `send_scoped_message`
- 不做 rich-text rendering
- 屬於 `status` 或 `content`，取決於用途

### Restore / Media Control Message

- 這些是已存在訊息的更新
- 屬於 `edit`

## Ordering 規則

### 同一個 `thread_key`

- `content` 必須 FIFO
- `content` 不允許被 `status` 插隊
- final reply 發送前，preview heartbeat 與 typing heartbeat 必須停止
- overflow notice 必須先於 `reply.md` attachment 發出

### `draft`

- `draft` 可 coalesce
- 同一個 draft key 永遠只保留最新一版 render
- draft 發送失敗不應阻塞 final reply

### `status` 與 `edit`

- `status` 可以做 latest-wins 合併
- `edit` 以 target message 為 key，較舊的待送 edit 可被覆蓋
- `edit` 不得重排已經發出去的 `content`

## Parse / Preview 規則

- preview 永遠是 plain text draft
- final reply 才走 Telegram HTML renderer
- 所有文字 send / edit 路徑都關閉 link preview
- final reply 的 render policy 不應隱式套用到 preview、restore page、media control

## Failure Semantics

### Preview

- draft 發送失敗只記 log
- 不重試成普通 message
- 不阻塞後續 final reply

### Final Reply

- HTML 送信失敗時，retry 一次 plain text
- plain text 若仍失敗，視為 final delivery failure
- attachment cleanup 是 best-effort

### Edit 類

- restore page 或 media control edit 失敗時記 log
- 失敗不影響已送出的 final content

## 不在這份文件裡解決的事

下面這些要在其他計劃裡定義，不在這裡偷帶：

- 同一 thread 的新 user input 是否排隊
- `running` 狀態下是 reject 還是 queue
- `/history`、unread range、history pagination
- runtime-generic `RuntimeEvent` stream

## 與其他計劃的關係

- `runtime-state-machine`
  - 定義 thread 本體狀態；這份只定義 delivery lane
- `codex-busy-input-gate`
  - 定義 inbound gate；這份不把 outbound lane 等同成 input queue
- `telegram-markdown-adaptation`
  - final reply renderer 屬於這份 delivery 文檔裡的 `content` 規則

## 暫定結論

`threadBridge` 的 Telegram delivery v1 應理解成：

- per-thread outbound lane
- preview draft 與 final reply 分離
- `content`、`draft`、`status`、`edit` 分類明確
- queue 是 outbound delivery queue，不是 user input queue
