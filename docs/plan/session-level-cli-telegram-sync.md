# Session-Level CLI / Telegram 同步

## 目前進度

這份 Plan 現在是部分落地。

目前已落地：

- `selected_session_id` 已退場，Telegram thread 的 canonical pointer 改成 `current_codex_thread_id`
- `SessionBinding` 已加入：
  - `current_codex_thread_id`
  - `tui_active_codex_thread_id`
  - `tui_session_adoption_pending`
  - `tui_session_adoption_prompt_message_id`
- threadBridge 會為每個 bound workspace 啟動共享的 `codex app-server` daemon，寫入 `./.threadbridge/state/app-server/current.json`
- Telegram bot 已從 per-turn `stdio://` child 改成連共享 websocket app-server
- `hcodex` 已改成受管 remote TUI 入口：
  - 透過 `resolve_hcodex_launch.py` 解析 workspace daemon 與 bound thread
  - 實際啟動 `codex --remote <ws-url> ...`
- `/attach_cli_session` 已從 command surface 移除
- topic title 已從 `.cli/.cli!/.attach` 收斂成 `busy/broken`

目前仍未完成：

- proxy-backed `hcodex` tracking
  - threadBridge 還無法攔截 TUI 內部的 `thread/resume` / `thread/start`
  - 因此 `tui_active_codex_thread_id` 還沒有被正式 runtime 更新
- remote TUI turn mirror
  - Telegram 還不能自動鏡像 shared TUI session 的 user / assistant 對話內容
- adoption flow
  - TUI 結束後的 `tui_session_adoption_prompt`
  - inline buttons
  - ignore prompt 後的 auto-adopt
- workspace-wide busy 尚未接入 remote TUI activity
  - 現在的 busy gate 仍主要來自 selected session snapshot 與既有 workspace status surface

## 現況定位

這份 Plan 已不再處理舊的 attach/viewer handoff 模型。

現在的正式方向是：

- Telegram 與本地 `hcodex` 共用同一個 workspace-scoped app-server daemon
- Telegram thread 只有一個 canonical continuity pointer：
  - `current_codex_thread_id`
- `tui_active_codex_thread_id` 是受管 TUI runtime 狀態，不是 canonical binding
- 是否把 TUI session 採納為 Telegram 當前 session，要由 adoption flow 決定

這表示：

- mirror 與 adopt 是兩件不同的事
- shared runtime 已經開始落地
- shared session adoption 還沒完成

## 已收斂的術語

- `current_codex_thread_id`
  - 代表這個 Telegram thread 目前正式採用的 Codex 對話
- `tui_active_codex_thread_id`
  - 代表這個 workspace 中，受管 `hcodex` 最近一次 `resume` 或 `new session` 使用的 Codex 對話
- `adoption`
  - 代表 TUI 結束後，Telegram 是否切換到 TUI session 繼續對話

## 已落地的 runtime 模型

### 1. Telegram 端

- `/bind_workspace` 會先確保 workspace runtime surface 與共享 daemon 存在
- Telegram 文字 turn / 圖片分析 / `/new` / `/reconnect_codex` 透過共享 daemon 執行
- `current_codex_thread_id` 是 Telegram 所有正常 turn 的唯一 thread pointer

### 2. 本地 `hcodex`

- workspace 內 source `./.threadbridge/shell/codex-sync.bash` 後可使用 `hcodex`
- `hcodex` 會：
  - 讀取 `./.threadbridge/state/app-server/current.json`
  - 讀取 bot-local binding
  - 預設執行 `codex --remote <ws-url> resume <current_codex_thread_id>`
- `hcodex --thread-key <key>` 可在同 workspace 綁定多個 Telegram threads 時消歧義

### 3. Title 與 UX

- title suffix 目前只保留：
  - `· busy`
  - `· broken`
- `/attach_cli_session`
  - 已不再是正式控制面
- `threadbridge_viewer`
  - 不再是正式 handoff UX 的一部分

## 尚未完成的關鍵缺口

### 1. TUI runtime 觀測

threadBridge 雖然已經把 `hcodex` 接到 shared daemon，但還沒有一層 proxy 或 observer 去精準知道：

- TUI 連到哪個 thread id
- TUI 是否在介面內切了 `new session`
- TUI 何時真正結束

所以目前 `tui_active_codex_thread_id` 只是資料模型準備完成，尚未進入正式 runtime 寫入路徑。

### 2. Mirror

產品目標已經固定為：

- 無論 `tui_active_codex_thread_id` 是否等於 `current_codex_thread_id`
- Telegram 都應該自動鏡像受管 TUI session 的對話內容

但這條 mirror path 目前尚未完成。

既有 `codex-sync` hooks 仍可提供一部分 legacy CLI transcript mirror，但那不是 shared remote TUI 的正式答案。

### 3. Adoption

當 `tui_active_codex_thread_id != current_codex_thread_id` 且 TUI 結束時，最終目標是：

- bot 發送 `tui_session_adoption_prompt`
- 提供：
  - `繼續 TUI 對話`
  - `恢復原對話`
- 若使用者忽略 prompt，下一條普通 Telegram 訊息自動採納 `tui_active_codex_thread_id`

這條 adoption flow 目前尚未實作。

## 與舊 Hook V1 的關係

[codex-cli-telegram-status-sync-hooks.md](/Volumes/Data/Github/threadBridge/docs/plan/codex-cli-telegram-status-sync-hooks.md) 現在應理解成：

- 保留中的兼容層
- 仍可提供：
  - workspace-level busy signal
  - legacy CLI transcript mirror
- 但它不再是 shared app-server TUI 主模型

它不能回答：

- remote TUI 目前實際附著到哪個 thread
- TUI 內部 `new session` 是否發生
- TUI 結束後是否需要 adoption prompt

## 下一步

1. 為 `hcodex` 補一個 threadBridge-owned websocket proxy，能觀察 `thread/resume`、`thread/start` 與連線關閉事件。
2. 讓 proxy 正式寫入 `tui_active_codex_thread_id`。
3. 為 remote TUI 加 mirror observer，把 TUI session 內容同步到 Telegram。
4. 補 `tui_session_adoption_prompt`、callback handling 與 ignore-prompt auto-adopt。
