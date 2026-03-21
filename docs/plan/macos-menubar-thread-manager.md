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
  - `POST /api/workspaces/pick-and-add`
  - `POST /api/runtime-owner/reconcile`
  - `POST /api/threads/:thread_key/adopt-tui`
  - `POST /api/threads/:thread_key/reject-tui`
  - `GET /api/workspaces/:thread_key/launch-config`
  - `POST /api/workspaces/:thread_key/reconnect`
  - `POST /api/workspaces/:thread_key/open`
  - `POST /api/workspaces/:thread_key/repair-runtime`
  - `POST /api/workspaces/:thread_key/launch-new`
  - `POST /api/workspaces/:thread_key/launch-resume`
  - `POST /api/threads/:thread_key/archive`
  - `POST /api/threads/:thread_key/restore`
  - `POST /api/managed-codex/preference`
  - `POST /api/managed-codex/refresh-cache`
  - `POST /api/managed-codex/build-source`
  - `POST /api/managed-codex/build-defaults`
  - `GET /api/events`
- runtime 已開始維護 workspace 維度的 recent Codex session history
- workspace-first create-bind flow 已開始拒絕同一 workspace 的第二個 active binding
- 已新增 `threadbridge_desktop`：
  - macOS-first `tray-icon` 常駐入口
  - top-level tray menu 會列出 managed workspace submenu
  - 每個 workspace submenu 會列出 `Start New hcodex Session` 與最近 5 個 session id
- `Settings` 會在預設瀏覽器中打開本地 management UI
- managed Codex health 已開始暴露真實 source / binary path / version，且本地管理面可切換 Codex source preference 並同步已綁定 workspace 的 launcher
- desktop runtime owner 已開始在背景定期 reconcile 已管理 workspace，並主動 ensure shared app-server 與 TUI proxy；同時也提供單 workspace 的 `repair runtime` control action
- 本地管理面已開始提供 machine-level 的 runtime owner reconcile action，可一次對所有非 conflict workspace 做全域 repair / ensure
- 本地管理面已開始提供 managed Codex cache refresh，能把目前 `PATH` 上的 `codex` 複製進 repo 管理快取
- 本地管理面已開始提供 managed Codex source build，可直接從本機 Codex Rust workspace 建出受管 binary 並寫入 build info
- managed Codex source build 已開始把 default source repo / source rs dir / build profile 暴露進 management view，且本地 UI 可在每次 build 時顯式覆蓋
- managed Codex source build defaults 已開始持久化到 repo-local config，而不是只依賴 shell env 或單次 request
- 本地管理面已開始提供 `open workspace` control action
- 本地管理面已開始提供 adopt / reject pending TUI handoff control action
- Telegram bot 啟動已抽成可複用 runner，headless `threadbridge` 與 desktop runtime 共用同一套 bot/runtime 啟動邏輯
- setup 儲存後，desktop runtime 已會在背景重新嘗試拉起 Telegram polling，不再只剩重啟一條路

目前仍缺：

- desktop runtime owner 對 handoff continuity / adoption 狀態的 owner 收斂仍不完整
- managed Codex source build 目前仍是直接呼叫 cargo 的實作骨架，尚未收斂成更正式的 update/install UX
- web 管理面已拆成靜態 HTML/CSS/JS asset，但前端結構仍偏輕量，尚未收斂成更正式的模組化 UI

目前新增確認的優先級判斷是：

- owner 責任收斂應視為這條 plan 的高優先級工作

因為本地管理面真正的價值，不只是多一個 UI，而是讓 desktop runtime 成為可信的本地 runtime owner。

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
- 瀏覽器管理頁承接完整管理面
- threadBridge 提供 local server 與 runtime owner

這份 plan 應處理：

- workspace-first 管理面與 archived workspace 歷史列表
- thread 級 control action 的 runtime / debug 邊界
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

瀏覽器管理面是正式管理面，不是單純 token 設定頁。

v1 的主模型固定為：

- `workspace = thread`
- `Workspaces` 是唯一主實體列表
- `Archived Workspaces` 是唯一歷史列表
- `ThreadStateView` 保留給 runtime / debug 視角，不再作為普通用戶主區塊

首頁至少顯示：

- managed Codex binary path / version / ready
- app-server status
- TUI proxy status
- handoff readiness
  - `ready`: app-server 與 TUI proxy 都可用，且沒有 pending adoption
  - `pending_adoption`: 底層 runtime 可用，但目前有待 adopt/reject 的 TUI handoff
  - `degraded`: 只有部分 runtime surface 可用
  - `unavailable`: handoff 目前不可用
- desktop runtime owner state
  - last reconcile started / finished / successful timestamps
  - last reconcile error
  - last reconcile report
- broken thread 數量
- running thread 數量
- Telegram polling 狀態

workspace 管理頁至少顯示：

- workspace label 或 path 摘要
- `binding_status`
- `run_status`
- recent session
- `last_used_at`
- continuity / runtime recovery hint

技術欄位例如 `thread_key`、`current_codex_thread_id`、`tui_active_codex_thread_id` 可以存在，但應降到 advanced / debug 區，而不是主卡片第一層。

目前代碼已開始同時暴露 `GET /api/threads`、`GET /api/workspaces`、`GET /api/archived-threads`，但主 UI 應以 `GET /api/workspaces` 與 `GET /api/archived-threads` 為準；`GET /api/threads` 不再作為普通用戶的一級列表。

### 3. Workspace 快捷操作

web 管理面中的 v1 action 以既有 lifecycle/control 語義為主：

- add workspace
- open workspace
- reconcile runtime owner
- repair continuity
- launch new `hcodex`
- archive workspace
- restore archived workspace

其中：

- `Add Workspace` 應走 native folder picker，而不是要求普通用戶輸入絕對路徑
- `repair continuity` 應按狀態自動選擇 `Adopt TUI` 或 `Repair Session`
- raw `bind workspace` / `reject TUI` 可以保留，但只應存在於 advanced / debug 區

若某個 action 有風險，例如 archive / restore，應有明確確認步驟。

目前 web 管理面已開始對 archive / restore 加上顯式確認，且 conflict workspace 的 launch / resume 入口會禁用，而不是繼續允許誤操作。

`Open in Telegram` 不作為 v1 必選 action；只有在驗證出穩定 deep link 方案後才加入。

### 4. First-Run Onboarding

暫不提供。

目前 desktop runtime 雖然仍可在沒有 Telegram 憑據時先啟動，但在真正可用的一次使用引導完成前，web 管理面不應再暴露半成品 onboarding 區塊。設定與 workspace 管理先直接放在正式頁面中處理。

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

但在產品層定位上，`ThreadStateView` 是 runtime / debug 視角，不是普通用戶主列表。

`ManagedCodexView` 至少應包含：

- `binary_path`
- `binary_ready`
- `version` 或 `revision`
- `source`

`ManagedWorkspaceView` 應同時暴露 workspace runtime heartbeat 的來源與最近一次檢查結果，至少包含：

- `app_server_status`
- `tui_proxy_status`
- `handoff_readiness`
- `runtime_health_source`
  - `owner_heartbeat`
  - `workspace_state`
- `heartbeat_last_checked_at`
- `heartbeat_last_error`
- `session_broken_reason`

但這裡要明確區分：

- owner heartbeat
  - 回答 runtime health
- workspace shared status / state file
  - 回答 session / CLI activity 或 artifact observation

現在管理面同時暴露 `runtime_health_source`，其實反映的是目前仍處於過渡性多來源模型，而不是已經有完全收斂的單一 heartbeat authority。

`SetupStateView` 還應暴露 desktop 能力位，至少包含：

- `native_workspace_picker_available`

`RuntimeHealthView` 除了 machine-level aggregate status，也應暴露：

- `ready_workspaces`
- `degraded_workspaces`
- `unavailable_workspaces`
- `build_info_file_path`
- `build_info`

這一層應盡量沿用 [runtime-protocol.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-protocol.md) 的命名，而不是在 UI 層另造狀態模型。

## 建議的架構方向

### 1. Rust runtime 繼續作為 source of truth

tray / 瀏覽器管理頁不應直接改寫 repository 檔案，也不應模擬發送 Telegram 命令來完成操作。

比較穩定的做法是：

- threadBridge runtime 暴露本地 management API
- tray / 瀏覽器管理頁只做 query、render、control action 送出

### 2. desktop runtime 作為正式的 workspace `ws` runtime owner

這份 plan 的核心不是 UI，而是 owner 邊界。

這也意味著，本地管理面最終不應只是「顯示目前有哪些 heartbeat source」，而應逐步收斂到：

- desktop runtime owner 提供 canonical runtime health heartbeat
- workspace state files 只作為 session / activity / fallback observation

如果這個邊界不先收斂，管理面的 health view 看起來會完整，但底層 authority 仍然是拼接的。

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
- `/new_session`
- `/repair_session`
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
- `/add_workspace`
- `/new_session`
- `/repair_session`
- `/archive_workspace`
- `/restore_workspace`

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
4. 在 API 穩定後，再持續收斂 tray-icon UI 與 workspace-first 瀏覽器管理頁。
