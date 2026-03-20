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
  - 優先透過 threadBridge websocket proxy 啟動 `codex --remote <ws-url> ...`
- proxy-backed `hcodex` tracking 已落地：
  - threadBridge 會攔截 TUI 的 `thread/resume` / `thread/start`
  - `tui_active_codex_thread_id` 會隨 TUI `resume` 與 `new session` 更新
- remote TUI turn mirror 已落地：
  - Telegram 會自動鏡像受管 TUI session 的 user / assistant 對話內容
  - proxy 事件會被正式標成 `TUI`，不再混用舊 `CLI` 顯示語義
- adoption flow 已落地：
  - TUI 結束且 session 不同時，bot 會發 `tui_session_adoption_prompt`
  - inline buttons 可採納或恢復原對話
  - 忽略 prompt 後，下一條 Telegram 文字或圖片訊息會 auto-adopt
- workspace-wide busy 已接入 remote TUI activity：
  - 只要 `tui_active_codex_thread_id` 的 turn 正在跑，Telegram 會命中 busy gate
- `/attach_cli_session` 已從 command surface 移除
- topic title 已從 `.cli/.cli!/.attach` 收斂成 `busy/broken`
- `threadbridge_viewer` 與 attach-intent handoff plumbing 已從 runtime 中移除
- `hcodex` 已具備本地 self-heal：
  - 如果 `./.threadbridge/state/app-server/current.json` 指到 stale ws endpoint
  - `hcodex` 會先在本機補拉 shared daemon / TUI proxy，再啟動 `codex --remote`

目前仍未完成：

- shared runtime 的長壽命 owner 仍未收斂
- `/reconnect_codex` 目前會重寫 runtime state，但不能保證留下來的 ws endpoint 持續存活
- 舊 viewer/attach 流程的歷史文檔仍待清理或移入明確的 archive 區

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
- shared runtime 主模型已落地
- adoption 已成為正式 runtime 行為

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
- Telegram 正式 turn 路徑現在是 shared websocket app-server，不再是 per-turn `stdio://`

### 2. 本地 `hcodex`

- workspace 內直接執行 `./.threadbridge/bin/hcodex`
- `hcodex` 會：
  - 讀取 `./.threadbridge/state/app-server/current.json`
  - 讀取 bot-local binding
  - 預設執行 `codex --remote <ws-url>`
  - 若要接續既有 session，顯式使用 `hcodex resume <session-id>`
- `hcodex --thread-key <key>` 可在同 workspace 綁定多個 Telegram threads 時消歧義
- 若 `current.json` 已 stale，`hcodex` 會先做 self-heal，再把活的 endpoint 交給 `codex --remote`

### 3. Runtime Ownership 現況

- bot 端目前已經是 shared runtime 的 client，但還不是可靠的長壽命 owner
- `/reconnect_codex` 之後，`current.json` 可能仍指到 dead ws endpoint
- 所以目前的 operational reality 是：
  - Telegram turn 在 bot 成功 `ensure` 當下可正常走 shared websocket daemon
  - 本地 `hcodex` 依賴 self-heal 作為 fallback
  - `current.json` 本身暫時不能被視為「只要 bot 重寫過就一定長期有效」

### 4. Title 與 UX

- title suffix 目前只保留：
  - `· busy`
  - `· broken`
- `/attach_cli_session`
  - 已不再是正式控制面
- `threadbridge_viewer` / attach-intent handoff
  - 已不再存在於 runtime

## 剩餘缺口

### 1. Shared Runtime Owner 收斂

- 需要把 shared daemon / TUI proxy 的長壽命 owner 正式定下來
- 目前更合理的方向是：
  - `hcodex` self-heal 保留為本地 fallback
  - 後續由本機常駐進程接手 runtime ownership 與 health management

### 2. 文檔與術語收尾

runtime 已移除 viewer/attach handoff，但 repo 內仍有部分歷史文檔保留舊模型敘述，後續需要：

- 標註為 archive / historical
- 或整理後移出主要閱讀動線

## 與舊 Hook V1 的關係

[codex-cli-telegram-status-sync-hooks.md](/Volumes/Data/Github/threadBridge/docs/plan/codex-cli-telegram-status-sync-hooks.md) 現在應理解成：

- 已退役的舊 hook-based CLI sync 模型
- 只保留作為歷史參考，不再代表現行 runtime surface

它不能回答：

- remote TUI 目前實際附著到哪個 thread
- TUI 內部 `new session` 是否發生
- TUI 結束後是否需要 adoption prompt

## 下一步

1. 收斂 shared daemon / TUI proxy 的長壽命 owner 模型。
2. 清理或歸檔仍描述 viewer/attach handoff 的歷史文檔。
3. 視需要再把 shared-runtime status / event 面從目前形狀抽成更明確的主規格。
