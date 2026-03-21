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
- desktop runtime 對 app-server / TUI proxy / managed Codex 的 owner 職責仍是部分實作
- `hcodex` self-heal 仍未收斂成純 fallback

## 現況定位

這份 plan 處理的是 shared runtime、`hcodex`、Telegram continuity 與 owner 邊界。

現在的正式方向是：

- Telegram 與本地 `hcodex` 共用同一個 workspace-scoped app-server daemon
- `current_codex_thread_id` 是 Telegram continuity
- `tui_active_codex_thread_id` 是受管 TUI runtime state
- 本地管理面是新的 owner / control surface 候選，不是新的聊天入口

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

## 與本地管理面的關係

本地 tray / web 管理面不只是 UI surface，它也開始承擔 owner 模型的落腳點。

合理的分工應是：

- desktop runtime
  - managed Codex binary
  - app-server
  - TUI proxy
  - local management API
  - runtime health view
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

## 下一步

1. 繼續把 desktop runtime owner 模型補齊。
2. 讓本地 management API 成為正式 query / control surface。
3. 讓 `hcodex` 的 self-heal 逐步收斂成 fallback。
