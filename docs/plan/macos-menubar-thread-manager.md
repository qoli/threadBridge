# macOS 托盤 Thread 管理面草稿

## 目前進度

這份文檔目前仍是純草稿，尚未開始實作 macOS 托盤程式。

目前代碼中已經有的前置能力：

- Telegram thread / workspace binding / Codex thread lifecycle 已經存在
- `/bind_workspace`、`/new`、`/reconnect_codex`、`/archive_thread`、`/restore_thread` 已有既定語義
- bot-local repository 已經保存 thread metadata、`session-binding.json`、archive 狀態
- topic title、busy gate、session continuity 已經開始形成可供 UI 消費的狀態訊號
- 本地 `hcodex` 已具備 shared runtime self-heal，能在 `current.json` stale 時自行拉起 shared daemon / proxy
- workspace runtime 安裝流程目前仍有 `brew` / `source` 兩種 `codex` 來源偏好，`hcodex` launcher 會依 repo 狀態決定實際用哪個 binary
- handoff 目前仍依賴定製版 `codex`，不是已可穩定依賴的官方多客戶端 `codex_tui_app_server`

目前仍缺：

- 本地桌面管理程式
- 給本地管理面使用的 query / control API
- thread list 與 thread state 的正式 view model
- 非 Telegram surface 的即時狀態推送機制
- shared daemon / TUI proxy 的正式長壽命 owner
- 當前 `brew/source` 雙來源模型下的 `codex` 受管定位、驗證、啟動與更新語義
- 從指定 workspace 快速啟動受管 `codex` 的正式入口

## 問題

`threadBridge` 目前的主入口是 Telegram，這對對話本身是合理的，但對「管理 thread」與「從本地接手工作」都不夠順手。

目前常見摩擦點包括：

- 跨多個 thread 看狀態時，主要只能回到 Telegram topic 列表與標題 suffix 猜測
- `/bind_workspace`、`/new`、`/reconnect_codex`、`/archive_thread`、`/restore_thread` 都是命令驅動，缺少一個本地集中管理面
- archived thread 的 restore 雖然已存在，但目前是 Telegram 私聊中的互動流程，不是本地常駐入口
- workspace binding、broken、busy、current/TUI session 等訊號分散在 title、回覆訊息與本地檔案裡
- 想從某個 workspace 本地接手工作時，仍缺少一個穩定、可掃描的「在這裡啟動 Codex」入口
- handoff 依賴特定能力的 `codex` runtime，但目前代碼仍保留 `brew/source` 雙路徑，缺少 machine-level 的受管 owner 與健康狀態呈現

對維護者來說，一個常駐在 macOS menu bar 的輕量入口，會比切回 Telegram 更適合做 thread 級管理，也更適合做 workspace 級的本地 `codex` 啟動。

## 定位

這份文檔定義的是本地 macOS 管理 surface、當前 `codex` runtime host、workspace `ws` runtime owner，以及 workspace 級 `codex` 快捷啟動入口，不是新的聊天入口，也不是完整 observability 平台。

這裡說的「托盤」在 macOS 上實際比較接近：

- menu bar extra
- status item

這份 plan 應處理：

- thread list 與 thread 摘要狀態
- thread 級 control action
- 本地快捷入口，例如開啟 Telegram thread、workspace，或直接在指定 workspace 啟動 `codex`
- shared app-server runtime 與 handoff 依賴 `ws` 服務器的長壽命 ownership / health management
- machine-level `codex` 來源、能力、定位、驗證與啟動

這份 plan 不處理：

- canonical runtime state naming
- Telegram renderer / delivery 行為
- 完整 turn timeline observability
- 跨平台桌面 UI 策略

## v1 目標

第一版建議先做成「輕量管理面 + 正式本地啟動入口」，不要一開始就承擔完整 debug console。

### 1. Menu Bar 總覽

至少應該能快速看見：

- 當前受管 `codex` runtime 是否可用
- shared runtime 是否健康
- 是否有 `broken` thread
- 是否有 `running` / `busy` thread
- 最近使用的 thread

menu bar icon 或選單標題可以考慮承載非常少量的全域訊號，例如：

- `broken` 數量
- `running` 數量
- runtime unavailable 指示

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
- Launch Codex in workspace
- Bind workspace
- New Codex session
- Reconnect Codex
- Archive thread
- Restore archived thread

如果某個操作風險較高，例如 reset / archive，應有明確確認步驟。

### 4. Workspace 啟動入口

這個托盤程式不只是一個 thread 清單，也應該是 workspace 級的本地 `codex` 快捷入口。

最低限度應支持：

- 從 thread 對應的 workspace 啟動受管 `codex`
- 從最近使用的 workspace 啟動受管 `codex`
- 從原生 folder picker 指定任意 workspace 後啟動受管 `codex`

這裡的「啟動 `codex`」不是單純 `open -a Terminal` 後讓使用者自己輸入命令，而是由托盤程式根據當前 runtime 設定選定對應的 `codex` binary，在目標 workspace 中啟動正式本地入口。

若該 workspace 已有共享 runtime 與既有 binding，這個入口應優先走與 `hcodex` 一致的受管路徑，而不是繞過 threadBridge runtime 自己直接亂啟一個未受管 TUI。

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

除了 thread view 以外，這份 plan 也需要一個 machine-level runtime view，至少回答：

- `managed_codex_binary_path`
- `managed_codex_binary_version`
- `managed_codex_source`
- `managed_codex_binary_ready`
- `handoff_supported`
- `app_server_status`
- `tui_proxy_status`

這一層是托盤總覽與 workspace 啟動入口的前置狀態，不應硬塞回單一 thread view。

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

### 3. 當前 `codex` 來源模型應由托盤程式受管

目前代碼現況不是「已經只剩單一固定 custom `codex`」，而是：

- workspace runtime 安裝流程仍會讀取 repo 內的 `codex` 來源偏好
- 現有 launcher 仍支持 `brew` / `source` 兩條路徑
- handoff 依賴特定能力的 `codex` runtime
- 是否以及何時有官方可替代的多客戶端 `codex_tui_app_server` 仍不可知

因此托盤程式不應：

- 依賴使用者 shell `PATH` 猜測 `codex`
- 假設任意系統上的 `codex` 都能支援 handoff
- 在未讀取當前 runtime 設定前，就自行假定只有單一 binary 模型

比較穩定的做法是由托盤程式或其背後的本地 runtime owner 明確管理：

- 當前實際使用的 `codex` source
- 對應 binary 的路徑
- binary 版本 / revision 顯示
- 啟動前能力驗證
- binary 缺失或不相容時的錯誤狀態

若未來代碼真的收斂成單一固定 custom binary，再把這層簡化成單一 binary owner 會比較合理；目前文檔應先忠於現有代碼模型。

### 4. macOS 常駐進程應作為正式的 workspace `ws` runtime owner

這是這份 plan 現在新增的重要責任邊界。

目前實測已知：

- Telegram bot 發 turn 時，已經是連共享 websocket app-server，不再是 per-turn `stdio://`
- `/reconnect_codex` 會重寫 `./.threadbridge/state/app-server/current.json`
- 但 bot 目前仍可能留下 dead ws endpoint
- `hcodex` 已經用 self-heal 補上本地入口可用性

這表示真正缺的不是「再補一個 ws bridge」，而是 handoff 所依賴的 workspace `ws` runtime 正式 owner。

對 macOS 本機環境來說，menu bar 常駐進程應成為這個正式 owner。它應負責：

- 持有當前選定 `codex` runtime 的啟動責任
- 持有 `codex app-server`
- 持有 TUI proxy
- 持有 handoff 所依賴的 `ws` runtime continuity
- 做 healthcheck / restart
- 對外發布穩定的 `current.json` / 本地狀態面
- 在 bot 或 `hcodex` 需要時提供既存 runtime，而不是讓每個 client 自己補救
- 從指定 workspace 啟動受管 `codex`

這裡的關鍵不是「誰有能力在當下拉起 ws」，而是「誰對 ws 的持續可用性負責」。

只要 handoff continuity 依賴這個 `ws` runtime，這個責任就不應再留給 Telegram bot 或 `hcodex` 的臨時 self-heal，而應由 menubar app 集中持有。

### 5. 與 `hcodex` 的關係

托盤程式新增 workspace 啟動入口後，不能和現有 `./.threadbridge/bin/hcodex` 形成兩套互相競爭的本地入口。

比較合理的方向是：

- `hcodex` 保持 workspace 內的正式受管 CLI 入口
- 托盤程式負責替使用者找到目標 workspace，並啟動等價的受管路徑
- 如果該 workspace 已經綁定且共享 runtime 可用，托盤程式應盡量複用既有 runtime
- `hcodex` 的 self-heal 應收斂成 owner 尚未完全切換前的 fallback，而不是長期 owner 模型
- 如果 runtime state stale，托盤程式應做正式 ensure；`hcodex` 只保留最小 fallback，不應再作為常態 owner 補位

這樣托盤程式才是 `hcodex` 的 launch surface，而不是另一個語義分叉的 launcher。

### 6. UI 技術方向

如果未來真的落地 macOS 原生 UI，v1 比較自然的方向是：

- SwiftUI `MenuBarExtra`
- 必要時補 AppKit `NSStatusItem`

這比用跨平台桌面殼更符合這份 plan 的「本地維護者工具」定位。

## 與既有能力的對應

這份 plan 的 value 不在於新增全新的 thread 操作，而是在於把既有能力集中到本地可掃描的管理面，並補上一個正式的 workspace `codex` 啟動入口。

目前已存在、可被托盤管理面承接的能力包括：

- `./.threadbridge/bin/hcodex`
- `/bind_workspace`
- `/new`
- `/reconnect_codex`
- `/archive_thread`
- `/restore_thread`

所以 v1 的實際目標應理解成：

- 先把既有操作做成本地管理入口
- 先把當前 `codex` runtime owner 做穩
- 先把 workspace 的 `codex` 啟動入口做成正式受管路徑
- 再決定是否要加入托盤專屬的新能力

## 與其他計劃的關係

- [session-lifecycle.md](/Volumes/Data/Github/threadBridge/docs/plan/session-lifecycle.md)
  - 定義 thread / workspace / Codex thread 的控制語義
- [runtime-state-machine.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-state-machine.md)
  - 應提供這個管理面要顯示的 canonical 狀態軸
- [runtime-protocol.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-protocol.md)
  - 應提供本地 query / control surface 的基礎命名
- [session-level-cli-telegram-sync.md](/Volumes/Data/Github/threadBridge/docs/plan/session-level-cli-telegram-sync.md)
  - 定義受管 `hcodex`、shared daemon、TUI proxy、adoption 與本地入口的現行模型
- [telegram-adapter-migration.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-adapter-migration.md)
  - 這個托盤程式可以視為「第二個管理型 adapter / client surface」的候選驗證面
- [telegram-webapp-observability.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-webapp-observability.md)
  - 兩者可以共用同一份 thread state / event model，但這份文檔偏管理與控制，不是完整 observability

## 開放問題

- v1 是否至少要包含 `Launch Codex in workspace`，再把 destructive control action 延後？
- `Bind workspace` 與 `Launch Codex in workspace` 是否都應走原生 folder picker？
- archived threads 應該直接出現在 menu 裡，還是需要獨立視窗？
- `Open in Telegram` 是否有穩定可用的 deep link 形式，還是只能先提供 thread metadata / copy action？
- 如果 threadBridge bot 沒在跑，托盤程式要負責啟動它，還是只顯示 disconnected？
- ws runtime owner 是否直接內嵌在 menubar app 進程內，還是由 menubar 監管另一個本地常駐 helper？
- 現有 `brew/source` 雙來源模型的安裝、更新與 rollback 語義要交給誰負責？
- 未來是否真的要收斂成單一固定 custom `codex` binary？
- 托盤程式是否需要顯式顯示「目前使用的 custom `codex` revision / build id」？
- `hcodex` 的 self-heal 應保留到什麼程度，何時收斂成純 fallback？
- local 管理面是否需要任何額外權限確認，還是預設信任本機登入使用者？

## 建議的下一步

1. 先把當前 `codex` 來源模型、workspace `ws` runtime owner、以及 handoff continuity 的責任邊界寫進主規格，明確區分 bot client、`hcodex` fallback、menubar owner。
2. 先把最小 `ThreadStateView`、machine-level runtime health view，以及 `Launch Codex in workspace` 這類 action 命名收斂到 [runtime-protocol.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-protocol.md)。
3. 在 Rust runtime 補一組最小本地查詢能力，至少能列出 threads、archived threads、managed binary 狀態與 runtime health。
4. 先做能從指定 workspace 啟動受管 custom `codex` 的 macOS menu bar prototype，驗證 folder picker、workspace launch、runtime ensure、health summary 這些核心流程。
5. 確認本地 API 與 UX 成形後，再加上 bind / reconnect / archive / restore 類控制操作。
