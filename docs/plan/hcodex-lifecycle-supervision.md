# Hcodex Lifecycle Supervision 草稿

## 目前進度

這份文檔目前已進入「部分落地」。

目前代碼裡已經存在的部分：

- `hcodex` 已是受管本地入口，而不是獨立 runtime owner
- `run-hcodex-session` 已成為正式的本地 launcher 執行點
- `workspace_status` 已有 `record_hcodex_launcher_started` / `record_hcodex_launcher_ended`
- `LocalTuiSessionClaim`、session snapshot、`tui_active_codex_thread_id`、adoption flow 已形成現行 TUI lifecycle 模型

目前尚未完成的部分：

- `hcodex` 尚未成為真正穩定的 local Codex child supervisor
- cleanup 仍主要依賴 `run-hcodex-session` 的 happy path
- signal / hangup / wrapper 自身異常退出時，缺少強保證的 teardown / reconciliation contract
- `workspace_status` 對 stale TUI busy state 的 recover policy 先前未被明確寫成主文檔

目前新增確認的一個回歸事實是：

- `tui_active_codex_thread_id = none`，但 `current_codex_thread_id` 對應的 session snapshot 仍可殘留 `activity_source = Tui` 與 `phase = TurnRunning`
- 在這個形狀下，Telegram busy gate 會把 thread 鎖成 `run_status = running`
- 這說明 today 的 `hcodex` lifecycle cleanup 契約不足以覆蓋異常退出形狀

## 問題

`hcodex` 在近期重構裡，邊界變得更清楚了，但也把一個以前較髒、卻更完整的責任面拆薄了：

- 舊模型裡，shell wrapper 與 shell-exit event path 對本地 `codex` child 有較強的生命周期閉環
- 新模型裡，`hcodex` 變成受管本地入口，owner / observer / ingress / bridge 與 launcher 各自拆開

這個方向本身是正確的，但它引出一個 today 的缺口：

- `hcodex` 已不再是 runtime owner
- 但它仍然是本地 `codex --remote` 子進程的直接啟動者
- 同時 `workspace_status`、Telegram busy gate、adoption、management observability 仍依賴它把本地 session 正確收尾

若這條本地 launcher path 只在正常 `child.wait()` 完成後才做 cleanup，就會留下：

- stale `LocalTuiSessionClaim`
- stale TUI busy snapshot
- 不再存在的 live TUI session 仍被 Telegram / management surface 視為 busy

所以這份文檔要固定的核心不是「`hcodex` 要不要重新變 owner」，而是：

- `hcodex` 必須重新成為它自己啟動的 local Codex child 的 lifecycle supervisor

## 歷史轉折

目前從 git 歷史可確認三個重要階段。

### 1. Shell 閉環時期

較早的 workspace shell / wrapper 模型會明確持有：

- shell process lifecycle
- child pid / child pgid 記錄
- shell exit event
- child info file / wrapper-side cleanup

代表提交包括：

- `d93d9f0` `feat(workspace): 新增 codex 啟動包裝腳本並重構子程序管理邏輯`
- `501763d` `feat(telegram_runtime): 新增子行程追蹤與終止邏輯`

這個模型很髒，混有較多 CLI / handoff 時代語義，但它有一個優點：

- 本地 `codex` child 的開始與結束，都仍被一條較完整的 shell lifecycle 鏈包住

### 2. Rust launcher 取代直接 `exec`

`88d4bb1` `fix(threadbridge): stabilize hcodex mirror lifecycle` 是明確的轉折點。

從這次開始：

- workspace `hcodex` launcher 不再直接 `exec "$codex_bin" --remote ...`
- 而是改成呼叫 `run-hcodex-session`
- `run-hcodex-session` 內部用 Rust `spawn + wait` 啟動真正的 Codex child
- `workspace_status` 的本地 lifecycle write-path 開始依賴：
  - `record_hcodex_launcher_started`
  - `record_hcodex_launcher_ended`

這一步讓 `hcodex` 正式成為 local child lifecycle 的 write-side 責任者。

### 3. Owner / observer / launch contract 進一步收斂

後續幾次重構讓 runtime 邊界更合理：

- `3c627be` `feat(threadbridge): make desktop runtime the sole owner`
- `b538b9d` `refactor(threadbridge): split tui ingress from app-server observer`
- `fa06570` / `a0936bf` / `00d3814` 進一步固定 launch URL、bridge 與 reconnect contract

這些提交主要收斂的是：

- owner authority
- observer read-side
- ingress / bridge launch contract

但它們沒有把 `run-hcodex-session` 提升成更強的 lifecycle supervisor。

結果是：

- launch contract 更清楚了
- 但 local child lifecycle cleanup 仍主要依賴 happy path

## 定位

這份文檔處理的是：

- `hcodex` 作為受管本地入口，應承擔哪些 local Codex child lifecycle 責任
- `workspace_status` / local claim / Telegram busy gate 對 `hcodex` 的最小依賴契約
- 哪些退出形狀應由 `hcodex` 主動完成 cleanup 或 reconciliation

這份文檔不處理：

- ingress launch URL 與 `launch_ticket` 的 websocket contract 細節
- `hcodex-ws-bridge` 的 replay / reconnect 內部協議
- adoption UI / Telegram callback 呈現
- desktop owner 本身的 runtime health authority

## 當前缺口

today 的主要缺口不是「`record_hcodex_launcher_ended` 函數本身壞掉」，而是：

- `record_hcodex_launcher_ended` 被放在過度脆弱的 lifecycle 契約上

具體來說，當前模型過度依賴：

1. `run-hcodex-session` 成功走到 `child.wait().await`
2. wrapper 本身沒有先於 cleanup 被 signal 或 hangup 終止
3. `LocalTuiSessionClaim` 中保存的 `thread_key` / `shell_pid` / `child_pid` 仍與 cleanup 時讀到的值一致

只要其中任一條不成立，就可能出現：

- claim 已消失，但 stale busy snapshot 還在
- `tui_active_codex_thread_id` 已清掉，但 current session snapshot 還留著 `Tui + TurnRunning`
- Telegram 仍把該 thread 視為 shared TUI session 正在執行 turn

## 正式責任

`hcodex` 在現行模型中至少必須承擔下面四個責任，而且不能只做到前兩個。

這四個責任可以濃縮成一條總約束：

- `hcodex` 不必是 runtime owner，但它必須是自己所啟動之 `codex --remote` child 的完整本地 lifecycle supervisor。

這裡的「完整本地 lifecycle supervisor」不是指「負責 spawn 一次」而已，而是必須對同一條本地 TUI session 的整個閉環負責：

- launch 前取得正確 session target
- spawn `codex --remote`
- 持有與維護 local claim / launcher ownership
- 轉發 signal
- 等待 child 結束
- 完成 teardown
- 完成最終 workspace-state reconciliation

只要其中任一段沒有正式責任者，`hcodex` 相關狀態就仍可能退化回「靠 read-side recover 擦屁股」的脆弱模型。

### 1. Launch orchestration

- 選定 thread
- 決定 launch URL / local bridge
- 啟動上游 `codex --remote`

### 2. Child process supervision

- 記錄實際 child pid / command
- 區分 launcher process 與真正的 Codex child
- 對 child 終止形狀有最小可觀測能力

### 3. Signal forwarding and teardown

- wrapper 自己收到 `SIGINT` / `SIGHUP` / `SIGTERM` / `SIGQUIT` 時，不能只讓自己直接死掉
- 它必須先決定：
  - 轉發給 child
  - 等待 child 結束或進入有限 timeout
  - 再完成本地 cleanup

### 4. Final status reconciliation

- 若 launcher 正常收尾，應寫回 canonical `Idle`
- 若 launcher 異常退出，仍應有 recovery path 能把 stale TUI busy state 收斂掉
- `workspace_status` 不能永遠相信一份舊的 `Tui + TurnRunning` snapshot

## 維護不變式

任何後續重構只要碰到 `run-hcodex-session`、`workspace_status`、local claim 或 Telegram busy gate，都必須保住下面幾條。

- `hcodex` 不必是 runtime owner，但必須是 local Codex child lifecycle 的責任者。
- `hcodex` 的最小責任單位不是「成功啟動 Codex」，而是「完整收斂它自己啟動的本地 TUI session lifecycle」。
- `hcodex` 可以依賴 upstream Codex 提供 remote websocket transport、initialize、`thread/resume` 等語義，但不能把 local child supervision 委託給 upstream Codex。
- `record_hcodex_launcher_ended` 不能是唯一 cleanup 防線；它只能是正常路徑的一部分。
- stale TUI busy snapshot 不得永久鎖住 Telegram thread。
- 若 session snapshot 顯示 `activity_source = Tui` 且 `phase` 為 busy，但已無對應 live claim / process，系統必須有收斂到 `Idle` 的正式路徑。
- adoption state、`tui_active_codex_thread_id`、local claim、session snapshot 不得長期互相矛盾。

## 測試要求

凡是修改這條 lifecycle 鏈，至少要保住下面幾類測試。

- 正常 child exit 會觸發 launcher cleanup
- live local TUI session 不會被誤判成 stale busy
- stale TUI busy snapshot 會被 recover，而不是永久鎖死 busy gate
- `tui_active_codex_thread_id = none` 時，current session 不得僅因殘留的 TUI busy snapshot 而無限期維持 `run_status = running`

若未來補上 signal-aware supervision，還應新增：

- wrapper 收到中斷信號時仍能完成 teardown / reconciliation
- child 先於 wrapper 結束、wrapper 先於 child 收到信號、以及 bridge 異常退出這三類非 happy path 測試

## 與其他計劃的關係

- [hcodex-launch-contract.md](/Volumes/Data/Github/threadBridge/docs/plan/hcodex-launch-contract.md)
  - 定義 launch URL、bridge 與 upstream Codex `--remote` 的 websocket 契約
- [post-cli-runtime-cleanup.md](/Volumes/Data/Github/threadBridge/docs/plan/post-cli-runtime-cleanup.md)
  - 描述 CLI 時代遺留語義與 `hcodex` 啟動鏈的收尾工作
- [session-lifecycle.md](/Volumes/Data/Github/threadBridge/docs/plan/session-lifecycle.md)
  - 定義 `current_codex_thread_id`、`tui_active_codex_thread_id`、adoption 與 Telegram continuity 的關係
- [owner-runtime-contract.md](/Volumes/Data/Github/threadBridge/docs/plan/owner-runtime-contract.md)
  - 固定 `hcodex` 在 owner-managed runtime 中不是 owner，而是受管本地入口

## 開放問題

- `run-hcodex-session` 是否應直接進入正式的 signal-aware supervisor 形狀，還是再包一層更窄的 local supervisor 元件
- `workspace_status` 的 stale TUI recovery，應完全放在 read-side resolver，還是應補一條 owner / launcher restart 時的主動 reconciliation
- local claim 是否應明確帶出 launcher process state 與 child process state 的分離，而不只是一份當前 claim

## 建議的下一步

1. 先把這份文檔視為 `hcodex` lifecycle 子問題的主草稿，不再把它混寫成 launch contract 的附帶細節。
2. 將 `run-hcodex-session` 補成 signal-aware 的 child supervisor，而不是只做 `spawn + wait + happy path cleanup`。
3. 把 stale TUI busy recovery 與 launcher teardown 測試固定成 regression suite，避免後續再次出現「thread 被錯誤鎖死為 running」。
