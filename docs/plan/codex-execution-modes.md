# Codex 執行模式草稿

## 目前進度

這份文檔目前仍是純草稿，尚未開始實作。

目前代碼狀態：

- workspace app-server thread start 仍固定使用：
  - `approvalPolicy = never`
  - `sandbox = danger-full-access`
- 這代表 threadBridge 目前只有一種近似固定的執行模式，還沒有正式的 session execution profile 概念
- management API 的 `launch-config` 目前已提供 `new` / `continue current` / recent-session resume 等 `hcodex` 啟動命令，但尚未暴露 execution mode
- Telegram turn、背景執行、busy gate、mirror 等流程目前也都建立在這個固定模式上

## 問題

如果 threadBridge 之後要更完整地承接本地 Codex runtime，那它不能永遠只假設單一執行模式。

目前缺的是一套明確語義，回答：

- threadBridge 是否支持不同的 Codex execution mode
- 所謂 `yolo mode` 在這個產品裡到底代表什麼
- 哪些 surface 可以啟動 `yolo mode`
- execution mode 是 workspace 預設、session 屬性，還是一次性的 launch option

如果這些不先定清楚，後面就很容易出現：

- Telegram、desktop、`hcodex` 各自帶不同的 mode 假設
- launch config、runtime protocol、session continuity 彼此不一致
- 使用者以為只是「更激進一點的執行」，實際上卻碰到 authority、repair、mirror、審計語義都改變

## 方向

先把 `Codex execution mode` 定義成 runtime core 的正式概念，而不是 Telegram 或 `hcodex` 自己加的局部開關。

比較合理的方向是：

- threadBridge 支持顯式的 execution profile
- `yolo mode` 只是其中一種 profile，而不是沒有邊界的隱含行為
- execution mode 應由 runtime / management API 對外暴露
- Telegram 是否允許直接進入 `yolo mode`，應晚於本地 owner / management surface 的收斂

## `yolo mode` 的暫定語義

在這個項目裡，`yolo mode` 不應只是一句「更放手讓 Codex 自己做」。

至少要明確涵蓋：

- approval behavior
  - 是否允許自動通過更多 action
- tool autonomy
  - Codex 是否被允許更自主地連續使用 tool / command
- operator expectation
  - 使用者是否預期較少確認、較強自動推進
- audit surface
  - 過程事件、Plan / Tool 文本、最終 summary 是否需要更強的可見性

換句話說，`yolo mode` 比較像：

- 一種 execution contract

而不是：

- 單一布林值

## 建議的 execution profile

v1 不需要一開始就做很多模式，但至少可以先把語義切成：

### 模式 A：`guarded`

- 現有預設模式的延續
- 適合 Telegram 背景 turn 與一般遠端使用
- 行為以穩定、可預測、低意外為優先

### 模式 B：`yolo`

- 目標是讓本地互動或 owner-managed 啟動時，Codex 具備更高自動推進能力
- 比 `guarded` 更強調少打斷、少顯式確認、快速完成任務
- 但它仍應是顯式選擇，不應默默套到所有 workspace

如果之後需要，也可以再補：

- `custom`
  - 允許未來掛更多 provider / policy 組合

但 v1 不必先做。

## 建議的資料模型

這份 plan 先不綁死最終欄位名，但至少應正式承認下面幾類資料：

- `execution_mode`
  - 例如 `guarded` / `yolo`
- `approval_policy`
  - 對應實際傳給 Codex runtime 的 policy
- `sandbox_policy`
  - 對應實際 sandbox 模式
- `mode_source`
  - 例如 `workspace_default` / `launch_override` / `session_inherited`

比較合理的責任是：

- workspace 可保存預設 execution mode
- 每次 launch 可以選擇覆蓋
- session binding / runtime view 至少能讀到目前 session 採用的 mode

## 與 continuity 的關係

`yolo mode` 不能只被理解成 UI 選項，因為它可能影響同一個 Codex session 的可預期性。

至少要回答：

- session 一旦以 `yolo` 啟動，之後 Telegram reconnect 是否仍沿用同一 mode
- adoption 後，Telegram 是否只作 viewer / sender，而不改變原 session mode
- `current_codex_thread_id` 是否需要伴隨 mode 一起被視為 continuity 的一部分

也就是說，continuity 不只可能是 `thread_id`，還可能包含：

- `thread_id + execution contract`

## 與 owner 收斂的關係

這份 plan 和 owner convergence 直接相關。

原因是：

- `yolo mode` 本質上會放大 runtime authority 的問題
- 如果 Telegram、desktop runtime、`hcodex` 都能各自決定 mode，owner 邊界會更混亂
- 因此 execution mode 應優先掛在 owner / management API 上，而不是先做 Telegram slash command

比較合理的順序是：

1. 先把 owner authority 收斂。
2. 再讓 management API / desktop runtime 暴露 execution mode。
3. 最後才決定 Telegram 是否允許直接切換或僅展示目前 mode。

## 對管理面的影響

如果 execution mode 進入正式模型，management API / launch config 至少應能表達：

- workspace 預設 mode
- 目前 active session mode
- 啟動新 `hcodex` session 時可選的 mode
- 哪些 mode 目前可用

這表示現在的 `launch-config` 之後可能不只回傳命令字串，還要帶：

- launch profile metadata
- 預設 mode
- resume 時是否允許切 mode

## 對 mirror / observability 的影響

如果引入 `yolo mode`，過程可見性的重要性會更高。

原因是：

- 當 Codex 被允許更自主推進時，使用者更需要知道它在 Plan / Tool 階段做了什麼
- 這會直接加強目前已知的 mirror 缺口：尚未完整承接 Plan / Tool 文本

所以這份 plan 應與下面兩件事聯動：

- process transcript event
- observability / runtime event stream

## 與其他計劃的關係

- [session-level-mirror-and-readiness.md](/Volumes/Data/Github/threadBridge/docs/plan/session-level-mirror-and-readiness.md)
  - execution mode 會影響 `hcodex`、adoption、continuity 與 mirror 的語義
- [runtime-protocol.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-protocol.md)
  - execution mode、launch profile、active session mode 之後應進入 view / action 模型
- [runtime-transport-abstraction.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-transport-abstraction.md)
  - execution mode 屬於 core runtime 語義，不應先做成 Telegram-only 開關
- [telegram-adapter-migration.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-adapter-migration.md)
  - Telegram 是否能直接切 `yolo mode`，應晚於 adapter 邊界收斂
- [llm-guidance-and-goals.md](/Volumes/Data/Github/threadBridge/docs/plan/llm-guidance-and-goals.md)
  - 若之後有 secondary LLM guidance，execution mode 也可能影響它對 Codex 的互動策略

## 開放問題

- `yolo mode` 的 v1 是否只允許本地 `hcodex` / desktop runtime 啟動？
- Telegram 是否只能看見 mode，不能直接切 mode？
- execution mode 應該是 workspace 預設、session 屬性，還是兩者都有？
- resume 既有 session 時，是否允許切換 mode，還是必須沿用原 mode？
- `yolo mode` 是否需要更強的 event / audit retention？
- 如果 Codex runtime 未來支持更多 approval / sandbox 組合，threadBridge 要暴露「原始組合」還是只暴露高階 mode？

## 建議的下一步

1. 先在代碼與文檔裡承認 `execution mode` 是正式問題域，不再只把當前模式寫死當作永遠預設。
2. 把 `guarded` / `yolo` 做成最小 profile 草稿，先不要一開始暴露太多模式。
3. 先把 mode 掛進 management / owner surface，再考慮 Telegram command 或 UI。
4. 把 active session mode 與 process transcript / observability 的關聯補進 `runtime-protocol`。
