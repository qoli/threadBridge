# Session 生命週期草稿

## 目前進度

這份文檔已部分落地，但仍不是完整主規格。

目前已實作：

- `/new_thread`
- `/bind_workspace <absolute-path>`
- `/new`
- `/reconnect_codex`
- `session-binding.json` 持久化 Telegram thread / workspace / Codex thread 關聯

目前尚未完成：

- 與 `runtime-state-machine` 的正式對齊
- 更清晰的 runtime 層抽象，而不是由 Telegram flow 直接承載主要生命週期

## 問題

`threadBridge` 最早的模型比較接近：

- Telegram thread 綁定到一個已經存在的 Codex session
- bot 只是負責接上那個 session

但在改成對接 Codex app-server 之後，這個前提已經不對了。

現在 bot 自己已經可以明確控制：

- 建立新的 Codex thread
- 把 Telegram thread 綁定到一個 workspace path
- 對同一個 workspace 重建一個新的 Codex thread
- 驗證目前記錄的 Codex thread 是否還能 resume

所以未來的 runtime 應該用 `new / bind / resume / reset` 來描述，而不是再用「接上一個本地已存在 session」的角度來理解。

## 方向

把 Codex thread 的生命週期，明確變成產品模型的一部分。

核心觀念：

- Telegram thread 是 UI 容器
- workspace binding 是本地目錄選擇
- Codex thread 是由 threadBridge 管理建立的 runtime 資源
- `session-binding.json` 只是 Telegram thread、workspace path、Codex `thread.id` 之間的對照關係

## 建議的心智模型

建議把術語固定成下面這幾層：

- `Telegram thread`
  - Telegram 裡的 topic / 討論串
- `Workspace binding`
  - 由 `/bind_workspace` 選定的真實本地目錄
- `Codex thread`
  - threadBridge 透過 app-server 建立的 Codex thread
- `Reset`
  - 放棄舊的 Codex continuity，為同一個 workspace 建立新的 Codex thread
- `Reconnect`
  - 驗證目前保存的 Codex thread 是否仍然可以 resume，且 `cwd` 是否仍然對得上

應該避免的說法：

- 把 `data/` 描述成可以恢復 Codex runtime 的來源
- 把主要流程描述成「綁定一個現成 session」
- 把 `data/` 誤認成真正的 workspace

## 預期的使用流程

### 新 Thread

1. 使用者透過 `/new_thread` 建立 Telegram thread
2. bot 只建立 bot-local metadata
3. 這時候還沒有 Codex thread
4. 使用者透過 `/bind_workspace <absolute-path>` 綁定 workspace
5. bot 安裝 runtime appendix、建立新的 Codex thread、materialize 它，然後寫入 binding

### 一般延續對話

1. 使用者在已綁定的 Telegram thread 裡發訊息
2. bot resume 目前保存的 Codex thread
3. bot 在綁定的 workspace 裡開始新 turn
4. bot 串流 preview，最後把結果送回 Telegram

### Reset

1. 目前的 Codex thread 壞掉、過時，或不再適合繼續
2. 使用者執行 `/new`
3. bot 對同一個 workspace 建立新的 Codex thread
4. 舊 continuity 直接放棄

### Reconnect

1. 使用者或 bot 懷疑目前 binding 已經失效
2. 使用者執行 `/reconnect_codex`
3. bot 驗證 resume 是否成功，並確認 `thread.cwd` 是否仍然等於保存的 `workspace_cwd`
4. 成功就清除 broken 狀態，失敗就要求 reset

## 對實作的影響

### 指令面

目前的指令面已經接近這個方向：

- `/bind_workspace`
- `/new`
- `/reconnect_codex`

可能的後續增強：

- 一個更清楚的 `/status`
- 顯示目前 workspace path 與 Codex thread id
- 提供明確的「替換目前 Codex thread，但不改 workspace」操作

### 資料模型

`session-binding.json` 應該保持最小且明確：

- `workspace_cwd`
- `codex_thread_id`
- 驗證時間
- broken 狀態相關欄位

它不應該暗示 threadBridge 可以只靠 `data/` 自己恢復 Codex runtime。

### 使用者文案

使用者看見的說法應該逐步固定成：

- 綁定 workspace
- 建立新的 Codex session
- 重新連接目前的 Codex session
- 目前的 workspace binding

而不是：

- 從本地資料恢復 session
- 綁定現有的本地 session

## 開放問題

- `/new_thread` 之後，未來要不要支援直接帶入 workspace path？
- 要不要把目前的 Codex `thread.id` 暴露在 `/status` 裡？
- `/new` 之後，bot 端要不要保留某些本地摘要或狀態？
- 當錯誤是 `no rollout found for thread id ...` 時，要不要直接把 `/new` 作為主建議？

## 建議的下一步

1. 增加一個 thread 狀態指令，顯示 workspace path、目前 binding 狀態、Codex thread 是否健康。
2. 收斂所有使用者文案，統一用 `workspace binding` 和 `Codex thread` 的語言。
3. 收斂 `/new` 的文案與 Telegram UX，讓 fresh session 語意固定下來。
