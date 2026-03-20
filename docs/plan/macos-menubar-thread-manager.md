# macOS 托盤 Thread 管理面草稿

## 目前進度

這份文檔目前仍是純草稿，尚未開始實作 macOS 托盤程式。

目前代碼中已經有的前置能力：

- Telegram thread / workspace binding / Codex thread lifecycle 已經存在
- `/bind_workspace`、`/new`、`/reconnect_codex`、`/archive_thread`、`/restore_thread` 已有既定語義
- bot-local repository 已經保存 thread metadata、`session-binding.json`、archive 狀態
- topic title、busy gate、session continuity 已經開始形成可供 UI 消費的狀態訊號

目前仍缺：

- 本地桌面管理程式
- 給本地管理面使用的 query / control API
- thread list 與 thread state 的正式 view model
- 非 Telegram surface 的即時狀態推送機制

## 問題

`threadBridge` 目前的主入口是 Telegram，這對對話本身是合理的，但對「管理 thread」不夠順手。

目前常見摩擦點包括：

- 跨多個 thread 看狀態時，主要只能回到 Telegram topic 列表與標題 suffix 猜測
- `/bind_workspace`、`/new`、`/reconnect_codex`、`/archive_thread`、`/restore_thread` 都是命令驅動，缺少一個本地集中管理面
- archived thread 的 restore 雖然已存在，但目前是 Telegram 私聊中的互動流程，不是本地常駐入口
- workspace binding、broken、busy、current/TUI session 等訊號分散在 title、回覆訊息與本地檔案裡

對維護者來說，一個常駐在 macOS menu bar 的輕量入口，會比切回 Telegram 更適合做 thread 級管理。

## 定位

這份文檔定義的是本地 macOS 管理 surface，不是新的聊天入口，也不是完整 observability 平台。

這裡說的「托盤」在 macOS 上實際比較接近：

- menu bar extra
- status item

這份 plan 應處理：

- thread list 與 thread 摘要狀態
- thread 級 control action
- 本地快捷入口，例如開啟 Telegram thread 或 workspace

這份 plan 不處理：

- canonical runtime state naming
- Telegram renderer / delivery 行為
- 完整 turn timeline observability
- 跨平台桌面 UI 策略

## v1 目標

第一版建議先做成「輕量管理面」，不要一開始就承擔完整 debug console。

### 1. Menu Bar 總覽

至少應該能快速看見：

- 是否有 `broken` thread
- 是否有 `running` / `busy` thread
- 最近使用的 thread

menu bar icon 或選單標題可以考慮承載非常少量的全域訊號，例如：

- `broken` 數量
- `running` 數量

### 2. Thread 清單

每個 thread 至少顯示：

- title
- `thread_key`
- workspace label 或 path 摘要
- binding 狀態
- run 狀態
- session ownership 狀態
- archived 與否
- 最後使用時間

### 3. Thread 快捷操作

每個 thread 的 v1 操作建議以既有 runtime control action 為主：

- Open in Telegram
- Open workspace
- Bind workspace
- New Codex session
- Reconnect Codex
- Archive thread
- Restore archived thread

如果某個操作風險較高，例如 reset / archive，應有明確確認步驟。

## 建議的資料模型

這個管理面不應直接把 `data/*.json` 當成 UI 的穩定 API。

比較合理的方向是由 threadBridge runtime 提供本地 query / control surface，至少能回答：

- 列出 active threads
- 列出 archived threads
- 讀取單一 thread 的 `ThreadStateView`
- 送出 control action
- 訂閱 thread 狀態更新

對這個 surface 而言，最低限度的 thread view 應包含：

- `thread_key`
- `title`
- `workspace_cwd`
- `binding_status`
- `run_status`
- `current_codex_thread_id`
- `tui_active_codex_thread_id`
- `archived_at`
- `last_used_at`

這一層應該盡量沿用 [runtime-protocol.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-protocol.md) 的命名，而不是再在 macOS UI 內自創狀態模型。

## 建議的架構方向

### 1. Rust runtime 繼續作為 source of truth

托盤程式不應直接改寫 repository 檔案，也不應模擬發送 Telegram 命令來完成控制操作。

比較穩定的做法是：

- threadBridge runtime 暴露本地管理 API
- macOS app 只做 query、render、control action 送出

### 2. macOS app 作為獨立 companion app

第一版較適合做成獨立小型 app，而不是把 AppKit / SwiftUI 直接嵌進主 Rust bot 進程。

這樣可以保留比較乾淨的分工：

- Rust
  - runtime
  - repository
  - control actions
  - local API
- macOS app
  - menu bar UI
  - native folder picker
  - 本地通知

### 3. UI 技術方向

如果未來真的落地 macOS 原生 UI，v1 比較自然的方向是：

- SwiftUI `MenuBarExtra`
- 必要時補 AppKit `NSStatusItem`

這比用跨平台桌面殼更符合這份 plan 的「本地維護者工具」定位。

## 與既有能力的對應

這份 plan 的 value 不在於新增全新的 thread 操作，而是在於把既有能力集中到本地可掃描的管理面。

目前已存在、可被托盤管理面承接的能力包括：

- `/bind_workspace`
- `/new`
- `/reconnect_codex`
- `/archive_thread`
- `/restore_thread`

所以 v1 的實際目標應理解成：

- 先把既有操作做成本地管理入口
- 再決定是否要加入托盤專屬的新能力

## 與其他計劃的關係

- [session-lifecycle.md](/Volumes/Data/Github/threadBridge/docs/plan/session-lifecycle.md)
  - 定義 thread / workspace / Codex thread 的控制語義
- [runtime-state-machine.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-state-machine.md)
  - 應提供這個管理面要顯示的 canonical 狀態軸
- [runtime-protocol.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-protocol.md)
  - 應提供本地 query / control surface 的基礎命名
- [telegram-adapter-migration.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-adapter-migration.md)
  - 這個托盤程式可以視為「第二個管理型 adapter / client surface」的候選驗證面
- [telegram-webapp-observability.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-webapp-observability.md)
  - 兩者可以共用同一份 thread state / event model，但這份文檔偏管理與控制，不是完整 observability

## 開放問題

- v1 應該先做純 read-only，還是直接帶 control actions？
- `Bind workspace` 應該走原生 folder picker，還是只先顯示最近路徑並讓使用者貼上？
- archived threads 應該直接出現在 menu 裡，還是需要獨立視窗？
- `Open in Telegram` 是否有穩定可用的 deep link 形式，還是只能先提供 thread metadata / copy action？
- 如果 threadBridge bot 沒在跑，托盤程式要負責啟動它，還是只顯示 disconnected？
- local 管理面是否需要任何額外權限確認，還是預設信任本機登入使用者？

## 建議的下一步

1. 先把最小 `ThreadStateView` 與 control action 命名收斂到 [runtime-protocol.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-protocol.md)。
2. 在 Rust runtime 補一組最小本地查詢能力，至少能列出 threads 與 archived threads。
3. 先做 read-only 的 macOS menu bar prototype，驗證 thread list、狀態摘要、open workspace 這些核心流程。
4. 確認本地 API 與 UX 成形後，再加上 bind / reconnect / archive / restore 類控制操作。
