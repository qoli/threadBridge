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
- [session-level-mirror-and-readiness.md](/Volumes/Data/Github/threadBridge/docs/plan/session-level-mirror-and-readiness.md)
  - shared app-server daemon、`./.threadbridge/bin/hcodex`、TUI proxy、mirror、adoption、auto-adopt 已落地
  - desktop runtime 已成為正式 owner 啟動模型，headless 啟動路徑已退場
  - `hcodex` self-heal 已移除，缺少 desktop owner 時會明確失敗
  - workspace heartbeat / runtime health 已改成以 desktop owner heartbeat 為主 authority
  - 舊 `CLI owner / handoff` 概念已退出現行模型，主語義改為 local/TUI mirror + idle/free readiness
  - process transcript 已正式區分 final / process，並補上 management transcript read API、web observability pane 與 Telegram rolling preview 摘要
  - 它同時也是 Telegram 退回通用 adapter 模式的前置條件

## 部分落地

- [telegram-markdown-adaptation.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-markdown-adaptation.md)
  - final reply 的 Telegram HTML renderer、plain-text fallback、attachment fallback 已落地
  - 但 attachment fallback 與 Telegram 文件大小上限的關係仍待收斂
- [codex-busy-input-gate.md](/Volumes/Data/Github/threadBridge/docs/plan/codex-busy-input-gate.md)
  - v1 忙碌閘控已落地
  - Telegram 文字 turn / 圖片分析已改成 background 執行，後續輸入現在會命中 reject
  - bot 啟動時的 stale busy reconciliation 已開始落地
  - 但 queue 模型、更完整的狀態語義、`STOP` / 提示類互動控制面、更乾淨的 ingress / dispatcher 邊界，以及更完整的 stale busy owner 模型仍未收斂
- [topic-title-status.md](/Volumes/Data/Github/threadBridge/docs/plan/topic-title-status.md)
  - 已落地 `workspace/title + busy/broken suffix`
  - 已落地新產生的 topic rename service message best-effort cleanup
  - context ratio 仍未實作
- [session-lifecycle.md](/Volumes/Data/Github/threadBridge/docs/plan/session-lifecycle.md)
  - `/add_workspace`、`/new_session`、`/repair_session` 的正式生命週期已存在
  - `current_codex_thread_id` 已成為 canonical pointer，`tui_active_codex_thread_id` / adoption 也已進入正式 runtime
  - Telegram thread 內的一般輸入與 session-control gate 已開始直接讀 canonical state
  - 剩餘工作主要是兼容層與狀態語義收尾
- [runtime-protocol.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-protocol.md)
  - 本地 management API 已開始承接它的 view / action 命名
  - local HTTP + SSE 已從草稿變成實際 transport
  - 近期已再補上 runtime-owner reconcile、managed Codex build defaults、workspace launch config、continue-current launch control，以及 thread transcript read API
  - `GET /api/threads` 已開始對外暴露 canonical `lifecycle_status`
  - runtime health 已改成 owner-canonical，`workspace_state` 僅保留 debug/observation 語義
  - process transcript event / mirror model 已接到 management API、web UI 與 Telegram rolling preview，但 protocol 仍未收斂成正式 transport-neutral 契約
- [macos-menubar-thread-manager.md](/Volumes/Data/Github/threadBridge/docs/plan/macos-menubar-thread-manager.md)
  - `threadbridge_desktop`、macOS-first tray menu、workspace-first browser management UI 已開始落地
  - pick-and-add、adopt / reject TUI、runtime-owner reconcile、launch config 等 control 已進入 management API
  - managed Codex source build / cache refresh / build defaults 已進入 management API
  - tray menu 已收斂成 `New Session` 與 `Continue Telegram Session`
  - management health view 已改成 owner heartbeat 為主的 desktop-first 模型
  - management UI 已補上 transcript observability pane，且 adoption/repair action 已改成 owner-canonical 語義
  - web 管理面新增確認的 UI 收斂方向是可評估以 HeroUI 重構
  - 目前新增確認的收斂方向是 `workspace = thread` 主模型、desktop-only 啟動與移除暫不可用的 onboarding
- [runtime-state-machine.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-state-machine.md)
  - canonical `lifecycle_status` / `binding_status` / `run_status` 已開始透過 shared resolver 進入代碼
  - ordinary Telegram gate、management API、topic title 已開始共用同一套 canonical state axes
  - `binding_status=conflict`、`run_status=unbound` 這類過渡值已退出 canonical state axes
  - 但它仍未成為所有 surface 的完整唯一 source of truth

## 純草稿

- [message-queue-and-status-delivery.md](/Volumes/Data/Github/threadBridge/docs/plan/message-queue-and-status-delivery.md)
  - Telegram outbound delivery 主規格草稿
  - 也承接 busy / running 狀態訊息上的互動 control surface 規格
  - 以及文件 / 媒體超過 Telegram 上限時的 delivery fallback 規格
- [telegram-webapp-observability.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-webapp-observability.md)
  - Telegram Web App 觀測面草稿
  - 目前已有通用 management API / SSE 骨架，但 thread-level observability API 仍未成形
- [llm-guidance-and-goals.md](/Volumes/Data/Github/threadBridge/docs/plan/llm-guidance-and-goals.md)
  - secondary LLM 設定、AI 建議與 AI 目標層草稿
- [codex-execution-modes.md](/Volumes/Data/Github/threadBridge/docs/plan/codex-execution-modes.md)
  - Codex execution profile / `yolo mode` 草稿
- [desktop-runtime-tool-bridge.md](/Volumes/Data/Github/threadBridge/docs/plan/desktop-runtime-tool-bridge.md)
  - desktop runtime 作為跨沙盒 capability host / tool bridge 草稿
- [optional-agents-injection.md](/Volumes/Data/Github/threadBridge/docs/plan/optional-agents-injection.md)
  - appendix 注入可選化草稿
- [runtime-transport-abstraction.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-transport-abstraction.md)
  - core runtime / adapter 抽象化草稿
  - owner 收斂應視為這條抽象化路線的高優先級前置工作
- [telegram-adapter-migration.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-adapter-migration.md)
  - Telegram adapter 遷移草稿
  - owner authority 應先從 Telegram 路徑抽離，再做更完整的 adapter migration

## 主規格

- [runtime-state-machine.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-state-machine.md)
  - 目標是未來的狀態語義主規格
- [message-queue-and-status-delivery.md](/Volumes/Data/Github/threadBridge/docs/plan/message-queue-and-status-delivery.md)
  - 目標是 Telegram delivery 主規格

目前這兩份都還沒有完全變成實際代碼的唯一 source of truth。

## 依賴關係

- `session-lifecycle`
  - 描述 thread / workspace / Codex thread 的生命週期
- `codex-busy-input-gate`
  - 描述 turn 互斥與 busy gate
- `codex-cli-telegram-status-sync-hooks`
  - 把舊的本地 CLI 狀態接到同一份 busy / title 模型
- `session-level-mirror-and-readiness`
  - 描述 local/TUI mirror、adoption、與 idle/free readiness 的現行模型
- `topic-title-status`
  - 描述 title 應承載哪些狀態
- `runtime-state-machine`
  - 最終應把上面幾份文件的狀態語義統一

## 備註

- 這個目錄現在同時包含已落地方案和未實作草稿，不能只看標題判斷成熟度。
- 如果某份文檔和代碼有衝突，先以代碼為準，再回來更新該文檔。
