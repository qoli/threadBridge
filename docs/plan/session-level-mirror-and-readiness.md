# Session-Level Mirror And Readiness

## 目前進度

這份 plan 已落地。

目前已落地：

- `current_codex_thread_id` 已成為 Telegram thread 的 canonical continuity pointer
- `tui_active_codex_thread_id`、adoption、auto-adopt、mirror 已落地
- `threadBridge` 會為每個 bound workspace 啟動共享的 `codex app-server`
- `./.threadbridge/bin/hcodex` 已是受管 remote TUI 入口，且依賴 desktop runtime owner
- process transcript 已正式區分 final / process，並補上 management transcript read API、web observability pane 與 Telegram rolling preview
- Telegram 文本顯示已開始從舊 `CLI/TUI` label 收斂到更明確的使用者 / assistant / system 呈現

## 現況定位

這份 plan 處理的是：

- shared runtime
- `hcodex`
- Telegram continuity
- local/TUI mirror
- idle/free readiness

現在的正式模型是：

- desktop runtime 是唯一 owner
- Telegram 與本地 `hcodex` 共用同一個 workspace-scoped app-server daemon
- `current_codex_thread_id` 是 Telegram continuity
- `tui_active_codex_thread_id` 是受管 TUI runtime state
- Telegram 不再透過舊 handoff / CLI owner 模型工作，而是透過 mirror + adoption + idle/free readiness 工作

## 已收斂的術語

- `current_codex_thread_id`
  - Telegram thread 目前正式採用的 Codex 對話
- `tui_active_codex_thread_id`
  - 受管 `hcodex` 最近一次 `resume` 或 `new session` 使用的 Codex 對話
- `adoption`
  - TUI 結束後，Telegram 是否切換到 TUI session 繼續對話
- `local session`
  - workspace 內活著的本地受管互動 session
- `mirror`
  - 把 local/TUI prompt、assistant final reply、以及 process transcript 映射回 Telegram / management transcript
- `idle/free readiness`
  - Telegram 是否可安全發起下一個 turn 的 readiness 判斷

## Runtime Ownership 與 Readiness

- bot 現在是 shared runtime client，不再是正式 owner
- `hcodex` 不再補拉 runtime，只是 owner-managed shared runtime 的受管本地入口
- desktop runtime 對 app-server、TUI proxy、owner heartbeat、repair/reconcile 持有 canonical authority
- runtime health 以 `owner_heartbeat` 為主，workspace shared status 只保留 activity / observation 語義
- Telegram 是否可發起新 turn，判斷依據是：
  - owner 是否健康
  - active session 是否 busy
  - 是否有 pending adoption
  - 是否處於 idle/free

## Mirror 與 Transcript

目前 mirror 的正式範圍是：

- local/TUI user prompt
- final assistant reply
- Plan / Tool process transcript

目前已明確收斂的做法是：

- `final transcript`
  - user prompt + final assistant
- `process transcript`
  - plan text、tool text、其他過程事件
- Telegram 與 management UI 共用同一份 transcript/mirror 基礎
- transport/source metadata 只保留作 debug / observability，不再作為 Telegram 可見角色命名

## 與本地管理面的關係

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
  - workspace 內的正式受管本地入口
  - 不再承擔 fallback owner 職責

## 與其他計劃的關係

- [macos-menubar-thread-manager.md](/Volumes/Data/Github/threadBridge/docs/plan/macos-menubar-thread-manager.md)
  - 定義 tray / web 管理面與 owner 邊界
- [runtime-protocol.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-protocol.md)
  - 定義管理面使用的 view / action / transcript 命名
- [runtime-transport-abstraction.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-transport-abstraction.md)
  - owner 收斂完成後，Telegram 才能真正退回成 transport adapter
- [telegram-adapter-migration.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-adapter-migration.md)
  - adapter 化建立在 mirror/readiness 模型，而不是舊 CLI handoff 模型之上

## 下一步

1. 繼續把 transcript / event contract 往更完整的 transport-neutral protocol 收斂。
2. 在 owner 去 Telegram 化之後，再推進更完整的 transport / adapter 抽象。
