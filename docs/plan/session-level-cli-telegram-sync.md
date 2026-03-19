# Session-Level CLI / Telegram 同步

## 目前進度

這份 Plan 現在是部分落地。

目前已落地：

- threadBridge 已經從 workspace-level 單快照升級到 `workspace aggregate + per-session registry`
- Telegram thread binding 已經有明確的 `selected_session_id`
- topic title 已經改成 ownership 標記：
  - `.cli` = `hcodex live / Telegram viewer`
  - `.attach` = `Telegram live / threadbridge_viewer`（reedline viewer）
- `/attach_cli_session` 已落地
- `/attach_cli_session` 現在是排他式 handoff，不是單純選中
- attach 成功時會結束本地 `codex` TUI，並回覆 `codex resume <session-id>`
- busy gate 已經拆成：
  - selected-session turn busy gate
  - selected live CLI session ownership gate
- Telegram 對已 attach 的 CLI session 發文字 / 圖片分析時，已經會對同一個 `thread.id` 做 `thread/resume` + `turn/start`

目前已確認：

- `codex app-server` 已經有 `thread/start`、`thread/resume`、`turn/start`、`turn/interrupt`
- 同一個 thread 可以被多個 connection 訂閱 turn / item 事件
- `thread/resume` 對已經 running 的 thread 有明確處理路徑
- `thread/unsubscribe` 只會在最後一個 subscriber 離開時才真正 unload thread
- TUI 的 session layer 同時支持 embedded 和 remote app-server client

目前尚未具備：

- threadBridge 與本地 `codex` CLI 共用同一個 live app-server runtime
- CLI 正在進行中的 turn/item/delta 事件完整鏡像到 Telegram
- Telegram 發出的 turn 在本地 CLI 開著的情況下，被 CLI 以 live attach UI 方式即時看見
- 真正的 shared live TUI continuity

## 名詞固定

- `· cli`
  - `hcodex` 現在是 live
  - Telegram 是 viewer
  - Telegram 只應看到 `CLI user + Codex final`
- `· attach`
  - Telegram 現在是 live
  - 本地 `codex` TUI 已被 kill
  - 本地終端改跑 `threadbridge_viewer`（reedline viewer）
  - viewer 只應看到 attach 之後的 `Telegram user + Codex final`
- viewer 只顯示 `user + assistant`
- `user` 文本要求在送出後立刻鏡像
- `assistant` 只顯示最終文本
- Telegram 作為 live 時，只有普通文字消息進入 viewer timeline；命令、系統事件、圖片分析內部 prompt 不算 viewer 文本

## 目前已知缺口

- `.cli` 狀態下，CLI user prompt 的鏡像目前嚴格依賴 `UserPromptSubmit` hook
- threadBridge 不接受從 `turn_completed.input-messages`、rollout、history 對 user prompt 做 fallback
- 如果本地 `codex` build 沒有真正把 `UserPromptSubmit` 寫進 workspace 事件流，owner thread 只會看到 `Codex:` final，而不會事後補 `CLI:` user 行
- 這種缺口只應記錄 debug / warn，不應升級成 `.cli!`，也不應在 Telegram thread 發系統提示

## 願景

使用者可以把 CLI 或 Telegram 視為同一個 Codex session 的兩個輸入窗口。

具體來說：

- 在 Telegram 輸入的內容，若綁定到同一個 Codex thread / session，CLI 可以看到並延續
- CLI 發起的 turn，其 turn 級別事件與最終結果可以同步到 Telegram
- 使用者可以隨時切換「在 CLI 繼續」或「在 Telegram 繼續」，而不是在兩條分裂的 session 上工作

## 為什麼現有 Hook V1 不夠

現有 [codex-cli-telegram-status-sync-hooks.md](/Volumes/Data/Github/threadBridge/docs/plan/codex-cli-telegram-status-sync-hooks.md) 解決的是 workspace-level busy signal。

它能做到：

- Telegram 知道本地 CLI 是否活著
- Telegram 知道 workspace 是否忙碌
- Telegram 避免和本地 CLI 對同一 workspace 並發發起新 turn

它做不到：

- Telegram 把輸入送進 CLI 正在使用的同一個 live session
- CLI 直接看到 Telegram 插入的 turn
- Telegram 訂閱並呈現 CLI 正在進行中的 item / delta / turn 事件

所以這份 Plan 處理的是 session-level sync，而不是 workspace-level occupancy。

## 調研結論

### 1. Codex app-server 協議層具備 session 級共享的基礎能力

從 `codex` 原始碼來看：

- app-server 有明確的 `thread/start`、`thread/resume`、`turn/start`
- app-server 會在 thread 上維護 subscriber 集合，事件會推送給所有訂閱該 thread 的 connection
- `thread/unsubscribe` 只有在最後一個 subscriber 離開時才會 unload thread
- `thread/resume` 對已經 running 的 thread 有專門的 `resume_running_thread()` 路徑

這表示「同一個 thread 被多個前端共同觀察與繼續對話」在協議模型上是成立的。

### 2. TUI 內部已經有 remote app-server client 能力

`codex` 的 TUI session layer 不只支持 embedded/in-process，也支持 remote app-server client。

這說明「CLI 接到外部 app-server」在程式架構上不是不可能。

### 3. 但公開 CLI 入口目前沒有把 remote 模式打開

目前公開的 `codex` CLI 入口仍然把 TUI 啟動固定在 `remote = None`。

這意味著：

- 本地 `codex` 啟動時，通常還是在自己的進程內建立 app-server runtime
- threadBridge 目前也是自己單獨啟動 `codex app-server --listen stdio://`
- 兩邊即使使用同一個 persisted thread id，也不是連到同一個 live app-server runtime

因此，真正的「CLI / Telegram 隨時切換同一個 live session」目前不是 threadBridge 單邊就能完整落地。

## 設計目標

### 目標 1: 同一個 thread id 成為雙端共享的 session identity

- Telegram thread 不只綁定 workspace，也綁定一個明確的 Codex thread id
- CLI 若要接管或恢復，應顯式進入同一個 thread id，而不是只靠 cwd 匹配

### 目標 2: 輸入走 session，而不是走“哪個前端先占住 workspace”

- Telegram 發送訊息時，如果目標 session 正存在，就向同一個 session 發 `turn/start`
- CLI 在相同 session 裡輸入時，也向同一個 session 發 `turn/start`
- 兩端都以 session 為中心，而不是各自創建獨立 runtime 再靠 rollout 歷史補 continuity

### 目標 3: CLI 與 Telegram 都能訂閱同一個 session 的 turn 事件

- turn 開始、item started/completed、agent message delta、turn completed
- 兩端都能收到同一個 thread 的事件流
- Telegram 可以做 preview / title / final reply
- CLI 可以保留自己的交互渲染

## 需要拆開看的兩條路

### A. threadBridge-only 增量版

不修改上游 `codex`，只能做到有限近似：

- Telegram 端繼續綁定 persisted `thread.id`
- 本地 CLI 仍然使用自己的 in-process app-server runtime
- 透過 hooks / notify / rollout 變化，把 CLI turn 的結果更完整地反映到 Telegram

這一版最多能做到：

- 更接近 session continuity 的感知
- 更完整的 CLI -> Telegram turn 完成同步

但仍做不到真正的：

- Telegram 輸入直接進入 CLI 正在使用的 live session
- CLI 即時看到 Telegram 插入的 turn

### B. 完整共享 session 版

這一版需要 upstream `codex` 配合，至少滿足其中一條：

- 公開 CLI 入口支持連接到外部 remote app-server
- 或提供一個新的 CLI 啟動模式，讓 TUI 可以附著到既有 app-server thread

threadBridge 這邊則需要：

- 維護長壽命 app-server runtime，而不是每次 turn 臨時起一個 `stdio://` 進程
- 按 workspace 或按 session 管理 app-server endpoint
- 保存 Telegram thread -> session endpoint -> Codex thread id 的綁定
- 讓 Telegram 和 CLI 都對同一個 app-server thread 做 `thread/resume` / `turn/start`

## 建議的實作分期

### Phase 0: 文檔與模型收斂

- 明確把 Hook V1 定位為 workspace busy signal
- 補出 session-level 願景與 upstream 依賴

### Phase 1: threadBridge 端 session registry

目前已大致落地：

- 在 bot-local state 中引入 `session-runtime.json`
- 區分：
  - persisted `thread.id`
  - live runtime endpoint
  - runtime owner / last attached client
- Telegram title 與 busy 文案改成 session-aware，而不是只有 `cli` / `bot`

### Phase 2: CLI / Telegram 同 session 的被動同步

目前已部分落地：

不要求同一個 live runtime，只先做到：

- Telegram 能更準確識別 CLI 正在使用哪個 persisted thread id
- Telegram 可以手動 attach 到 live CLI session
- Telegram 往 attach 後的 session 發 turn 時，會沿用同一個 persisted `thread.id`
- CLI turn 完成後，Telegram thread 能讀到 session 級狀態與最後摘要

這一階段仍然不能承諾輸入窗口可無縫切換。

### Phase 3: upstream Codex remote attach 能力

需要上游 `codex` 提供正式入口，例如：

- `codex` 啟動時傳入 remote app-server URL
- TUI 對既有 thread id 做 attach / resume
- 對外部注入的 turn 保持正常 UI 呈現

### Phase 4: 真正的 session handoff

在具備 shared live runtime 之後，threadBridge 才能實作：

- Telegram 送入的 prompt 直接進入 CLI 正在觀察的同一個 thread
- CLI 發起的 turn 全量事件同步到 Telegram
- 使用者在兩端之間切換輸入窗口，而不丟失 session continuity

## threadBridge 代碼面的主要缺口

目前 threadBridge 的 Codex 整合模型仍是：

- 每次操作臨時啟動一個 `codex app-server --listen stdio://`
- 對既有 thread id 做 `thread/resume`
- 跑完當前 turn 後結束這個 app-server client

這個模型足夠支持 persisted thread continuity，但不支持 shared live session。

因此，要進入這份 Plan 的完整目標，threadBridge 自身也需要從「每 turn 一個短連線」改成「長壽命 session runtime」。

## 風險與開放問題

- 若 CLI 與 Telegram 同時向同一 session 發 `turn/start`，最終互斥策略要由誰決定
- 若 CLI 離線但 session runtime 仍由 threadBridge 持有，CLI 重新 attach 的 UX 如何設計
- 若 remote app-server 只支持 websocket，threadBridge 的部署與安全邊界要重新定義
- 是否需要把「同一個 workspace 多個 session」與「同一個 session 多個 frontend」分成兩個不同模型

## 完成條件

只有滿足以下條件，才算真正達成這份 Plan：

- Telegram 與 CLI 可以明確指向同一個 Codex thread id
- Telegram 新輸入能進入 CLI 正在觀察的同一個 live session
- CLI turn 的 turn/item 事件能同步到 Telegram，而不只是在完成後寫狀態
- 使用者可以在 CLI 和 Telegram 之間切換輸入窗口，而不需要新建 session
