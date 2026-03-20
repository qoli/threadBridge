# Session 生命週期

## 目前進度

這份文檔已部分落地。

目前已實作：

- `/new_thread`
- `/bind_workspace <absolute-path>`
- `/new`
- `/reconnect_codex`
- `session-binding.json` 持久化 Telegram thread / workspace / Codex thread 關聯
- canonical pointer 已收斂到 `current_codex_thread_id`

目前尚未完成：

- `tui_active_codex_thread_id` 的正式 runtime 更新
- TUI adoption flow
- 與 `runtime-state-machine` 的完整對齊

## 核心模型

應固定用下面這幾層理解：

- `Telegram thread`
  - Telegram 裡的 topic / 討論串
- `Workspace binding`
  - 由 `/bind_workspace` 選定的真實本地目錄
- `Codex thread`
  - 由 threadBridge 透過 app-server 建立與續接的 Codex `thread.id`
- `current_codex_thread_id`
  - 這個 Telegram thread 目前正式採用的 Codex 對話
- `tui_active_codex_thread_id`
  - 受管本地 TUI 最近一次使用的 Codex 對話

## 指令語義

### `/new_thread`

- 只建立 Telegram thread 與 bot-local metadata
- 不建立 Codex thread
- 不綁定 workspace

### `/bind_workspace`

- 綁定 workspace path
- 安裝 runtime appendix 與 `.threadbridge/`
- 確保共享 app-server daemon 可用
- 建立 fresh Codex thread
- 將該 `thread.id` 寫進 `current_codex_thread_id`

### 一般延續對話

- Telegram 在已綁定 thread 收到文字或圖片分析請求
- 使用 `current_codex_thread_id`
- 透過共享 workspace daemon 對同一個 Codex thread 發 turn

### `/new`

- 對同一個 workspace 建立 fresh Codex thread
- 原子替換 `current_codex_thread_id`
- 清除殘留的 adoption 狀態

### `/reconnect_codex`

- 驗證 `current_codex_thread_id` 是否仍能 `thread/read`
- 驗證返回的 `cwd` 是否仍等於保存的 `workspace_cwd`
- 成功則清除 broken 狀態
- 失敗則保留原 binding，但標成 broken，要求 `/new` 或重試

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

## `/new` 與 TUI 的關係

這是目前最容易混淆的點。

- `/new`
  - 永遠代表 Telegram thread 的 canonical continuity 切換
  - 也就是替換 `current_codex_thread_id`
- TUI 內部的 `new session`
  - 最終目標不是立刻覆蓋 `current_codex_thread_id`
  - 而是先更新 `tui_active_codex_thread_id`
  - 等 TUI 結束後再走 adoption flow

所以未來的正確語義是：

- `current_codex_thread_id` 是 Telegram continuity
- `tui_active_codex_thread_id` 是 TUI runtime state
- 兩者可以暫時不同

## 後續工作

1. 為 shared remote TUI 補正式 thread tracking。
2. 讓 `tui_active_codex_thread_id` 不再只是資料模型欄位。
3. 完成 TUI adoption prompt 與 auto-adopt。
4. 把 `session-lifecycle`、`session-level-cli-telegram-sync`、`runtime-state-machine` 的狀態語義完全收斂。
