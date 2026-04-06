# Codex 執行模式

## 目前進度

這份文檔已從純草稿進入「部分落地」。

目前代碼裡已經有的狀態：

- `ExecutionMode` 已成為正式 runtime 型別：
  - `full_auto`
  - `yolo`
- workspace-local execution mode config 已落地：
  - `./.threadbridge/state/workspace-config.json`
- workspace 預設 mode 已落地，且預設值為 `full_auto`
- v1 語義已對齊 Codex 現有 profile：
  - `full_auto`
    - `approvalPolicy = on-request`
    - `sandbox = workspace-write`
  - `yolo`
    - `approvalPolicy = never`
    - `sandbox = danger-full-access`
- 新 session 與 resume 路徑已開始按 workspace mode 收斂：
  - Telegram fresh binding
  - Telegram text turn
  - Telegram image analysis turn
  - local management bind flow
  - `hcodex` new / continue current / resume launch commands
- `session-binding.json` 已開始持久化目前 session 的 execution snapshot：
  - `current_execution_mode`
  - `current_approval_policy`
  - `current_sandbox_policy`
- workspace recent session history 已開始記錄 `execution_mode`
- management API 與 web 管理面已開始暴露：
  - workspace mode
  - current session mode
  - `mode_drift`
  - mode-aware launch commands
- Telegram adapter 已提供 execution mode command surface：
  - `/get_workspace_execution_mode`
  - `/set_workspace_execution_mode`
  - 可直接查看目前 workspace mode / current session mode / `mode_drift`
  - 可直接寫回 workspace-local execution mode config
- tray workspace label 已改成顯示 workspace execution mode，而不是 `ready/degraded`

目前仍未收斂的部分：

- v1 仍只支持 `full_auto` / `yolo` 兩種高階 mode
- mode 與更完整 observability / audit retention 的關聯仍未 formalize
- mode 是否允許未來出現 session-level override，仍是開放問題

目前新增記錄的產品想法是：

- Telegram 雖然已能直接設定 execution mode，但這條 control surface 的 naming / owner 邊界仍需更正式定義
- 但 Telegram user-facing execution mode 文案是否沿用 `full_auto / yolo`，目前仍未定案
- `execution mode` 與 `Codex 工作模型` 應明確視為兩個不同控制面，不應混成同一個切換器
- `threadBridge` 長期應支持自定義 Codex config，但它也應屬於與 execution mode 分離的獨立 control surface，而不是把任意 config 直接塞進 `full_auto / yolo`
- 另外要明確記錄：Telegram 的 `Plan mode / 普通模式` 切換已作為 collaboration mode command surface 落地，不屬於 execution mode
- 換句話說，現有 `/plan_mode` / `/default_mode` 應視為 collaboration mode control surface，不應掛進這份 execution mode 語義

## 問題

如果 `threadBridge` 要同時支撐：

- Telegram adapter
- desktop runtime owner
- 本地 `hcodex`
- browser management UI

那 execution mode 不能再只是某個 surface 私下帶的 CLI flag。

它需要是一個正式 runtime contract，回答：

- 哪個 workspace 預設使用哪個 mode
- 目前 active session 實際跑在哪個 mode
- mode 改變後，既有 session 如何收斂
- launch / resume / Telegram turn 是否遵守同一套 mode 假設

如果這些不固定，很容易出現：

- Telegram、desktop、`hcodex` 各自帶不同 mode
- 同一個 `thread_id` 在不同 surface 下被理解成不同 execution contract
- UI 以為切的是「偏好」，runtime 實際改的是 approval / sandbox 事實

## 已確認的 v1 方向

目前代碼已經實作並驗證的 v1 決策是：

- execution mode 掛在 workspace，而不是 Telegram thread 單次請求
- workspace mode 是 sticky default
- 既有 session 不強制重建；在下一次 turn 或 resume 時原地覆蓋到 workspace mode
- `hcodex` 與 Telegram 走同一套 mode 收斂語義
- mode 已對外暴露在 management API / owner-facing surface，Telegram 也已有 adapter command，但它寫回的仍是同一份 workspace/runtime contract

換句話說，現在的正式模型是：

- workspace 有一個目標 execution contract
- active session 有一個目前 execution snapshot
- 兩者不一致時，以 workspace mode 為收斂目標

但這不代表 execution mode 應承擔所有 Codex 啟動設定。

近期新增記錄的一個產品方向是：

- `threadBridge` 應支持自定義 Codex config
- 這類 config 應被理解為另一條 `Codex 工作模型 / Codex config` control surface
- 它可能涵蓋 model/profile/config file/額外啟動參數等設定，但不應回頭污染 execution mode 本身的高階語義
- 無論最終由 management UI、Telegram、還是其他 surface 暴露，都應先收斂成同一份 runtime contract，而不是讓每個入口各自偷帶 CLI flags

但近期也新增一個需要在後續規格中明確處理的方向：

- Telegram 已不再只是 mode consumer
- 但 Telegram 這條 control surface 仍應被理解為 owner/runtime 已授權的 adapter surface
- 也就是說，現有 Telegram mode 切換已寫回同一份 workspace/runtime contract，而不是私下帶 CLI flag

## v1 資料模型

目前實際落地的資料模型已至少包含：

- `execution_mode`
  - `full_auto | yolo`
- `approval_policy`
- `sandbox_policy`

已落地的主要 artifact / view：

- workspace-local config
  - `./.threadbridge/state/workspace-config.json`
- session binding fields
  - `current_execution_mode`
  - `current_approval_policy`
  - `current_sandbox_policy`
- recent session history
  - `execution_mode`
- management API views
  - `workspace_execution_mode`
  - `current_execution_mode`
  - `current_approval_policy`
  - `current_sandbox_policy`
  - `mode_drift`

v1 沒有正式落地 `mode_source` 或 arbitrary launch override。

## 與 continuity 的關係

這份 plan 已經不再是假設「continuity 只等於 `thread_id`」。

目前實作已開始承認：

- `thread_id` 是 continuity 的主要 identity
- `execution_mode` / approval / sandbox 是該 session 的有效 contract
- 若 workspace mode 改變，舊 session 仍可沿用 `thread_id`
- 但下一次 turn / resume 會重新施加 workspace mode，讓 execution contract 收斂

因此現行模型比較接近：

- `thread_id` 保持 continuity
- execution contract 可在 owner/runtime 控制下被原地更新

## 與 owner 收斂的關係

這份 plan 之所以先掛在 owner / management surface，而不是 Telegram command，上游原因仍然成立：

- execution mode 會直接影響 approval 與 sandbox
- 它本質上屬於 runtime authority，而不是 adapter 文案
- 如果每個 surface 都能各自切 mode，owner 邊界會重新變混亂

目前代碼已經遵守這個順序：

1. execution mode 先進入 core runtime 型別與 repository/session model
2. 再進入 management API、launch-config、web UI 與 tray 表達
3. Telegram 已提供 mode command surface，但它操作的仍是同一份 workspace-level contract，而不是 Telegram 私有狀態

這裡要再補一個近期產品方向：

- 之後若 Telegram 真的暴露 mode 切換，它也應被理解為 owner/runtime 已授權的 control surface
- 不應回退成「Telegram 自己決定 approval / sandbox」的舊邏輯

## 對管理面的影響

這部分已經有 v1 實作，不再只是預期方向。

management API / launch-config 現在已開始表達：

- workspace 預設 mode
- current session mode
- current approval / sandbox policy
- `mode_drift`
- mode-aware `hcodex` launch / continue / resume commands

另外已新增：

- `GET /api/workspaces/:thread_key/execution-mode`
- `POST /api/threads/:thread_key/actions` + `{ "action": "set_workspace_execution_mode", "execution_mode": "full_auto|yolo" }`

web 管理面現在也已提供：

- per-workspace execution mode selector
- `Save Mode`
- drift 提示
- mode-aware recent-session resume / current-session continue command model

## 對 tray / local launch 的影響

這部分也已開始落地：

- tray workspace label 現在顯示 workspace mode
- tray action 仍只保留：
  - `New Session`
  - `Continue Telegram Session`
- tray 本身不承擔 mode 切換；mode 切換仍留在管理面
- `hcodex` launch / resume command 會自動加上對應 mode flag

## 對 mirror / observability 的影響

這部分還沒有完全落地，但方向已更清楚：

- execution mode 已進入 session / workspace view model
- observability 現在至少能看見：
  - workspace mode
  - current session mode
  - mode drift
- 更深層的 audit / event retention 仍未因 `yolo` 額外升級

換句話說，v1 已經把「mode 看不見」這個缺口補上，但還沒把「mode 改變 audit 保證」做成獨立規格。

## 與其他計劃的關係

- [runtime-protocol.md](runtime-protocol.md)
  - execution mode 已開始進入正式 view / action 模型
- [macos-menubar-thread-manager.md](../management-desktop-surface/macos-menubar-thread-manager.md)
  - management UI 與 tray 已開始表達 execution mode
- [session-level-mirror-and-readiness.md](session-level-mirror-and-readiness.md)
  - execution mode 已影響 `hcodex` launch / resume 與 session continuity 收斂
- [runtime-transport-abstraction.md](runtime-transport-abstraction.md)
  - execution mode 仍屬於 core runtime 語義，不應回退成 Telegram-only 開關
- [telegram-adapter-migration.md](../telegram-adapter/telegram-adapter-migration.md)
  - Telegram 已有 mode control surface，但仍不應把 execution mode 退化成 Telegram-only authority
  - 自定義 Codex config 若落地，也應先成為 owner/runtime 授權的正式 control surface，而不是 shell env 或 Telegram 私有參數注入

## 開放問題

- Telegram 是否應提供只讀 mode 顯示，還是未來允許直接切 mode？
- 若 Telegram 真的提供 execution mode 切換，user-facing naming 應該沿用 `full_auto / yolo`，還是改成其他更穩定的高階命名？
- 既然 Telegram 已支持 `Plan mode / 普通模式` collaboration mode 切換，應如何避免它和 `full_auto / yolo` 混淆？
- `Codex 工作模型` 是否也應由 Telegram 提供對應設定入口，且與 execution mode 明確拆成兩條 control surface？
- `threadBridge` 若支持自定義 Codex config，scope 應先做 global default、per-workspace sticky config，還是兩者並存？
- 自定義 Codex config 的正式 artifact 應掛在既有 `workspace-config.json`，還是拆成獨立 config surface？
- 哪些內容屬於可安全暴露的高階 config，哪些仍不應讓一般 surface 直接透傳到底層 Codex CLI？
- execution mode 是否需要額外的 audit retention 或更長 process transcript 保留？
- 未來若 Codex 支持更多 approval / sandbox 組合，threadBridge 是否仍只暴露高階 mode？
- 是否需要在 launch surface 正式支持一次性 override，而不是永遠以 workspace mode 為準？
- `mode_source` 是否值得在後續 view model 中正式落地？

## 建議的下一步

1. 更新其餘 plan 文檔，反映 execution mode 已是部分落地的正式 runtime 語義，且 Telegram 已有 `/get_workspace_execution_mode` / `/set_workspace_execution_mode` surface。
2. 在 `runtime-protocol` 補齊 execution mode control 的正式命名，避免目前仍同時存在 management API route 與 Telegram slash command 的雙重表述。
3. 固定 Telegram user-facing naming，決定是否繼續沿用 `full_auto / yolo`，或提供更穩定的高階文案。
4. 補一份與 execution mode 分離的 `Codex 工作模型 / 自定義 Codex config` control 規格，不要把 model、config 與 mode 混在同一份開關語意裡。
5. 先決定自定義 Codex config 的 scope、artifact 與安全邊界，再決定它是否進入 management UI、Telegram command surface 或其他 launch surface。
6. 若未來真的引入更多 profile，再決定是否需要 `mode_source`、launch override 或更細的 audit policy。
