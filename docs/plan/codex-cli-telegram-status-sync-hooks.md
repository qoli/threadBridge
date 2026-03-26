# Codex CLI / Telegram 狀態同步: Bash + Codex Hooks

## 目前進度

這份 Plan 的 `v1` 曾完成並落地到代碼，但現在屬於 `已退役`。

目前定位已改成：

- 歷史方案 / archive 參考
- 用來說明舊的 hook-based CLI sync 曾經如何工作
- 不再代表當前 runtime surface

目前對現況的意義：

- 它描述的是已被 shared app-server runtime 取代的舊模型
- 現在正式路徑是 shared daemon + `./.threadbridge/bin/hcodex` + `hcodex` ingress + adoption flow
- 文中提到的 `codex-sync.bash`、`codex_sync_event`、`codex_sync_notify`、`.codex/hooks.json` 已不再由現行 `/bind_workspace` 安裝

這份歷史方案當時的邊界：

- 只支持 Bash
- 只追蹤已 source wrapper 的本地 `codex`
- 不支持同一 workspace 的多個並發本地 CLI session
- 這份 `v1` 是 workspace-level busy signal，不是 session registry 或多 agent scheduler

## 背景

現在 `threadBridge` 只能感知由 Telegram bot 自己發起的 Codex turn。

如果使用者在同一個 workspace 裡本地直接跑 `codex`，Telegram 端看不到這個 session 的活躍狀態，也就無法：

- 在 topic title 上反映 `cli` / `bot` 的當前來源
- 阻止 Telegram 和本地 CLI 同時對同一 workspace 發出新 turn
- 統一表達 `idle`、`running`、`finalizing` 這類狀態

這會讓使用者誤以為 Telegram thread 是 idle，但實際上本地 Codex CLI 仍在進行中。

## 目標

為單一 workspace 建立一份共享狀態，讓：

- 本地 Bash `codex` 啟動與退出可以被 threadBridge 看見
- Codex turn 級別事件可以透過 hooks / notify 寫入共享狀態
- Telegram bot 也把自己發起的 turn 寫入同一份狀態
- Telegram topic title 能反映 `cli` / `bot` / `broken`
- Telegram 在 workspace busy 時阻止新的文字輸入、圖片分析與 session 變更命令

## 範圍

V1 只修改 `threadBridge`。

- 不修改上游 `codex`
- 不要求全局安裝 shell plugin
- 不自動改寫使用者的 `~/.bashrc`
- 只先支持 Bash

## 模型邊界

這份 `v1` 的核心模型是：

- 每個 workspace 只有一份共享快照 `current.json`
- 這份快照表達的是「這個 workspace 現在是否忙碌，以及忙碌來源是 `cli` 還是 `bot`」

這代表它是 workspace-level busy signal，不是 session-level registry。

它明確不處理：

- 同一 workspace 下多個本地 Codex CLI session 的並發追蹤
- 同一 workspace 下多個 agent 的衝突協調
- Telegram thread 和某一個特定 CLI session 的精確一對一映射
- 哪些 session 是只讀、哪些 session 會改文件的差異化策略

所以這層兼容 busy signal 的語義應理解成：

- 這個 workspace 目前被某個本地 Codex CLI 活躍使用
- Telegram 端應暫停新的 turn，以避免和本地工作撞車

而不是：

- threadBridge 已經完整理解這個 workspace 裡所有 Codex session 的拓撲
- threadBridge 已經支持同一 workspace 的多 agent 並發調度

## 設計

### 1. Workspace 內共享狀態

每個 bound workspace 安裝：

- `./.threadbridge/state/codex-sync/current.json`
- `./.threadbridge/state/codex-sync/events.jsonl`

`current.json` 作為最新快照，`events.jsonl` 作為追加式事件流。

### 2. Bash Hook 層

`/bind_workspace` 時安裝：

- `./.threadbridge/shell/codex-sync.bash`

使用者手動在該 workspace 執行：

```bash
source ./.threadbridge/shell/codex-sync.bash
```

這個 Bash snippet 會定義 workspace-scoped `codex()` wrapper：

- 在當前目錄位於該 workspace 時才攔截
- 啟動前寫入 `shell_process_started`
- 自動注入：
  - `-c features.codex_hooks=true`
  - `-c notify=["<workspace>/.threadbridge/bin/codex_sync_notify"]`
- 保留原參數與 exit code
- 退出後寫入 `shell_process_exited`

### 3. Codex Hook 層

workspace 內安裝由 threadBridge 管理的：

- `./.codex/hooks.json`

註冊：

- `SessionStart`
- `UserPromptSubmit`
- `Stop`

三個 hook 都寫到：

- `./.threadbridge/bin/codex_sync_event`

由它讀取 hook stdin JSON，轉成共享狀態事件。

### 4. Notify 層

workspace 內安裝：

- `./.threadbridge/bin/codex_sync_notify`

Codex 每次 turn 完成後，會把 legacy notify payload 作為最後一個 argv 傳進來。

這一層負責把 `agent-turn-complete` 轉成 `turn_completed`，更新共享快照。

### 5. Telegram 端整合

bot 啟動後跑 background watcher：

- 枚舉 active bound threads
- 按 workspace 聚合讀取 `current.json`
- 只在 title 真的變化時呼叫 `edit_forum_topic`

topic title 規則：

- 基底：thread title，否則 workspace basename，否則 `Unbound`
- suffix：
  - ` · busy`
  - ` · broken`

### 6. Busy Gate

如果共享 workspace 狀態不是 `idle`：

- 阻止新文字訊息直接進入 Codex
- 圖片仍保存，但不立即啟動分析
- 阻止 `/new`
- 阻止 `/reconnect_codex`
- 阻止對已綁定 thread 的再次 `/bind_workspace`

CLI busy 提示文案：

- 文字：
  - `Local Codex CLI is active in this workspace. Wait for it to finish before sending a new Telegram request.`
- 圖片：
  - `Image saved. Analysis will stay pending until local Codex CLI becomes idle.`

## 實作落點

- `rust/src/workspace.rs`
  - 安裝 hooks、shell snippet、status surface
- `tools/codex_sync.py`
  - 寫入 `current.json` / `events.jsonl`
- `rust/src/workspace_status.rs`
  - Rust 端讀寫 shared status
- `rust/src/telegram_runtime/status_sync.rs`
  - watcher、title rendering、busy message helper
- `rust/src/telegram_runtime/thread_flow.rs`
  - text / command busy gate
- `rust/src/telegram_runtime/media.rs`
  - image queue / analysis busy gate

## 限制

- 只支持 Bash
- 只追蹤經過已 source wrapper 的 `codex` 啟動
- 同一 workspace 的多個並發本地 CLI session 不作為 V1 目標
- `.codex/hooks.json` 由 threadBridge 接管
- `current.json` 是單一 workspace 快照，不是多 session 狀態表

## 完成條件

- `/bind_workspace` 後能在 workspace 看到完整 hook surface
- source Bash snippet 後，本地 `codex` 活躍會反映到 Telegram title
- Telegram 在 CLI / bot busy 時阻止新 turn
- bot 自己發起的文字與圖片分析也更新同一份共享狀態
