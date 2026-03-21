# Plan Index

這個目錄用來放 `threadBridge` 的設計草稿、已落地方案與後續重構方向。

如需新增新想法或整理既有 plan，先看 [authoring-guide.md](/Volumes/Data/Github/threadBridge/docs/plan/authoring-guide.md)。

## 閱讀方式

- 先看「已落地 / 部分落地 / 純草稿」區分，不要把所有文件都當成同一成熟度。
- 再看「主規格」與「依賴關係」。
- 單篇文檔內的 `目前進度` 是這次整理後的最新狀態註記。

## 已落地

- [codex-cli-telegram-status-sync-hooks.md](/Volumes/Data/Github/threadBridge/docs/plan/codex-cli-telegram-status-sync-hooks.md)
  - 已完成 v1
  - Bash wrapper、Codex hooks、notify、workspace shared status、topic title watcher、busy gate 都曾落地
  - 現在已退役，只保留作為舊模型參考

## 部分落地

- [telegram-markdown-adaptation.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-markdown-adaptation.md)
  - final reply 的 Telegram HTML renderer、plain-text fallback、attachment fallback 已落地
  - 但 attachment fallback 與 Telegram 文件大小上限的關係仍待收斂
- [codex-busy-input-gate.md](/Volumes/Data/Github/threadBridge/docs/plan/codex-busy-input-gate.md)
  - v1 忙碌閘控已落地
  - Telegram 文字 turn / 圖片分析已改成 background 執行，後續輸入現在會命中 reject
  - 但 queue 模型、更完整的狀態語義、`STOP` / 提示類互動控制面、更乾淨的 ingress / dispatcher 邊界，以及 bot crash 後 stale busy gate recovery 仍未收斂
- [topic-title-status.md](/Volumes/Data/Github/threadBridge/docs/plan/topic-title-status.md)
  - 已落地 `workspace/title + busy/broken suffix`
  - 已落地新產生的 topic rename service message best-effort cleanup
  - context ratio 仍未實作
- [session-lifecycle.md](/Volumes/Data/Github/threadBridge/docs/plan/session-lifecycle.md)
  - `/new_thread`、`/bind_workspace`、`/new`、`/reconnect_codex` 的基本生命週期已存在
  - `current_codex_thread_id` 已成為 canonical pointer，`tui_active_codex_thread_id` / adoption 也已進入正式 runtime
  - 剩餘工作主要是兼容層與狀態語義收尾
- [session-level-cli-telegram-sync.md](/Volumes/Data/Github/threadBridge/docs/plan/session-level-cli-telegram-sync.md)
  - shared app-server daemon、`./.threadbridge/bin/hcodex`、TUI proxy、mirror、adoption、auto-adopt 已落地
  - `/attach_cli_session`、viewer handoff、attach-intent、hooks-based CLI sync、`.cli/.attach` title 已退場
  - 目前新增確認的缺口是 desktop runtime owner 尚未完全收斂；`hcodex` self-heal 目前仍只是 fallback，bot 重寫出的 ws state 也仍可能 stale
  - local management API 與無 Telegram 憑據先啟動的 groundwork 已開始落地
  - 剩餘工作主要是 runtime ownership 與本地管理面收尾

## 純草稿

- [runtime-state-machine.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-state-machine.md)
  - 狀態語義主規格草稿
- [message-queue-and-status-delivery.md](/Volumes/Data/Github/threadBridge/docs/plan/message-queue-and-status-delivery.md)
  - Telegram outbound delivery 主規格草稿
  - 也承接 busy / running 狀態訊息上的互動 control surface 規格
  - 以及文件 / 媒體超過 Telegram 上限時的 delivery fallback 規格
- [telegram-webapp-observability.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-webapp-observability.md)
  - Telegram Web App 觀測面草稿
- [optional-agents-injection.md](/Volumes/Data/Github/threadBridge/docs/plan/optional-agents-injection.md)
  - appendix 注入可選化草稿
- [runtime-transport-abstraction.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-transport-abstraction.md)
  - core runtime / adapter 抽象化草稿
- [runtime-protocol.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-protocol.md)
  - runtime 協議草稿
  - 現在也承接本地 management API 的 view / action 命名與 local HTTP + SSE 載體
- [telegram-adapter-migration.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-adapter-migration.md)
  - Telegram adapter 遷移草稿
- [macos-menubar-thread-manager.md](/Volumes/Data/Github/threadBridge/docs/plan/macos-menubar-thread-manager.md)
  - macOS 托盤 thread 管理面草稿
  - 現在也承接 workspace `ws` runtime owner、managed Codex binary、workspace `hcodex` 快捷啟動入口，以及 tray + web 管理面的方向收斂

## 主規格

- [runtime-state-machine.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-state-machine.md)
  - 目標是未來的狀態語義主規格
- [message-queue-and-status-delivery.md](/Volumes/Data/Github/threadBridge/docs/plan/message-queue-and-status-delivery.md)
  - 目標是 Telegram delivery 主規格

目前這兩份都還沒有變成實際代碼的唯一 source of truth。

## 依賴關係

- `session-lifecycle`
  - 描述 thread / workspace / Codex thread 的生命週期
- `codex-busy-input-gate`
  - 描述 turn 互斥與 busy gate
- `codex-cli-telegram-status-sync-hooks`
  - 把本地 CLI 狀態接到同一份 busy / title 模型
- `session-level-cli-telegram-sync`
  - 描述真正的同 session 雙窗口輸入 / 事件同步願景
- `topic-title-status`
  - 描述 title 應承載哪些狀態
- `runtime-state-machine`
  - 最終應把上面幾份文件的狀態語義統一

## 備註

- 這個目錄現在同時包含已落地方案和未實作草稿，不能只看標題判斷成熟度。
- 如果某份文檔和代碼有衝突，先以代碼為準，再回來更新該文檔。
