# Telegram 斜線命令

這份文件是 threadBridge Telegram bot 斜線命令的維護中參考文檔。

實作來源：

- 命令註冊：`rust/src/telegram_runtime/mod.rs`
- 命令行為：`rust/src/telegram_runtime/thread_flow.rs`

運作模型：

- 私聊主對話是控制台 chat。
- 每個受管 workspace 都有自己的 Telegram topic/thread。
- 大多數 workspace 生命週期與 session 命令只能在 workspace thread 內使用。
- 在 workspace thread 裡發送普通非斜線文字訊息，會延續目前保存的 Codex session。
- Bot 也接受帶 bot 名稱的限定命令，例如 `/launch_local_session@threadbridge_bot continue_current`。

## 控制台 Chat 命令

這些命令應該在與 bot 的主私聊中使用。

| 命令 | 參數 | 用途 | 說明 |
| --- | --- | --- | --- |
| `/start` | 無 | 初始化或重新進入控制台。 | 在控制台 chat 中會記錄初始化事件，並提示使用者先加入 workspace。 |
| `/add_workspace` | `<absolute-path>` | 新增 workspace，並建立或重用對應的 Telegram thread。 | 只能在控制台 chat 使用。用法：`/add_workspace <absolute-path>`。 |
| `/restore_workspace` | 無 | 顯示已封存的 workspace，並互動式恢復。 | 只能在控制台 chat 使用。這只恢復 Telegram / 本地狀態，不會單獨恢復 Codex 連續性。 |

## Workspace Thread 命令

這些命令應該在某個受管 workspace 的 topic/thread 中使用。

| 命令 | 參數 | 用途 | 說明 |
| --- | --- | --- | --- |
| `/start` | 無 | 顯示 workspace thread 的命令入口說明。 | 在 workspace thread 中會提示主要生命週期命令。 |
| `/start_fresh_session` | 無 | 為目前 workspace 啟動一個全新的 Codex session。 | 在控制台 chat 中會被拒絕；如果 workspace 已封存或當前正忙，也會被拒絕。 |
| `/repair_session_binding` | 無 | 重新驗證或修復目前 workspace 保存的 Codex session 連續性。 | 在控制台 chat 中會被拒絕；如果無法驗證連續性，bot 會提示重試或改用 `/start_fresh_session`。 |
| `/workspace_info` | 無 | 顯示 thread key、workspace 路徑、execution mode、session id、lifecycle state、binding state、run state 與 gate state。 | 用於排查 workspace / session 狀態。 |
| `/rename_workspace` | 無 | 根據目前 Codex session 歷史生成新的 Telegram topic 標題。 | 需要可用的已綁定 session，且當前不能有 busy gate。 |
| `/archive_workspace` | 無 | 封存目前的 workspace thread。 | 會在可能時刪除 Telegram forum topic，然後封存本地 thread 狀態。 |
| `/launch_local_session` | `new`、`continue_current` 或 `resume <session_id>` | 為這個 workspace 啟動受管的本地 `hcodex` 終端 session。 | 用法：`/launch_local_session new`、`/launch_local_session continue_current`、`/launch_local_session resume <session_id>`。 |
| `/get_workspace_execution_mode` | 無 | 查看 workspace 級別的 execution mode。 | 會顯示 workspace mode、目前 session mode、approval policy、sandbox policy 與 drift 狀態。 |
| `/set_workspace_execution_mode` | `full_auto` / `full-auto` / `yolo` | 修改 workspace 級別的 execution mode。 | 用法：`/set_workspace_execution_mode full_auto` 或 `/set_workspace_execution_mode yolo`。 |
| `/get_running_input_policy` | 無 | 查看 running 狀態下普通文字訊息的處理策略。 | 顯示目前策略，以及可用的 `reject`、`queue`、`steer` 選項。 |
| `/set_running_input_policy` | `reject`、`queue` 或 `steer` | 修改 running 狀態下普通文字訊息的處理策略。 | `reject` 會拒絕插入；`queue` 會排到目前 turn 完成後執行；`steer` 會透過 Codex `turn/steer` 注入目前 turn。 |
| `/sessions` | 無 | 列出此 workspace 最近的 working sessions。 | 顯示 session id、是否為 current、run status、record 數、tool 數、是否有 final reply，以及來源。 |
| `/session_log` | `<session_id>` | 顯示某個 working session 的最近記錄。 | 用法：`/session_log <session_id>`。 |
| `/stop` | 無 | 中斷目前 workspace 正在執行中的 turn。 | 只有在存在活動 turn 且 turn id 可用時才能生效。 |
| `/plan_mode` | 無 | 將目前 workspace thread 切換到 Plan collaboration mode。 | 在控制台 chat 中會被拒絕；會把 collaboration mode 持久化到 workspace binding。 |
| `/default_mode` | 無 | 將目前 workspace thread 切回 Default collaboration mode。 | 在控制台 chat 中會被拒絕；會把 collaboration mode 持久化到 workspace binding。 |

## 使用備註

- 如果命令發送到了錯誤的位置，bot 會回覆正確的使用範圍，例如提示要在 workspace thread 裡用，或要在主私聊裡用。
- 已封存的 workspace 在恢復之前，會拒絕大多數 workspace-thread 命令。
- 正在忙碌的 workspace 會拒絕與當前執行中 turn 衝突的命令。
- `execution_mode` 是 workspace 級別狀態，保存在 `.threadbridge/state/workspace-config.json`。
- `launch_local_session` 使用受管的 `./.threadbridge/bin/hcodex` 路徑，並遵循該 workspace 的 execution mode。

## 建議操作流程

1. 先在私聊中使用 `/start`。
2. 用 `/add_workspace <absolute-path>` 新增 workspace。
3. 之後主要在該 workspace thread 中直接發送普通文字訊息來工作。
4. 當 session 連續性看起來不對時，使用 `/repair_session_binding`。
5. 當你想在同一個 workspace 上重開一個乾淨的 Codex session 時，使用 `/start_fresh_session`。
6. 當你想切到本地受管 `hcodex` TUI 時，使用 `/launch_local_session ...`。
