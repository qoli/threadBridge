# macOS 托盤 Thread 管理面草稿

## 目前進度

這份文檔不再是「完全未實作」的純構想，v1 的 desktop runtime 骨架已經開始落地，但 broader management UX 仍未完整收尾。

目前代碼裡已經有的前置能力：

- Telegram thread / workspace binding / Codex thread lifecycle 已存在
- shared websocket app-server、受管 `hcodex`、hcodex ingress、adoption 已部分落地
- `threadBridge` 現在可在缺少 Telegram 憑據時先啟動本地 management API，不再只能以 Telegram bot 形態啟動
- 已有本地 management API：
  - `GET /api/setup`
  - `PUT /api/setup/telegram`
  - `GET /api/runtime-health`
  - `GET /api/threads`
  - `GET /api/threads/:thread_key/transcript`
  - `GET /api/threads/:thread_key/sessions`
  - `GET /api/threads/:thread_key/sessions/:session_id/records`
  - `GET /api/workspaces`
  - `GET /api/archived-threads`
  - `POST /api/workspaces/pick-and-add`
  - `POST /api/runtime-owner/reconcile`
  - `POST /api/threads/:thread_key/adopt-tui`
  - `POST /api/threads/:thread_key/reject-tui`
  - `POST /api/threads/:thread_key/actions`
  - `GET /api/workspaces/:thread_key/launch-config`
  - `POST /api/workspaces/:thread_key/open`
  - `POST /api/workspaces/:thread_key/repair-runtime`
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
  - 每個 workspace submenu 已收斂成 `New Session` 與 `Continue Telegram Session`
- `threadbridge_desktop` 現在會以 menubar-only 形態啟動：
  - bundle `Info.plist` 已固定注入 `LSUIElement = true`
  - runtime event loop 已固定使用 macOS `Accessory` activation policy，而不是一般前台 Dock app
- `Settings` 會在預設瀏覽器中打開本地 management UI
- tray menu 已新增 runtime support 維護入口，用於在 bundled app 中清空 installed `runtime_support/` 並從 bundle seed runtime support 重建
- managed Codex health 已開始暴露真實 source / binary path / version，且本地管理面可切換 Codex source preference 並同步已綁定 workspace 的 launcher
- desktop runtime owner 已開始在背景定期 reconcile 已管理 workspace，並主動 ensure shared app-server 與 hcodex ingress；同時也提供單 workspace 的 `repair runtime` control action
- 本地管理面已開始提供 machine-level 的 runtime owner reconcile action，可一次對所有非 conflict workspace 做全域 repair / ensure
- 本地管理面已開始提供 managed Codex cache refresh，能把目前 `PATH` 上的 `codex` 複製進 repo 管理快取
- 本地管理面已開始提供 managed Codex source build，可直接從本機 Codex Rust workspace 建出受管 binary 並寫入 build info
- managed Codex source build 已開始把 default source repo / source rs dir / build profile 暴露進 management view，且本地 UI 可在每次 build 時顯式覆蓋
- managed Codex source build defaults 已開始持久化到 repo-local config，而不是只依賴 shell env 或單次 request
- 本地管理面已開始提供 `open workspace` control action
- 本地管理面已開始提供 adopt / reject pending TUI handoff control action
- 本地管理面已開始提供 transcript observability pane，可查看 final/process transcript
- 本地管理面已開始提供 workspace-card `Sessions` pane，可查看 session summary 與 inline records timeline
- workspace card 內的 `Sessions` / `Transcript` / `Launch Output` / `Advanced Workspace Details` 現在會在 refresh 後保留展開狀態
- Telegram bot 啟動已抽成可複用 runner，並由 desktop runtime 單一路徑持有
- setup 儲存後，desktop runtime 已會在背景重新嘗試拉起 Telegram polling，不再只剩重啟一條路

目前仍缺：

- managed Codex source build 目前仍是直接呼叫 cargo 的實作骨架，尚未收斂成更正式的 update/install UX
- web 管理面已拆成靜態 HTML/CSS/JS asset，但前端結構仍偏輕量，尚未收斂成更正式的模組化 UI

目前新增確認的一個 UI 收斂方向是：

- web 管理面已確認改走本地 vendored、無 React/Node build 的 Tabler 風格 CSS 重構路線
- dark mode 先固定為跟隨系統，不新增手動 theme toggle 或前端 theme state
- tray menu 已收斂；workspace submenu 在 v1 只保留 `New Session` 與 `Continue Telegram Session` 兩個入口，不再承擔 recent session browser 或其他 control action
- tray 的 maintenance action 只額外保留 `Rebuild Runtime Support`；它不應觸碰 `data/`、setup config 或 thread state
- macOS app 產品形態已開始收斂成 menubar-only 常駐工具；正常運行時預設隱藏 Dock 圖標，不把 Dock 當成主要入口

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
- 正常運行時以 macOS menubar app 形態存在，而不是常駐一個 Dock app

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

- `New Session`
- `Continue Telegram Session`

`New Session` 的語義應等價於：

- 對該 workspace 走受管的新 `hcodex` session 啟動

`Continue Telegram Session` 的語義應等價於：

- 對該 workspace 恢復目前 Telegram thread 綁定的 `current_codex_thread_id`

v1 的 tray menu 明確不再提供：

- recent session list / arbitrary session picker
- archive / restore / reconnect / repair runtime
- runtime/debug 類 control action

### 1.1 Dock Presence

v1 另固定一個產品形態約束：

- `threadBridge` 在正常背景運行時應隱藏 macOS Dock 圖標
- 主入口是 menubar tray icon 與瀏覽器管理頁，不是 Dock
- 若未來需要短暫前台視窗或 debug 視窗，也應視為例外 surface，而不是把 Dock 恢復成常駐主入口

這個約束的目的不是單純少一個圖標，而是避免 desktop runtime owner / tray utility / browser management surface 的產品定位再次漂回一般前台桌面 app。

### 2. Web 管理面

瀏覽器管理面是正式管理面，不是單純 token 設定頁。

在前端實作上，現在的靜態 HTML/CSS/JS 骨架比較像過渡方案。

若之後要把管理面收斂成更正式的產品 UI，一個合理方向是：

- 以本地 vendored、無 build 的 Tabler 風格 CSS 骨架重構 web 管理面

這條線的重點不是單純換皮，而是讓下面這些區塊有更穩定的組件化結構：

- workspace list / cards
- runtime health summary
- recent session list
- `Sessions` timeline pane
- managed Codex settings
- archive / restore / repair / adopt 等 action 的確認流程

v1 的主模型固定為：

- `workspace = thread`
- `Workspaces` 是唯一主實體列表
- `Archived Workspaces` 是唯一歷史列表
- `ThreadStateView` 保留給 runtime / debug 視角，不再作為普通用戶主區塊

首頁至少顯示：

- managed Codex binary path / version / ready
- app-server status
- hcodex ingress status
- runtime readiness
  - `ready`: app-server 與 hcodex ingress 都可用，且沒有 pending adoption
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

如果引入這條 Tabler 風格 CSS 重構，較合理的定位應是：

- 只重構前端呈現與互動組件
- 不引入 React、Node build pipeline、或外部 CDN 依賴
- dark mode 以 system theme 為準，不引入手動 toggle、localStorage theme persistence、或 theme 專屬 API
- 不改變 workspace-first 的資訊架構
- 不在 UI 層重新發明狀態模型

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

- tray menu 不應再分流這些 action；除 `New Session` 與 `Continue Telegram Session` 外，其餘入口都留在 web 管理面
- `Add Workspace` 應走 native folder picker，而不是要求普通用戶輸入絕對路徑
- `repair continuity` 應按狀態自動選擇 `Adopt TUI` 或 `Repair Session`
- raw `bind workspace` / `reject TUI` 可以保留，但只應存在於 advanced / debug 區

若某個 action 有風險，例如 archive / restore，應有明確確認步驟。

目前 web 管理面已開始對 archive / restore 加上顯式確認，且 conflict workspace 的 launch / resume 入口會禁用，而不是繼續允許誤操作。

`Open in Telegram` 不作為 v1 必選 action；只有在驗證出穩定 deep link 方案後才加入。

### 4. First-Run Onboarding

目前仍未提供完整 onboarding，但新增確認一條明確的 first-run flow 草稿。

第一次使用引導建議拆成 welcome 頁上的 setup 與 setup 儲存後的一般管理面後續操作：

1. 啟動 desktop runtime 後先顯示 native welcome alert，再打開 web 管理面的 welcome 頁。
2. welcome 頁完成 Telegram setup：
   - 引導使用者打開 `@BotFather` 建立 bot，並填寫 bot token
   - 引導使用者打開 `@userinfobot` 取得 Telegram user id
   - 儲存 token 與 authorized user ids
3. setup 儲存成功後，離開 welcome 頁，回到一般管理面承接剩餘步驟。
4. 一般管理面承接後續操作：
   - 給出 bot URL，讓使用者打開 bot 並發送第一條 `/start`
   - `control chat` ready 後，再新增第一個 workspace
   - 第一個 workspace 建立後，只提示使用者往該 workspace 發 `Hi`，不追蹤它是否已完成

這條 onboarding 的目的不是再做一套獨立設定精靈，而是把既有正式管理面上的關鍵首次操作串成一條最短成功路徑。

因此它應遵守幾個約束：

- onboarding 的主承載面仍是 web 管理面，不是 tray menu
- tray / alert 只負責 first-run 提示與導流，不承擔完整表單流程
- `welcome` 頁只承載 bot 與 authorized user setup，不把 `/start` 與 first workspace 一起塞進 first-run page
- 一般管理面的 overview / workspaces 承接 setup 之後的後續提示，而不是再開第二套 wizard
- 若使用者已完成 setup 存檔，就不應反覆彈出同樣的 first-run welcome

first-run gate 也應固定一條明確規則：

- 是否屬於第一次使用，應以 data root 下 `config.env.local` 是否存在作為唯一判斷依據
- 這裡的 `config.env.local` 指的是 runtime 解析出的 `<data-root>/config.env.local`，不是 repo 內固定相對路徑
- 若檔案不存在，可視為尚未建立本機 setup，desktop runtime 可顯示 welcome / 導流
- 若檔案已存在，即使 token、authorized users 或 control chat 仍未完成，也不再視為 first-run；後續只呈現一般 setup 缺失提示

這樣做的原因是把「首次使用」和「setup / onboarding 是否完整」拆開：

- `config.env.local` 是目前本機 Telegram setup 的持久化落點，`PUT /api/setup/telegram` 會直接寫入這個檔案
- `GET /api/setup` 回傳的 `telegram_token_configured`、`control_chat_ready`、workspace 數量與 bot URL，應用於顯示 setup 儲存後與一般管理面中的剩餘步驟，而不是回頭重新判定 first-run

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
- `hcodex_ingress_status`
- `handoff_readiness`
- `runtime_health_source`
  - `owner_heartbeat`
  - `owner_pending`
  - `owner_required`
- `heartbeat_last_checked_at`
- `heartbeat_last_error`
- `session_broken_reason`

但這裡要明確區分：

- owner heartbeat
  - 回答 runtime health
- workspace shared status / state file
  - 回答 session / local activity 或 artifact observation

現在管理面同時暴露 `runtime_health_source`，其實反映的是目前仍處於過渡性多來源模型，而不是已經有完全收斂的單一 heartbeat authority。

`SetupStateView` 還應暴露 desktop 能力位，至少包含：

- `native_workspace_picker_available`

`RuntimeHealthView` 除了 machine-level aggregate status，也應暴露：

- `ready_workspaces`
- `degraded_workspaces`
- `unavailable_workspaces`
- `build_info_file_path`
- `build_info`

這一層應盡量沿用 [runtime-protocol.md](../runtime-control/runtime-protocol.md) 的命名，而不是在 UI 層另造狀態模型。

也就是說，這條 Tabler 風格 CSS 重構若落地，應被理解為：

- web 管理面前端實作收斂

而不是：

- 管理面產品模型重寫

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
- `hcodex` ingress
- handoff continuity
- healthcheck / restart
- 對外發布穩定的 local status surface

只要 handoff continuity 依賴 workspace `ws` runtime，這個責任就不應再留給 Telegram bot 或 `hcodex` 的過渡性 self-heal。

### 3. tray 只是 `hcodex` 的 launch surface

托盤程式新增 workspace 啟動入口後，不能和現有 `./.threadbridge/bin/hcodex` 形成兩套互相競爭的本地入口。

比較合理的方向是：

- `hcodex` 保持 workspace 內的正式受管本地入口
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

web 管理面需要每個 workspace 最近 5 個 Codex `thread.id`；tray 已不再提供 recent session browser。

這份歷史應由 runtime 正式維護，而不是由 UI 掃描本地檔案臨時拼裝。

更新來源至少包括：

- Telegram 正常 turn
- `/start_fresh_session`
- `/repair_session_binding`
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
- `/start_fresh_session`
- `/repair_session_binding`
- `/archive_workspace`
- `/restore_workspace`

## 與其他計劃的關係

- [session-lifecycle.md](../runtime-control/session-lifecycle.md)
  - 定義 thread / workspace / Codex thread 的控制語義
- [runtime-state-machine.md](../runtime-control/runtime-state-machine.md)
  - 應提供這個管理面要顯示的 canonical 狀態軸
- [runtime-protocol.md](../runtime-control/runtime-protocol.md)
  - 應提供本地 query / control surface 的 view / action 命名
- [session-level-mirror-and-readiness.md](../runtime-control/session-level-mirror-and-readiness.md)
  - 定義 shared daemon、受管 `hcodex`、`hcodex` ingress、adoption 與 owner 現況
- [telegram-webapp-observability.md](../telegram-adapter/telegram-webapp-observability.md)
  - observability 可共用同一份 thread state / event model，但不是這份文檔的主責

## 開放問題

- `Open in Telegram` 是否有穩定可用的 deep link 方案？
- desktop runtime 是否最終要拆成 tray 進程 + helper 進程？
- managed Codex binary 的 update UX 要做成自動、手動，還是兩者並存？
- web 管理面與未來 observability 面是否共用同一個 web shell？

## 建議的下一步

1. 先把已存在的 local query / control API、typed SSE、session observability pane、與 execution mode controls 視為管理面 v1 的既有骨架，不再把它們當成前置待辦。
2. 讓 [runtime-protocol.md](../runtime-control/runtime-protocol.md) 與這份文檔一起收斂 naming，特別是 workspace/thread/session/control 的 user-facing vocabulary。
3. 繼續收斂 tray-icon UI 與 workspace-first 瀏覽器管理頁，特別是 `workspace = thread` 主模型、desktop-only 啟動、以及 first-run onboarding 的 welcome -> token -> first workspace 最短路徑。
4. 持續把 managed Codex update/install UX 從目前可用骨架收斂成更正式的產品面，而不是只停在 raw build / refresh action。
5. 若要正式重構 web 管理面，採用本地 vendored、無 build 的 Tabler 風格 CSS 骨架，第一階段先覆蓋 hero、setup/runtime 概覽、workspace/archived 主列表與 action shell，不重寫現有 query / SSE / session panes。
