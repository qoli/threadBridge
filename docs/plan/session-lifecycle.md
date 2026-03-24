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
- `binding_status` / `run_status` 已開始透過 shared resolver 與 `runtime-state-machine` 對齊
- Telegram thread 內的一般輸入與 session-control gate 已開始直接讀 canonical state，而不是各自重寫 archived / broken / running 判定

目前尚未完成：

- 與 `runtime-state-machine` 的完整 API / 文檔收斂

目前新增記錄的一個 Telegram adapter 相關想法是：

- 在現行 `Telegram thread = 工作 thread` 模型下，`main chat` 已更接近 control 面板
- 這使得使用者不一定適合直接在 `main chat` 做普通工作輸入
- 因此後續可評估一種 `forwarded input` 模式：
  - 允許使用者把 `main chat` 裡的轉發訊息投遞到某個目標 workspace thread
  - 讓 `main chat` 維持 control surface，同時保留較順手的輸入入口
- 另外也應記錄一條獨立的 Telegram desktop launch control：
  - 允許 Telegram slash command 觸發 desktop endpoint 的 `launch new` / `launch current` / `launch resume`
  - 但它不應重寫 `/new_session` 的語義，也不應直接覆蓋 `current_codex_thread_id`

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
- 這條命令的語義是 Telegram canonical continuity mutation，不是單純啟動本地桌面入口

### `/repair_session`

- 驗證 `current_codex_thread_id` 是否仍能 `thread/read`
- 驗證返回的 `cwd` 是否仍等於保存的 `workspace_cwd`
- 成功則清除 broken 狀態
- 失敗則保留原 binding，但標成 broken，要求 `/new_session` 或重試
- `/repair_session` 對 Telegram 來說是主要 continuity repair 命令
- 本地 management API 目前也提供等價的 reconnect control action
- 但現階段不能把它理解成「保證 shared ws endpoint 之後持續存活」
- 如果 `current.json` 指到 stale endpoint，本地 `hcodex` 不會再 self-heal，而是要求 desktop runtime repair runtime

### Telegram desktop launch control

- 這是一條和 `/new_session`、`/repair_session` 分離的 control surface
- 它的責任是從 Telegram 觸發 desktop runtime 已存在的本地 launch action
- 近期較合理的形狀是單獨的 slash command，而不是把「開桌面 session」塞進 `/new_session`
- 它可以承接：
  - `launch new`
  - `launch current`
  - `launch resume <session_id>`
- 它不應直接改寫：
  - `current_codex_thread_id`
  - `tui_active_codex_thread_id`
  - adoption 狀態
- 換句話說，Telegram desktop launch 只是在 Telegram 上暴露 owner/runtime 已授權的本地 launch control；真正的 continuity adoption 仍應沿用既有 TUI / adoption flow

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
- Telegram desktop launch command
  - 只代表「替 desktop endpoint 觸發受管本地入口」
  - 不等於 canonical continuity 已切換
- TUI 內部的 `new session`
  - 最終目標不是立刻覆蓋 `current_codex_thread_id`
  - 而是先更新 `tui_active_codex_thread_id`
  - 等 TUI 結束後再走 adoption flow

所以現在的正確語義是：

- `current_codex_thread_id` 是 Telegram continuity
- `tui_active_codex_thread_id` 是 TUI runtime state
- 兩者可以暫時不同

## 後續工作

1. 把 `session-lifecycle`、`session-level-mirror-and-readiness`、`runtime-state-machine` 的狀態語義完全收斂。
2. 把 `/repair_session` / reconnect control、shared runtime state、實際 runtime owner 的語義收斂成單一主模型。
3. 清理仍描述舊 viewer/attach handoff 的歷史文檔。
