# Session-Level CLI / Telegram 同步

## 目前進度

這份 plan 已部分落地。

目前已落地：

- `current_codex_thread_id` 已成為 Telegram thread 的 canonical continuity pointer
- `tui_active_codex_thread_id`、adoption、auto-adopt、mirror 已落地
- threadBridge 會為每個 bound workspace 啟動共享的 `codex app-server`
- `./.threadbridge/bin/hcodex` 已是受管 remote TUI 入口
- `hcodex` 目前仍保留過渡性的本地 self-heal
- threadBridge 已開始提供本地 management API 骨架，且可在沒有 Telegram 憑據時先啟動本地 runtime

目前仍未完成：

- workspace `ws` runtime 的正式長壽命 owner 尚未完全收斂
- desktop runtime 對 handoff continuity / adoption 的 owner 職責仍是部分實作
- `hcodex` self-heal 仍未收斂成純 fallback
- Codex mirror 目前仍未完整承接 Plan / Tool 過程中的文本
- workspace heartbeat / runtime health 目前仍是過渡性多來源模型

目前新增確認的優先級判斷是：

- owner 責任收斂應視為高優先級工作

原因是：

- 它會直接影響 shared runtime 是否可靠
- 也會直接影響 reconnect、self-heal、handoff continuity、runtime health 與 mirror 這些能力的語義是否可信
- 若 owner 邊界不先收斂，其他上層功能很容易繼續建立在過渡性行為上
- 它同時也是把 Telegram 從 runtime core 收斂成通用 adapter 的前置條件

目前新增確認的一個具體症狀是：

- workspace heartbeat 還沒有單一 canonical authority
- `owner_heartbeat` 與 workspace shared status / runtime state 目前同時存在
- 這不是單純 view model 問題，而是 owner 收斂尚未完成的直接表現

## 現況定位

這份 plan 處理的是 shared runtime、`hcodex`、Telegram continuity 與 owner 邊界。

現在的正式方向是：

- Telegram 與本地 `hcodex` 共用同一個 workspace-scoped app-server daemon
- `current_codex_thread_id` 是 Telegram continuity
- `tui_active_codex_thread_id` 是受管 TUI runtime state
- 本地管理面是新的 owner / control surface 候選，不是新的聊天入口

目前 mirror 的實際能力，仍主要偏向：

- CLI / TUI user prompt
- final assistant reply

而不是完整的 session transcript replay。

## 已收斂的術語

- `current_codex_thread_id`
  - Telegram thread 目前正式採用的 Codex 對話
- `tui_active_codex_thread_id`
  - 受管 `hcodex` 最近一次 `resume` 或 `new session` 使用的 Codex 對話
- `adoption`
  - TUI 結束後，Telegram 是否切換到 TUI session 繼續對話

## Runtime Ownership 現況

- bot 目前是 shared runtime client，但還不是可靠的長壽命 owner
- `hcodex` 仍保留補拉 runtime 的能力，但它不應成為正式 owner
- threadBridge 現在已能在無 Telegram 憑據時先啟動本地 management API
- 這表示 owner 模型正在從「bot / `hcodex` 臨時補位」移向「desktop runtime 正式持有本地 runtime」

因此目前的 operational reality 是：

- Telegram turn 在 bot 成功 `ensure` 當下可走 shared websocket daemon
- 本地 `hcodex` 仍依賴 self-heal 作為 fallback
- workspace `ws` runtime 的正式 owner 還需要進一步收斂到 desktop runtime
- runtime health 目前仍會在 `owner_heartbeat` 與 `workspace_state` 之間切換來源

更具體地說，目前系統其實在同時保存兩類不同訊號：

- owner heartbeat
  - 回答 app-server / TUI proxy / handoff readiness 是否健康
- workspace shared status
  - 回答 live CLI / shell / session activity 是否存在

這兩類訊號本來就不完全是同一件事，但現在還沒有唯一 owner 來定義哪個才是 canonical runtime health authority。

這件事不只是架構清理，而是目前整條 shared runtime 路線的高優先級收斂項。

比較合理的收斂方向應是：

- desktop owner heartbeat
  - 成為 canonical runtime health source
- workspace shared status
  - 只表達 CLI / turn / shell activity
- bot / `hcodex` / management UI
  - 只讀 owner view，或要求 owner repair，而不是各自再補 runtime health 判斷

## 與本地管理面的關係

本地 tray / web 管理面不只是 UI surface，它也開始承擔 owner 模型的落腳點。

合理的分工應是：

- desktop runtime
  - managed Codex binary
  - app-server
  - TUI proxy
  - local management API
  - runtime health view
  - 背景 reconcile / repair runtime
- bot
  - Telegram adapter / client
- `hcodex`
  - workspace 內的正式受管 CLI 入口
  - owner 尚未完全切換前的 fallback

## 與 `hcodex` 的關係

tray 或 web 管理面新增 workspace 啟動入口後，不能和現有 `./.threadbridge/bin/hcodex` 形成兩套互相競爭的本地入口。

比較合理的方向是：

- `hcodex` 保持 workspace 內的正式受管 CLI 入口
- tray / web 管理面只負責找到 workspace、展示 recent session、發送 launch action
- `hcodex` self-heal 應逐步收斂成 fallback，而不是長期 owner 模型

## Mirror 文本覆蓋缺口

目前 Codex mirror 已能把部分 CLI / TUI 互動映射回 Telegram thread，但仍不是完整的「Codex session 文本鏡像」。

目前已較明確落地的 mirror 類型主要是：

- user prompt submitted
- final assistant message

目前缺口是：

- Plan 過程中的文本尚未正式 mirror
- Tool 執行過程中的文本尚未正式 mirror
- 因此 Telegram thread 看到的 mirror，仍偏向「輸入 + 最終回覆」，而不是完整的過程文本

這個缺口的影響包括：

- 使用者在 Telegram 端難以理解 Codex 在 Plan / Tool 階段做了什麼
- mirror 目前更像 continuity assist，而不是 process visibility
- 若之後要把 mirror 當成 desktop / web / custom surface 的共用 transcript 基礎，現有事件粒度仍不足

比較合理的後續方向是：

- 先明確定義哪些 Plan / Tool 文本值得 mirror
- 把它們掛回 runtime event / protocol，而不是在 Telegram adapter 內臨時拼接
- 區分 `final transcript` 與 `process transcript`
  - `final transcript`：user prompt + final assistant
  - `process transcript`：plan text、tool text、其他過程事件

## recent session history

tray menu 需要每個 workspace 最近 5 個 Codex `thread.id`。

這份歷史應由 runtime 維護，至少從這些事件更新：

- Telegram 正常 turn
- `/new`
- `/reconnect_codex`
- 受管 TUI `thread/start`
- 受管 TUI `thread/resume`
- adoption 成功後切換

## 與其他計劃的關係

- [macos-menubar-thread-manager.md](/Volumes/Data/Github/threadBridge/docs/plan/macos-menubar-thread-manager.md)
  - 定義 tray / web 管理面與 owner 邊界
- [runtime-protocol.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-protocol.md)
  - 定義管理面應使用的 view / action 命名
  - 之後若 mirror 要承接 Plan / Tool 文本，應由 protocol 定義事件粒度，而不是只留在 Telegram mirror helper 內部
- [runtime-transport-abstraction.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-transport-abstraction.md)
  - owner 責任若不先收斂，Telegram 很難真正退回成單純 transport adapter
- [telegram-adapter-migration.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-adapter-migration.md)
  - owner 收斂應先於更完整的 Telegram adapter 遷移，否則 Telegram 仍會保有 runtime authority

## 下一步

1. 優先把 desktop runtime owner 模型補齊。
2. 讓本地 management API 成為正式 query / control surface。
3. 讓 `hcodex` 的 self-heal 逐步收斂成 fallback。
4. 明確補上 mirror 對 Plan / Tool 過程文本的事件與顯示語義。
5. 在 owner 去 Telegram 化之後，再推進更完整的 transport / adapter 抽象。
