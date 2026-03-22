# Session 生命週期

## 目前進度

這份文檔已部分落地。

目前已實作：

- `/add_workspace <absolute-path>`
- `/new_session`
- `/repair_session`
- `session-binding.json` 持久化 Telegram thread / workspace / Codex thread 關聯
- canonical pointer 已收斂到 `current_codex_thread_id`
- 本地 management API / desktop runtime 已開始承接等價的 create-bind / reconnect control flow

目前尚未完成：

- 與 `runtime-state-machine` 的完整對齊

## 核心模型

應固定用下面這幾層理解：

- `Telegram thread`
  - Telegram 裡的 topic / 討論串
- `Workspace binding`
  - 由 `/add_workspace` 或本地 management API 選定的真實本地目錄
- `Codex thread`
  - 由 threadBridge 透過 app-server 建立與續接的 Codex `thread.id`
- `current_codex_thread_id`
  - 這個 Telegram thread 目前正式採用的 Codex 對話
- `tui_active_codex_thread_id`
  - 受管本地 TUI 最近一次使用的 Codex 對話

## 指令語義

### `/add_workspace`

- 綁定 workspace path
- 建立或重用 Telegram workspace thread
- 安裝 runtime appendix 與 `.threadbridge/`
- 確保共享 app-server daemon 可用
- 建立 fresh Codex thread
- 將該 `thread.id` 寫進 `current_codex_thread_id`

本地管理面上的 `pick_and_add_workspace` / 等價 control action，語義上也屬於同一條 lifecycle。

### 一般延續對話

- Telegram 在已綁定 thread 收到文字或圖片分析請求
- 使用 `current_codex_thread_id`
- 透過共享 workspace daemon 對同一個 Codex thread 發 turn

### `/new_session`

- 對同一個 workspace 建立 fresh Codex thread
- 原子替換 `current_codex_thread_id`
- 清除殘留的 adoption 狀態

### `/repair_session`

- 驗證 `current_codex_thread_id` 是否仍能 `thread/read`
- 驗證返回的 `cwd` 是否仍等於保存的 `workspace_cwd`
- 成功則清除 broken 狀態
- 失敗則保留原 binding，但標成 broken，要求 `/new_session` 或重試
- `/repair_session` 對 Telegram 來說是主要 continuity repair 命令
- 本地 management API 目前也提供等價的 reconnect control action
- 但現階段不能把它理解成「保證 shared ws endpoint 之後持續存活」
- 如果 `current.json` 指到 stale endpoint，本地 `hcodex` 不會再 self-heal，而是要求 desktop runtime repair runtime

## `session-binding.json`

目前應理解成：

- 最小但明確的 binding 文件
- source of truth 是 Telegram thread 對 workspace 與 current Codex thread 的綁定

現行欄位重點：

- `workspace_cwd`
- `current_codex_thread_id`
- `last_verified_at`
- `session_broken`
- `session_broken_at`
- `session_broken_reason`
- `tui_active_codex_thread_id`
- `tui_session_adoption_pending`
- `tui_session_adoption_prompt_message_id`

舊欄位：

- `selected_session_id`
- `codex_thread_id`

已經進入兼容讀取、統一寫回新欄位的過渡狀態。

## `/new_session` 與 TUI 的關係

這是目前最容易混淆的點。

- `/new_session`
  - 永遠代表 Telegram thread 的 canonical continuity 切換
  - 也就是替換 `current_codex_thread_id`
- TUI 內部的 `new session`
  - 最終目標不是立刻覆蓋 `current_codex_thread_id`
  - 而是先更新 `tui_active_codex_thread_id`
  - 等 TUI 結束後再走 adoption flow

所以現在的正確語義是：

- `current_codex_thread_id` 是 Telegram continuity
- `tui_active_codex_thread_id` 是 TUI runtime state
- 兩者可以暫時不同

## 後續工作

1. 把 `session-lifecycle`、`session-level-cli-telegram-sync`、`runtime-state-machine` 的狀態語義完全收斂。
2. 把 `/repair_session` / reconnect control、shared runtime state、實際 runtime owner 的語義收斂成單一主模型。
3. 清理仍描述舊 viewer/attach handoff 的歷史文檔。
