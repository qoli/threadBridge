# Runtime Responsibility Drift Audit

## 目前進度

這份文檔目前已進入「部分落地」。

它不是新的角色主規格，也不是重構執行計劃；它是一份以 current code 為準的 implementation audit ledger。

目前已完成的部分：

- 已用 [runtime-architecture.md](runtime-architecture.md) 的 canonical role boundary 掃描 active code path
- 已確認 4 個 responsibility drift 功能點
- 其中 3 個已在 2026-03-26 進一步收斂回 shared runtime semantics
- 目前仍維持 active drift 的，剩 observer final reply composition 這一項

目前尚未完成的部分：

- 這一輪沒有宣稱已完整掃描整個 repo
- 這一輪不收錄候選 drift 或弱訊號耦合
- 這一輪不直接拆出 refactor steps、owner migration 順序或實作任務

## 問題

[runtime-architecture.md](runtime-architecture.md) 已經把 canonical role 與 temporary exception 定義清楚了，但它仍然是主規格文檔，不是 current-code 掃描報告。

實際遇到 bug、兼容回歸、或功能止血時，維護者真正需要的是另一份文檔：

- 哪些功能點今天仍在跨層
- 這些跨層具體落在哪些 code anchors
- 哪些 drift 已被主文檔承認
- 哪些 drift 其實還沒被正式記錄

如果沒有這份 implementation-facing audit，很容易再次出現兩種錯覺：

- 以為主文檔裡列出的 temporary exception 數量，就等於 current code 仍在活躍的 hotspot 數量
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

### 1. management / desktop control actions 曾依賴 Telegram runtime 存活（已於 2026-03-26 收斂）

- 功能點：
  - management API 的 `add_workspace`、`adopt_tui_session`、`reject_tui_session`、`restore_thread`、`repair_session_binding`
- 當前狀態：
  - shared control capability 已由 desktop owner 啟動時註冊到 management API
  - Telegram bot runtime 現在只提供 optional `telegram_bridge`
  - `repair_session_binding`、`adopt_tui_session`、`reject_tui_session`、`archive_thread` 等 control action 已可在 Telegram polling 中斷時繼續運作
  - 只有 create / restore Telegram topic 這類 action 仍保留 Telegram-required 語義
- today code anchors：
  - [management_api.rs](../../../rust/src/management_api.rs)
  - [bot_runner.rs](../../../rust/src/bot_runner.rs#L53)
  - [threadbridge_desktop.rs](../../../rust/src/bin/threadbridge_desktop.rs)
- 預期 owner role：
  - shared `runtime_control` 應擁有 control semantics
  - management / desktop surface 應只是一個 surface consumer
  - Telegram adapter 不應成為 management control availability 的前置依賴
- 收斂結果：
  - 這一項已不再是 active drift
  - 剩餘 Telegram dependency 已縮到 adapter-owned topic side effect，而不是 shared control availability
- 是否已在 `runtime-architecture` 記錄：
  - 不再需要新增 temporary exception
- 退出方向：
  - 將 management action 的 shared control semantics 與 Telegram side effect bridge 分開
  - 讓 management surface 可直接調 shared control，再由 Telegram adapter 處理必要的 Telegram-facing side effect

### 2. `app-server observer` 仍直接依賴 Telegram final reply composition

- 功能點：
  - observer turn finalization
- 當前 responsibility drift：
  - observer 在 finalize turn 時直接組出 Telegram-visible final reply text
- today code anchors：
  - [app_server_observer.rs](../../../rust/src/app_server_observer.rs#L24)
  - [app_server_observer.rs](../../../rust/src/app_server_observer.rs#L499)
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

### 3. Telegram `/launch` 與 `/execution_mode` 曾直接依賴 management API view/helper（已於 2026-03-26 收斂）

- 功能點：
  - Telegram `/launch`
  - Telegram `/execution_mode`
- 當前狀態：
  - launch / execution mode 的 shared model 與 helper 已抽回 `runtime_control`
  - Telegram adapter 與 management surface 現在共用同一套 shared runtime semantics
- today code anchors：
  - [thread_flow.rs](../../../rust/src/telegram_runtime/thread_flow.rs)
  - [runtime_control.rs](../../../rust/src/runtime_control.rs)
- 預期 owner role：
  - shared `runtime_control` 或 shared runtime semantics 應提供 launch / mode model
  - Telegram adapter 與 management / desktop surface 都只做 surface mapping
- 收斂結果：
  - 這一項已不再是 active drift
- 是否已在 `runtime-architecture` 記錄：
  - 對應 temporary exception 已退出 active list
- 退出方向：
  - 將 launch config 與 execution mode view 收斂回 shared semantics
  - Telegram 與 management surface 各自維持最薄的 presentation / trigger layer

### 4. `local_control` 曾是 management path 借來用的 Telegram side-effect toolbox（已於 2026-03-26 收斂）

- 功能點：
  - local management UI 的 create / bind / restore / adopt / reject / repair flow
  - Telegram `/add_workspace` 對同一個 helper 的重用
- 當前狀態：
  - `local_control` 已降格為 Telegram bridge
  - shared mutation path 已移回 `runtime_control`
  - Telegram `/add_workspace` 與 local management path 都改為 shared control + Telegram bridge 的分層
- today code anchors：
  - [local_control.rs](../../../rust/src/local_control.rs)
  - [runtime_control.rs](../../../rust/src/runtime_control.rs)
  - [thread_flow.rs](../../../rust/src/telegram_runtime/thread_flow.rs)
- 預期 owner role：
  - shared `runtime_control` 負責 workspace / session mutation
  - Telegram adapter 僅負責送訊息、topic title refresh、與 Telegram topic side effect
- 收斂結果：
  - `local_control` 不再是 active drift 載體
  - 剩餘 Telegram side effect 已被明確限制在 adapter bridge 內
- 是否已在 `runtime-architecture` 記錄：
  - 對應 temporary exception 已退出 active list
- 退出方向：
  - 將 shared mutation path 與 Telegram side effect bridge 拆開
  - 讓 `local_control` 不是「混合 helper」，而是明確降格成某一側的 adapter bridge，或被更乾淨的 shared service 取代

## 本輪未升格為 Confirmed Drift 的觀察

像 `bot_runner -> telegram_runtime::status_sync` 這種 bootstrap / watcher 耦合，這一輪先不升格為 confirmed drift。它值得後續再檢查，但目前不納入這份文檔的正式結論。

## 與其他計劃的關係

- [runtime-architecture.md](runtime-architecture.md)
  - canonical role boundary 的主文檔
  - 本文不重新定義角色，只驗證 current code 在哪些功能點偏離它
- [session-lifecycle.md](session-lifecycle.md)
  - 若 drift 牽涉 session bind / repair / adoption semantics，回看這份文檔理解正式語義
- [telegram-adapter-migration.md](../telegram-adapter/telegram-adapter-migration.md)
  - 若後續要處理 Telegram adapter 的責任退出，可把本文的 confirmed drift 當成 migration input
- [app-server-ws-mirror-observer.md](../app-server-observer/app-server-ws-mirror-observer.md)
  - observer 相關 drift 的子背景仍以這份文檔為準

## 開放問題

- 後續若再做第二輪掃描，是否要把 `bot_runner / status_sync` 這類候選耦合納入 confirmed scope？
- 這份文檔未來若長期存在，是否要再補一個簡短的 priority / risk 欄位，幫助安排重構順序？
