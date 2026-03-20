# Topic Title 狀態欄

## 目前進度

這份 Plan 已部分落地。

目前已實作：

- title 基底優先使用 thread title
- 若 thread title 缺失，回退到 workspace basename
- suffix 目前支持：
  - `· busy`
  - `· broken`
- background watcher 會在共享 workspace status 變化時更新 title
- threadBridge 管理的 topic 內，新的 rename service message 會 best-effort 清理

目前尚未實作：

- context ratio / ctx%
- adoption 相關額外 title 語義
- 更細緻的更新節流規格

## 現行語義

title 現在承載的是非常少量的 runtime state：

- `busy`
  - 當前 Telegram thread 的 `current_codex_thread_id` 在 session snapshot 中處於 turn busy
- `broken`
  - 目前 binding 已失效，需要 `/reconnect_codex` 或 `/new`

已退場的舊 suffix：

- `.cli`
- `.cli!`
- `.attach`

這些屬於舊 handoff / viewer 模型，不再是正式 title 語義。

## 渲染規則

目前格式是：

- `<thread-title> · busy`
- `<thread-title> · broken`
- `<thread-title> · busy · broken`

若 thread title 不存在，則改用 workspace basename。

## 資料來源

目前 `busy` 不是從本地 viewer / attach 狀態推導，而是從 selected current session 的 snapshot 推導：

- `current_codex_thread_id`
- `read_session_status(workspace, current_codex_thread_id)`

所以 title 目前對齊的是：

- Telegram 當前採用的 Codex session

而不是：

- 某個本地 CLI owner
- 某個 attach viewer 狀態

## 後續方向

之後若 TUI proxy 與 adoption 完成，title 還需要再決定是否承載：

- adoption pending
- alternate TUI session 正在 mirror
- context ratio

但目前不應過早把太多 runtime flag 塞進 title。
