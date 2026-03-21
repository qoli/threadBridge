# macOS 托盤 Thread 管理面草稿

## 目前進度

這份文檔不再是「完全未實作」的純構想，v1 的 desktop runtime 骨架已經開始落地，但仍未完整收尾。

目前代碼裡已經有的前置能力：

- Telegram thread / workspace binding / Codex thread lifecycle 已存在
- shared websocket app-server、受管 `hcodex`、TUI proxy、adoption 已部分落地
- `threadBridge` 現在可在缺少 Telegram 憑據時先啟動本地 management API，不再只能以 Telegram bot 形態啟動
- 已有本地 management API：
  - `GET /api/setup`
  - `PUT /api/setup/telegram`
  - `GET /api/runtime-health`
  - `GET /api/threads`
  - `GET /api/workspaces`
  - `GET /api/archived-threads`
  - `POST /api/threads`
  - `POST /api/threads/create-and-bind`
  - `POST /api/threads/:thread_key/bind-workspace`
  - `POST /api/workspaces/:thread_key/reconnect`
  - `POST /api/workspaces/:thread_key/open`
  - `POST /api/workspaces/:thread_key/launch-new`
  - `POST /api/workspaces/:thread_key/launch-resume`
  - `POST /api/threads/:thread_key/archive`
  - `POST /api/threads/:thread_key/restore`
  - `GET /api/events`
- runtime 已開始維護 workspace 維度的 recent Codex session history
- `/bind_workspace` 已開始拒絕同一 workspace 的第二個 active binding
- 已新增 `threadbridge_desktop`：
  - macOS-first `tray-icon` 常駐入口
  - top-level tray menu 會列出 managed workspace submenu
  - 每個 workspace submenu 會列出 `Start New hcodex Session` 與最近 5 個 session id
- `Settings` 會打開內嵌 webview 並載入本地 management UI
- managed Codex health 已開始暴露真實 source / binary path / version，且本地管理面可切換 Codex source preference 並同步已綁定 workspace 的 launcher
- desktop runtime owner 已開始在背景定期 reconcile 已管理 workspace，並主動 ensure shared app-server 與 TUI proxy；同時也提供單 workspace 的 `repair runtime` control action
- 本地管理面已開始提供 managed Codex cache refresh，能把目前 `PATH` 上的 `codex` 複製進 repo 管理快取
- 本地管理面已開始提供 managed Codex source build，可直接從本機 Codex Rust workspace 建出受管 binary 並寫入 build info
- 本地管理面已開始提供 `open workspace` control action
- 本地管理面已開始提供 adopt / reject pending TUI handoff control action
- Telegram bot 啟動已抽成可複用 runner，headless `threadbridge` 與 desktop runtime 共用同一套 bot/runtime 啟動邏輯
- setup 儲存後，desktop runtime 已會在背景重新嘗試拉起 Telegram polling，不再只剩重啟一條路

目前仍缺：

- desktop runtime owner 對 handoff continuity / adoption 狀態的 owner 收斂仍不完整
- managed Codex source build 目前仍是直接呼叫 cargo 的實作骨架，尚未收斂成更正式的 update/install UX
- web 管理面已拆出靜態 asset，但前端結構仍偏輕量，尚未收斂成更正式的模組化 UI

## 問題

`threadBridge` 目前的主要入口仍是 Telegram，這對對話本身合理，但對 thread 管理、workspace 掃描、本地接手與 machine-level runtime 管理都不夠順手。

目前摩擦點包括：

- thread list、binding 狀態、busy / broken、TUI session、workspace runtime 健康度分散在 Telegram 訊息、本地檔案與工作區狀態檔裡
- 本地維護者缺少一個集中管理入口來做 workspace -> thread 對照、runtime 健康檢查、archive / restore、reconnect
- 想從指定 workspace 本地接手工作時，雖然已有 `hcodex`，但還缺少一個 machine-level 的 launch surface 與 owner 視角
- handoff continuity 依賴 workspace `ws` runtime，但 bot 與 `hcodex` 目前都還帶有過渡性 owner 行為

## 定位

這份文檔定義的是本地 thread manager / runtime manager，不是新的聊天入口，也不是完整 observability 平台。

這裡說的「托盤」在產品形態上應理解為：

- `tray-icon` 提供極簡入口
- webview 承接完整管理面
- threadBridge 提供 local server 與 runtime owner

這份 plan 應處理：

- thread list 與 archived thread list
- thread 級 control action
- workspace 級 `hcodex` 啟動入口
- machine-level runtime / managed Codex health view
- workspace `ws` runtime owner 的責任邊界

這份 plan 不處理：

- 完整 turn timeline observability
- interactive terminal
- 跨平台最終 UI 產品策略
- Telegram renderer / delivery 細節

## v1 目標

第一版建議做成「極簡 tray 入口 + 完整 web 管理面」，而不是把所有管理能力塞進 tray menu。

### 1. Tray Menu

頂層 menu 固定只包含：

- 每個 managed workspace 一個 submenu
- `Settings`

每個 workspace submenu 固定包含：

- `Start New hcodex Session`
- 分隔線
- 最近 5 個 Codex `thread.id`

點 recent `thread.id` 時，語義應等價於：

- 對該 workspace 走受管 `hcodex resume <thread.id>`

### 2. Web 管理面

webview 是正式管理面，不是單純 token 設定頁。

首頁至少顯示：

- managed Codex binary path / version / ready
- app-server status
- TUI proxy status
- handoff readiness
- broken thread 數量
- running thread 數量
- Telegram polling 狀態

thread / workspace 管理頁至少顯示：

- title
- `thread_key`
- workspace label 或 path 摘要
- `binding_status`
- `run_status`
- `current_codex_thread_id`
- `tui_active_codex_thread_id`
- `tui_session_adoption_pending`
- archived 與否
- `last_used_at`

目前代碼已開始同時暴露 `GET /api/threads`、`GET /api/workspaces`、`GET /api/archived-threads`，但 web 管理面的視圖層仍偏簡單，還沒有收斂成更正式的前端模組。

### 3. Thread 快捷操作

web 管理面中的 v1 action 以既有 lifecycle/control 語義為主：

- create thread
- bind workspace
- open workspace
- adopt pending TUI handoff
- reject pending TUI handoff
- launch new `hcodex`
- reconnect Codex
- archive thread
- restore archived thread

若某個 action 有風險，例如 archive / restore，應有明確確認步驟。

`Open in Telegram` 不作為 v1 必選 action；只有在驗證出穩定 deep link 方案後才加入。

### 4. First-Run Onboarding

第一次使用時，desktop runtime 必須能在沒有 Telegram 憑據時先啟動，並由 web 管理面完成引導。

最低限度要覆蓋：

- `TELEGRAM_BOT_TOKEN`
- `AUTHORIZED_TELEGRAM_USER_IDS`
- 首個 workspace 建立 / 綁定
- runtime ready 檢查

## 建議的資料模型

這個管理面不應直接把 `data/*.json` 當成 UI 的穩定 API。

比較合理的方向是由 threadBridge runtime 提供本地 query / control surface，至少能回答：

- 列出 active threads
- 列出 archived threads
- 列出 managed workspaces
- 讀取 machine-level runtime health
- 送出 control action
- 訂閱狀態更新

對這個 surface 而言，最低限度的 view 包含：

- `SetupStateView`
- `ManagedCodexView`
- `RuntimeHealthView`
- `ManagedWorkspaceView`
- `ThreadStateView`
- `ArchivedThreadView`
- `RecentCodexSessionView`

`ThreadStateView` 至少應包含：

- `thread_key`
- `title`
- `workspace_cwd`
- `binding_status`
- `run_status`
- `current_codex_thread_id`
- `tui_active_codex_thread_id`
- `archived_at`
- `last_used_at`

`ManagedCodexView` 至少應包含：

- `binary_path`
- `binary_ready`
- `version` 或 `revision`
- `source`
- `handoff_supported`

這一層應盡量沿用 [runtime-protocol.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-protocol.md) 的命名，而不是在 UI 層另造狀態模型。

## 建議的架構方向

### 1. Rust runtime 繼續作為 source of truth

tray / webview 不應直接改寫 repository 檔案，也不應模擬發送 Telegram 命令來完成操作。

比較穩定的做法是：

- threadBridge runtime 暴露本地 management API
- tray / webview 只做 query、render、control action 送出

### 2. desktop runtime 作為正式的 workspace `ws` runtime owner

這份 plan 的核心不是 UI，而是 owner 邊界。

desktop runtime 應對下列責任負責：

- managed Codex binary 啟動與驗證
- `codex app-server`
- TUI proxy
- handoff continuity
- healthcheck / restart
- 對外發布穩定的 local status surface

只要 handoff continuity 依賴 workspace `ws` runtime，這個責任就不應再留給 Telegram bot 或 `hcodex` 的過渡性 self-heal。

### 3. tray 只是 `hcodex` 的 launch surface

托盤程式新增 workspace 啟動入口後，不能和現有 `./.threadbridge/bin/hcodex` 形成兩套互相競爭的本地入口。

比較合理的方向是：

- `hcodex` 保持 workspace 內的正式受管 CLI 入口
- tray 負責替使用者找到目標 workspace，並啟動等價的受管路徑
- web 管理面負責展示這個入口的可用性、最近 session 與 health 狀態
- `hcodex` self-heal 收斂成 fallback，而不是長期 owner 模型

### 4. managed Codex binary 模型

v1 對外不再以 `brew/source` 雙來源模型作為管理面概念。

這份 plan 改成：

- `hcodex` 由 threadBridge 管理的 Codex binary 啟動
- runtime health 以 threadBridge 管理的 binary 為準
- 更新 Codex binary 是正式 feature
- 不關心 brew 版本的 Codex
- 不依賴 shell `PATH` 猜測真正使用的 binary

### 5. Workspace 維度的 recent session history

tray menu 需要每個 workspace 最近 5 個 Codex `thread.id`。

這份歷史應由 runtime 正式維護，而不是由 UI 掃描本地檔案臨時拼裝。

更新來源至少包括：

- Telegram 正常 turn
- `/new`
- `/reconnect_codex`
- 受管 TUI `thread/start`
- 受管 TUI `thread/resume`
- adoption 成功後切換

規則應固定為：

- 以 workspace 為 key
- 去重
- 最近使用移到最前
- 上限 5 筆

### 6. 一個 workspace 只允許一個 active binding

v1 明確限制：

- 同一 workspace 只允許一個 active Telegram/threadBridge binding

若現有資料中偵測到多個 active binding：

- 在 web 管理面標成 conflict
- 從 tray 隱藏該 workspace 的 launch 項
- 必須先解決衝突再恢復本地 launch

## 與既有能力的對應

這份 plan 的 value 不在於發明新的 thread 動作，而是在於：

- 把既有 lifecycle/control 能力集中成一個本地管理面
- 補上 machine-level runtime owner 與 managed Codex 視角
- 補上正式的 workspace `hcodex` launch surface

目前已存在、可被管理面承接的能力包括：

- `./.threadbridge/bin/hcodex`
- `/bind_workspace`
- `/new`
- `/reconnect_codex`
- `/archive_thread`
- `/restore_thread`

## 與其他計劃的關係

- [session-lifecycle.md](/Volumes/Data/Github/threadBridge/docs/plan/session-lifecycle.md)
  - 定義 thread / workspace / Codex thread 的控制語義
- [runtime-state-machine.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-state-machine.md)
  - 應提供這個管理面要顯示的 canonical 狀態軸
- [runtime-protocol.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-protocol.md)
  - 應提供本地 query / control surface 的 view / action 命名
- [session-level-cli-telegram-sync.md](/Volumes/Data/Github/threadBridge/docs/plan/session-level-cli-telegram-sync.md)
  - 定義 shared daemon、受管 `hcodex`、TUI proxy、adoption 與 owner 現況
- [telegram-webapp-observability.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-webapp-observability.md)
  - observability 可共用同一份 thread state / event model，但不是這份文檔的主責

## 開放問題

- `Open in Telegram` 是否有穩定可用的 deep link 方案？
- desktop runtime 是否最終要拆成 tray 進程 + helper 進程？
- managed Codex binary 的 update UX 要做成自動、手動，還是兩者並存？
- web 管理面與未來 observability 面是否共用同一個 web shell？

## 建議的下一步

1. 先把 [runtime-protocol.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-protocol.md) 補成可支撐這份管理面的最小 view / action 草稿。
2. 先把 [session-level-cli-telegram-sync.md](/Volumes/Data/Github/threadBridge/docs/plan/session-level-cli-telegram-sync.md) 補上 desktop runtime owner 的最新責任邊界。
3. 先在 runtime 裡補齊 local query / control API 與 managed Codex update 能力。
4. 在 API 穩定後，再落 tray-icon UI 與 webview shell。
