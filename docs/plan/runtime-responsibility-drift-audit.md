# Runtime Responsibility Drift Audit

## 目前進度

這份文檔目前已進入「部分落地」。

它不是新的角色主規格，也不是重構執行計劃；它是一份以 current code 為準的 implementation audit ledger。

目前已完成的部分：

- 已用 [runtime-architecture.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-architecture.md) 的 canonical role boundary 掃描 active code path
- 已確認 4 個 responsibility drift 功能點
- 其中 3 個已和 `runtime-architecture` 的 temporary exception 對上
- 其中 1 個屬於目前尚未進入主文檔的新增 drift

目前尚未完成的部分：

- 這一輪沒有宣稱已完整掃描整個 repo
- 這一輪不收錄候選 drift 或弱訊號耦合
- 這一輪不直接拆出 refactor steps、owner migration 順序或實作任務

## 問題

[runtime-architecture.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-architecture.md) 已經把 canonical role 與 temporary exception 定義清楚了，但它仍然是主規格文檔，不是 current-code 掃描報告。

實際遇到 bug、兼容回歸、或功能止血時，維護者真正需要的是另一份文檔：

- 哪些功能點今天仍在跨層
- 這些跨層具體落在哪些 code anchors
- 哪些 drift 已被主文檔承認
- 哪些 drift 其實還沒被正式記錄

如果沒有這份 implementation-facing audit，很容易再次出現兩種錯覺：

- 以為主文檔裡只列了 3 個 temporary exception，代碼就只剩 3 個 hotspot
- 以為某些 management / Telegram / observer 的耦合只是局部 helper reuse，而不是功能責任已偏移

## 定位

這份文檔是 `threadBridge` 目前 runtime architecture 的 **實作偏移審計文檔**。

它處理：

- 以功能點為單位記錄 confirmed responsibility drift
- 為每個 drift 提供 today code anchors
- 指出按 canonical role 判斷，責任應回到哪一層
- 指出這個 drift 是否已被 `runtime-architecture` 正式記錄

它不處理：

- 重新定義 canonical role
- 擴寫 `runtime-architecture` 的 normative 邊界
- 把 drift 直接拆成重構任務
- 收錄「可能有問題但尚未確認」的候選耦合

## Confirmed Drift

### 1. management / desktop control actions 仍依賴 Telegram runtime 存活

- 功能點：
  - management API 的 `add_workspace`、`adopt_tui_session`、`reject_tui_session`、`restore_thread`、`repair_session_binding`
- 當前 responsibility drift：
  - management / desktop surface 的多個 control action 仍要先取得 `LocalControlHandle`
  - 而 `LocalControlHandle` 只有在 Telegram bot runtime 啟動後，才會由 bot runner 注入到 management API
  - 這使 management surface 的控制能力，實質上被 Telegram adapter availability 綁住
- today code anchors：
  - [management_api.rs](/Volumes/Data/Github/threadBridge/rust/src/management_api.rs#L72)
  - [management_api.rs](/Volumes/Data/Github/threadBridge/rust/src/management_api.rs#L957)
  - [management_api.rs](/Volumes/Data/Github/threadBridge/rust/src/management_api.rs#L1405)
  - [management_api.rs](/Volumes/Data/Github/threadBridge/rust/src/management_api.rs#L1459)
  - [management_api.rs](/Volumes/Data/Github/threadBridge/rust/src/management_api.rs#L1600)
  - [management_api.rs](/Volumes/Data/Github/threadBridge/rust/src/management_api.rs#L1609)
  - [bot_runner.rs](/Volumes/Data/Github/threadBridge/rust/src/bot_runner.rs#L53)
- 預期 owner role：
  - shared `runtime_control` 應擁有 control semantics
  - management / desktop surface 應只是一個 surface consumer
  - Telegram adapter 不應成為 management control availability 的前置依賴
- 為何這是偏移：
  - 這不是單純 transport side effect，而是 management surface 的正式 control path 被 Telegram bot runtime 狀態卡住
  - 代表 desktop / management surface 還沒有真正脫離 Telegram path
- 是否已在 `runtime-architecture` 記錄：
  - 尚未記錄
- 退出方向：
  - 將 management action 的 shared control semantics 與 Telegram side effect bridge 分開
  - 讓 management surface 可直接調 shared control，再由 Telegram adapter 處理必要的 Telegram-facing side effect

### 2. `app-server observer` 仍直接依賴 Telegram final reply composition

- 功能點：
  - observer turn finalization
- 當前 responsibility drift：
  - observer 在 finalize turn 時直接組出 Telegram-visible final reply text
- today code anchors：
  - [app_server_observer.rs](/Volumes/Data/Github/threadBridge/rust/src/app_server_observer.rs#L24)
  - [app_server_observer.rs](/Volumes/Data/Github/threadBridge/rust/src/app_server_observer.rs#L499)
- 預期 owner role：
  - `app-server observer` 應只做 read-side projection
  - Telegram final reply composition 應留在 shared projection helper 或 Telegram adapter
- 為何這是偏移：
  - observer 已經跨進 adapter-specific output language
  - 這會讓 observer 的 read-side projection 邊界重新和 Telegram renderer 黏在一起
- 是否已在 `runtime-architecture` 記錄：
  - 已記錄，對應 temporary exception #1
- 退出方向：
  - 將 final text composition 抽成 shared helper，或讓 Telegram adapter 在消費 observer projection 時自行組裝

### 3. Telegram `/launch` 與 `/execution_mode` 仍直接依賴 management API view/helper

- 功能點：
  - Telegram `/launch`
  - Telegram `/execution_mode`
- 當前 responsibility drift：
  - Telegram adapter 直接 import management surface 的 view 型別與 desktop launch helper
  - 同時在 Telegram path 重複構造與 management surface 相同的 launch / execution mode model
- today code anchors：
  - [thread_flow.rs](/Volumes/Data/Github/threadBridge/rust/src/telegram_runtime/thread_flow.rs#L21)
  - [thread_flow.rs](/Volumes/Data/Github/threadBridge/rust/src/telegram_runtime/thread_flow.rs#L182)
  - [thread_flow.rs](/Volumes/Data/Github/threadBridge/rust/src/telegram_runtime/thread_flow.rs#L202)
  - [thread_flow.rs](/Volumes/Data/Github/threadBridge/rust/src/telegram_runtime/thread_flow.rs#L703)
  - [thread_flow.rs](/Volumes/Data/Github/threadBridge/rust/src/telegram_runtime/thread_flow.rs#L813)
  - [management_api.rs](/Volumes/Data/Github/threadBridge/rust/src/management_api.rs#L199)
  - [management_api.rs](/Volumes/Data/Github/threadBridge/rust/src/management_api.rs#L217)
  - [management_api.rs](/Volumes/Data/Github/threadBridge/rust/src/management_api.rs#L1850)
  - [management_api.rs](/Volumes/Data/Github/threadBridge/rust/src/management_api.rs#L1873)
- 預期 owner role：
  - shared `runtime_control` 或 shared runtime semantics 應提供 launch / mode model
  - Telegram adapter 與 management / desktop surface 都只做 surface mapping
- 為何這是偏移：
  - management surface 的 transport-facing model 仍被 Telegram adapter 當成內部共享模型使用
  - 也表示 launch / mode semantics 目前仍卡在錯的層級上
- 是否已在 `runtime-architecture` 記錄：
  - 已記錄，對應 temporary exception #2
- 退出方向：
  - 將 launch config 與 execution mode view 收斂回 shared semantics
  - Telegram 與 management surface 各自維持最薄的 presentation / trigger layer

### 4. `local_control` 仍是 management path 借來用的 Telegram side-effect toolbox

- 功能點：
  - local management UI 的 create / bind / restore / adopt / reject / repair flow
  - Telegram `/add_workspace` 對同一個 helper 的重用
- 當前 responsibility drift：
  - `local_control` 雖位於 adapter 之外，但它直接持有 `Bot`，並依賴 `telegram_runtime::{AppState, send_scoped_message, status_sync, thread_id_to_i32}`
  - 它同時執行 shared control 與 Telegram-facing side effect
- today code anchors：
  - [local_control.rs](/Volumes/Data/Github/threadBridge/rust/src/local_control.rs#L9)
  - [local_control.rs](/Volumes/Data/Github/threadBridge/rust/src/local_control.rs#L12)
  - [local_control.rs](/Volumes/Data/Github/threadBridge/rust/src/local_control.rs#L41)
  - [local_control.rs](/Volumes/Data/Github/threadBridge/rust/src/local_control.rs#L49)
  - [local_control.rs](/Volumes/Data/Github/threadBridge/rust/src/local_control.rs#L133)
  - [local_control.rs](/Volumes/Data/Github/threadBridge/rust/src/local_control.rs#L271)
  - [local_control.rs](/Volumes/Data/Github/threadBridge/rust/src/local_control.rs#L347)
  - [thread_flow.rs](/Volumes/Data/Github/threadBridge/rust/src/telegram_runtime/thread_flow.rs#L420)
- 預期 owner role：
  - shared `runtime_control` 負責 workspace / session mutation
  - Telegram adapter 僅負責送訊息、topic title refresh、與 Telegram topic side effect
- 為何這是偏移：
  - `local_control` 現在實際上是一個把 management path 和 Telegram adapter 黏在一起的混合 helper
  - 它也是 management control 依賴 Telegram runtime 的主要載體
- 是否已在 `runtime-architecture` 記錄：
  - 已記錄，對應 temporary exception #3
- 退出方向：
  - 將 shared mutation path 與 Telegram side effect bridge 拆開
  - 讓 `local_control` 不是「混合 helper」，而是明確降格成某一側的 adapter bridge，或被更乾淨的 shared service 取代

## 本輪未升格為 Confirmed Drift 的觀察

像 `bot_runner -> telegram_runtime::status_sync` 這種 bootstrap / watcher 耦合，這一輪先不升格為 confirmed drift。它值得後續再檢查，但目前不納入這份文檔的正式結論。

## 與其他計劃的關係

- [runtime-architecture.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-architecture.md)
  - canonical role boundary 的主文檔
  - 本文不重新定義角色，只驗證 current code 在哪些功能點偏離它
- [session-lifecycle.md](/Volumes/Data/Github/threadBridge/docs/plan/session-lifecycle.md)
  - 若 drift 牽涉 session bind / repair / adoption semantics，回看這份文檔理解正式語義
- [telegram-adapter-migration.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-adapter-migration.md)
  - 若後續要處理 Telegram adapter 的責任退出，可把本文的 confirmed drift 當成 migration input
- [app-server-ws-mirror-observer.md](/Volumes/Data/Github/threadBridge/docs/plan/app-server-ws-mirror-observer.md)
  - observer 相關 drift 的子背景仍以這份文檔為準

## 開放問題

- 是否要把本文第 1 項 drift 直接升格進 [runtime-architecture.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-architecture.md) 的 temporary exception？
- 後續若再做第二輪掃描，是否要把 `bot_runner / status_sync` 這類候選耦合納入 confirmed scope？
- 這份文檔未來若長期存在，是否要再補一個簡短的 priority / risk 欄位，幫助安排重構順序？
