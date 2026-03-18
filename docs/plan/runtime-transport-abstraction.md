# Runtime / Transport 抽象化草稿

## 目前進度

這份文檔目前仍是草稿，尚未正式開始重構。

目前已有一些前置跡象：

- workspace status 與 title watcher 已經開始把 Telegram UI 和 runtime 狀態分開
- final reply renderer 已經有比較清楚的 Telegram 表示層

但整體上 `threadBridge` 目前仍然是 Telegram-first 結構。

## 問題

目前 `threadBridge` 的產品邊界仍然偏向：

- 一個 Telegram bot
- 內部再去控制 Codex app-server
- 最後把結果送回 Telegram

這個模型在早期是合理的，但它會自然帶來一個限制：

- Telegram 容易被誤認成 runtime 的核心
- Telegram-specific 行為容易滲進 session control、preview、error handling、renderer 等核心流程
- 若未來要接 `custom app`、Web UI、CLI 或其他通道，會發現很多邏輯其實綁死在 Telegram 假設上

如果未來要讓 `threadBridge` 成為一個可被多種前端或宿主重用的本地 runtime，應該把它重新描述成：

- `Core runtime`
- `Transport adapter`

而不是：

- `Telegram bot` 加上一些內部模組

## 方向

把 `threadBridge` 的核心責任收斂成一個和 transport 無關的 runtime。

新的產品心智模型應該是：

- `threadBridge core`
  - 管理 thread lifecycle
  - 管理 workspace binding
  - 控制 Codex thread
  - 處理工具執行與 artifact 邊界
  - 發出標準化 runtime event
- `transport adapter`
  - 接收外部輸入
  - 將輸入轉成 core 可接受的 request / event
  - 將 core event 轉成平台可理解的 UI 呈現
- `client surface`
  - Telegram
  - custom app
  - CLI
  - future Web UI

## 核心原則

### 原則 1：Telegram 不是 runtime 模型本身

Telegram thread 仍然可以是預設 UI 容器，但不應再是核心資料模型唯一的外部表現。

runtime 應該只知道：

- 有一個外部 thread / conversation handle
- 有一組使用者輸入事件
- 有一組輸出事件要回送給 adapter

而不應該直接依賴：

- Telegram markdown 細節
- topic title API
- callback query 型別
- Telegram-specific media sending 模式

### 原則 2：平台表示層留在 adapter

下面這些都應該是 adapter 責任，而不是 runtime 責任：

- Telegram markdown / HTML renderer
- preview message 更新策略
- topic title 狀態欄
- slash command 對映
- custom app 的 UI action 與 button 表示

### 原則 3：runtime 對外只暴露穩定語意

runtime 對 adapter 應該提供穩定語意，而不是平台特定 callback：

- `thread is busy`
- `turn started`
- `preview delta`
- `tool started`
- `tool finished`
- `assistant final message`
- `binding broken`

這樣 Telegram 和 custom app 才能共享同一套核心執行模型。

## 建議的分層

### Layer 1: Core Runtime

責任：

- thread state machine
- workspace binding 驗證
- Codex app-server thread create / resume / reset / reconnect
- tool request/result orchestration
- artifact path ownership
- runtime event emission

不負責：

- Telegram message send / edit
- Telegram media upload
- custom app UI 組件
- 任何平台專用格式化

### Layer 2: Transport Adapter

責任：

- 平台輸入轉換
- 平台輸出渲染
- 平台命令路由
- 平台 session / identity 對映
- 將 runtime event 呈現成該平台適合的互動方式

例子：

- `TelegramAdapter`
- `LocalWebAppAdapter`
- `CliAdapter`

### Layer 3: Client Surface

這一層不一定要在 Rust core 內部，但產品語意上應清楚存在：

- Telegram
- 自定義 app
- 本地觀測面

## 對現有模組的意義

目前 repo 其實已經有一些自然邊界，只是還沒有被正式命名成 transport abstraction：

- `rust/src/codex.rs`
  - 偏向 core runtime
- `rust/src/repository.rs`
  - 偏向 core runtime
- `rust/src/workspace.rs`
  - 偏向 core runtime
- `rust/src/telegram_runtime/`
  - 偏向 adapter，但目前可能仍摻雜一些核心流程假設
- `rust/src/bin/threadbridge.rs`
  - 目前像是 Telegram app 的 entrypoint，未來應更像 adapter-specific launcher

## 建議的重構語言

未來的文件與程式碼應逐步固定成以下語言：

- `threadBridge core runtime`
- `transport adapter`
- `Telegram adapter`
- `custom app adapter`
- `runtime event`
- `thread handle`

應避免把產品整體繼續描述成：

- `Telegram bot runtime`
- `Telegram thread 是唯一 thread 模型`

## 對 custom app 的意義

如果抽象成功，custom app 應該可以只關心：

- 如何送入 `text / image / command / control action`
- 如何接收 `preview / final / error / state`
- 如何顯示 thread 狀態與 artifact

而不需要重做：

- Codex thread 控制
- workspace binding
- tool runtime 協調
- session broken 驗證

## 風險

- 如果抽象做得太早、太大，可能會把現有 Telegram 流程打散
- 如果協議語意沒有先固定，adapter 化只會變成把 Telegram 型別包一層皮
- 如果 core 仍偷偷依賴 Telegram 特性，第二個 adapter 很快就會暴露問題

## 開放問題

- core runtime 是否應該是一個 Rust crate 邊界，而不是只是一組模組邊界？
- adapter 應該用 trait、channel、還是 event bus 方式接 core？
- control action 是否也應該走同一套事件模型，而不是保留 slash-command 專用入口？
- custom app 的最小驗證載體應該是 CLI、HTTP/WebSocket demo，還是真正的 app？

## 建議的下一步

1. 先明確列出目前 `telegram_runtime/` 內哪些責任應搬到 core。
2. 定義一組最小 runtime event 與 input request 型別。
3. 先把 Telegram renderer / command router 收斂到 adapter 邊界。
4. 實作第二個最小 adapter 來驗證抽象，而不是只停留在理論上。
