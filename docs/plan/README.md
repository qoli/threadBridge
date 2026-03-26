# Plan Index

這個目錄用來放 `threadBridge` 的設計草稿、已落地方案與後續重構方向。

如需新增新想法或整理既有 plan，先看 [authoring-guide.md](/Volumes/Data/Github/threadBridge/docs/plan/authoring-guide.md)。
如需先對齊詞彙，再看 [authoring-guide.md](/Volumes/Data/Github/threadBridge/docs/plan/authoring-guide.md) 裡的「術語與命名要求」。

## 閱讀方式

- 先看「已落地 / 部分落地 / 純草稿」區分，不要把所有文件都當成同一成熟度。
- 再看「主規格」與「依賴關係」。
- 若是在判斷角色邊界、ownership、或某個 bug 應該落在哪一層，先看 [runtime-architecture.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-architecture.md)。
- 單篇文檔內的 `目前進度` 是這次整理後的最新狀態註記。

## 已落地

- [codex-cli-telegram-status-sync-hooks.md](/Volumes/Data/Github/threadBridge/docs/plan/codex-cli-telegram-status-sync-hooks.md)
  - 已完成 v1
  - Bash wrapper、Codex hooks、notify、workspace shared status、topic title watcher、busy gate 都曾落地
  - 現在已退役，只保留作為舊模型參考
- [hcodex-pre-refactor-history.md](/Volumes/Data/Github/threadBridge/docs/plan/hcodex-pre-refactor-history.md)
  - 記錄重構前 `hcodex` / shell wrapper / `codex_sync.py` 的歷史模型
  - 固定「舊模型雖髒，但本地 `codex` child lifecycle 閉環較強」這個背景結論
- [session-level-mirror-and-readiness.md](/Volumes/Data/Github/threadBridge/docs/plan/session-level-mirror-and-readiness.md)
- shared app-server daemon、`./.threadbridge/bin/hcodex`、hcodex ingress、mirror、adoption、auto-adopt 已落地
  - desktop runtime 已成為正式 owner 啟動模型，headless 啟動路徑已退場
  - `hcodex` self-heal 已移除，缺少 desktop owner 時會明確失敗
  - workspace heartbeat / runtime health 已改成以 desktop owner heartbeat 為主 authority
  - 舊 `CLI owner / handoff` 概念已退出現行模型，主語義改為 local/TUI mirror + idle/free readiness
  - process transcript 已正式區分 final / process，並補上 management transcript read API、session summary / records API、web observability pane，以及 Telegram rolling preview 摘要
  - `codex plan` mirror、plan-only final reply fallback、Telegram preview process transcript 已落地
- Telegram `Questions` / `Implement this plan` 已改成 observer / ingress 發出 adapter-neutral interaction event，再由 Telegram interaction bridge 消費

## 部分落地

- [runtime-architecture.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-architecture.md)
  - current architecture 的角色與責任主文檔
  - 固定 `desktop runtime owner`、shared `runtime_control`、observer、`hcodex`、Telegram adapter、management / desktop surface 的邊界
  - 明確列出目前仍存在的 temporary exception，避免未來修 bug 又回到 CLI 時代的止血式修法
- [codex-plan-mirror.md](/Volumes/Data/Github/threadBridge/docs/plan/codex-plan-mirror.md)
  - `codex plan` mirror 子規格
  - upstream `item/plan/delta` / finalized `plan` item 已確認存在
  - `threadBridge` 已消費 live `item/plan/delta`，並補上 plan-only final reply fallback
- [telegram-markdown-adaptation.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-markdown-adaptation.md)
  - final reply 的 Telegram HTML renderer、plain-text fallback、attachment fallback 已落地
  - `reply.md` attachment 的 Telegram 文件大小 preflight 與 warning fallback 已開始落地
  - 但更完整的 artifact / URL fallback 仍待收斂
- [codex-busy-input-gate.md](/Volumes/Data/Github/threadBridge/docs/plan/codex-busy-input-gate.md)
  - v1 忙碌閘控已落地
  - Telegram 文字 turn / 圖片分析已改成 background 執行，後續輸入現在會命中 reject
  - `/stop` 已作為第一個正式 busy control action 落地，並開始使用 session turn id 走 app-server interrupt
  - bot 啟動時的 stale busy reconciliation 已開始落地
  - 但 queue 模型、更完整的狀態語義、`STOP 並插入發言` / `序列發言` 這類 follow-up 控制面、更乾淨的 ingress / dispatcher 邊界，以及更完整的 stale busy owner 模型仍未收斂
- [topic-title-status.md](/Volumes/Data/Github/threadBridge/docs/plan/topic-title-status.md)
  - 已落地 `workspace/title + broken suffix`
  - 已落地新產生的 topic rename service message best-effort cleanup
  - context ratio 仍未實作
- [session-lifecycle.md](/Volumes/Data/Github/threadBridge/docs/plan/session-lifecycle.md)
  - `/add_workspace`、`/new_session`、`/repair_session` 的正式生命週期已存在
  - `current_codex_thread_id` 已成為 canonical pointer，`tui_active_codex_thread_id` / adoption 也已進入正式 runtime
  - Telegram thread 內的一般輸入、圖片分析、session-control gate、以及 stale busy reconciliation 已開始直接讀 canonical state
  - `current_codex_thread_id` 的存在已不再被視為等於「目前一定可直接 resume」；usable continuity 仍取決於 canonical `binding_status`
  - canonical continuity mutation 已開始透過 repository 內部的共用 transition path 收斂
  - workspace runtime ensure、session bind/new/repair、以及 Telegram-to-live-TUI routing 已進一步抽成 shared `runtime_control` service
  - 已新增記錄：Telegram desktop launch command 應作為獨立 control surface，而不是改寫 `/new_session`
  - 剩餘工作主要是兼容層與狀態語義收尾
- [runtime-protocol.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-protocol.md)
  - 本地 management API 已開始承接它的 view / action 命名
  - local HTTP + SSE 已從草稿變成實際 transport
  - 近期已再補上 runtime-owner reconcile、managed Codex build defaults、workspace launch config、continue-current launch control、thread transcript read API，以及 session summary / session records read API
  - `GET /api/threads` 已開始對外暴露 canonical `lifecycle_status`，並補齊 `chat_id` / `message_thread_id` / `session_broken_reason` / `last_verified_at` / `last_codex_turn_at`
  - runtime health 已改成 owner-canonical，`workspace_state` 僅保留 debug/observation 語義
  - `runtime_protocol` 共享 view builder 已開始把 `ThreadStateView` / `ManagedWorkspaceView` / `ArchivedThreadView` / `RuntimeHealthView` / `WorkingSession*View` 從 transport 層抽離
  - broken thread count、workspace recovery hint、以及 working session broken error 聚合，已開始改以 canonical `binding_status` 判定
  - public view 已開始移除 `session_broken` / `last_error` alias；對外主判斷改固定收斂到 `binding_status` / `run_status` / `session_broken_reason`
  - `GET /api/events` 已開始輸出 typed SSE event，而不是每輪都推整包 snapshot
  - web UI 已開始直接套用 top-level typed SSE payload，並只對 transcript / sessions 做 targeted refetch
  - 但 protocol 仍未收斂成完整 transport-neutral 契約，尤其更細的 observability record 仍未走完整增量 event 模型
- [macos-menubar-thread-manager.md](/Volumes/Data/Github/threadBridge/docs/plan/macos-menubar-thread-manager.md)
  - `threadbridge_desktop`、macOS-first tray menu、workspace-first browser management UI 已開始落地
  - pick-and-add、adopt / reject TUI、runtime-owner reconcile、launch config 等 control 已進入 management API
  - managed Codex source build / cache refresh / build defaults 已進入 management API
  - tray menu 已收斂成 `New Session` 與 `Continue Telegram Session`
  - tray workspace label 現在已改成顯示 workspace execution mode，而不是 handoff `ready/degraded` 文案
  - management health view 已改成 owner heartbeat 為主的 desktop-first 模型
  - management UI 已補上 transcript observability pane、workspace-card `Sessions` pane 與 inline records timeline，且 adoption/repair action 已改成 owner-canonical 語義
  - management UI 已補上 workspace execution mode 切換、mode drift 提示，以及 mode-aware launch/resume commands
  - web 管理面新增確認的 UI 收斂方向是可評估以 HeroUI 重構
  - 目前新增確認的收斂方向是 `workspace = thread` 主模型、desktop-only 啟動與移除暫不可用的 onboarding
- [working-session-observability.md](/Volumes/Data/Github/threadBridge/docs/plan/working-session-observability.md)
  - desktop runtime / web 管理面的 session 級 observability 已進入部分落地
  - `WorkingSessionSummaryView` / `WorkingSessionRecordView`、`GET /api/threads/:thread_key/sessions`、`GET /api/threads/:thread_key/sessions/:session_id/records` 已落地
  - management UI 已可在 workspace card 的 `Sessions` pane 中直接打開 session timeline
  - artifact refs、獨立 observability page、retention / redaction 邊界仍未收斂
- [codex-execution-modes.md](/Volumes/Data/Github/threadBridge/docs/plan/codex-execution-modes.md)
  - execution mode 已進入正式 runtime 模型，不再只是草稿命題
  - workspace-local `workspace-config.json`、`ExecutionMode` enum、session execution snapshot 已落地
  - management API / launch-config / web UI 已開始暴露 workspace mode、current session mode 與 `mode_drift`
  - `hcodex` 與 Telegram turn/resume 已開始按 workspace mode 收斂到 `full_auto` 或 `yolo`
  - Telegram 已補上 `/execution_mode` command surface，但 user-facing naming、owner vocabulary、以及 `Codex 工作模型` 是否與 mode 分離對外暴露，仍未收斂
- [runtime-state-machine.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-state-machine.md)
  - canonical `lifecycle_status` / `binding_status` / `run_status` 已開始透過 shared resolver 進入代碼
  - ordinary Telegram gate、圖片分析、stale busy reconciliation、management API、topic title 已開始共用同一套 canonical state axes
  - management API 的 thread / workspace / runtime views 已開始透過共享 protocol/view builder 收斂到同一套 canonical state sources
  - repository write-side 的 canonical mutation 已開始透過 transition service 收斂
  - workspace recovery hint、broken thread count、以及 working session broken error 聚合，已開始從 canonical `binding_status` 派生
  - `/api/events` 已開始從 canonical view diff 輸出 typed SSE event
  - web 管理面已開始直接套用 top-level typed payload
  - `binding_status=conflict`、`run_status=unbound` 這類過渡值已退出 canonical state axes
  - `session_broken` 仍保留為內部持久化 continuity 記錄，但 public surface 的 canonical 判斷已收斂回 `binding_status`；`current_codex_thread_id` 也不再被等同於「一定可直接 resume 的 usable continuity」
  - 但它仍未成為所有 surface 的完整唯一 source of truth，尤其更細的 event payload coverage 與 observability 仍待收斂
- [workspace-runtime-surface.md](/Volumes/Data/Github/threadBridge/docs/plan/workspace-runtime-surface.md)
  - `.threadbridge/`、managed appendix、`hcodex`、tool request/result lane 已形成實際 workspace runtime surface
  - 但按 project type / workspace profile 選擇啟用 tools 的模型仍未收斂
- [message-queue-and-status-delivery.md](/Volumes/Data/Github/threadBridge/docs/plan/message-queue-and-status-delivery.md)
  - Telegram outbound delivery 主規格已從純草稿進入部分落地
  - workspace outbox `surface`、最小檔案大小 preflight、photo -> document fallback、以及 oversized attachment/document warning path 已開始落地
  - workspace outbox v1 目前只正式承諾 `content` / `status`；其他 `surface` 仍是保守兼容值
  - 已補記一個明確缺口：`codex mirror -> Telegram` 的 draft message 尚未實作 heartbeat，因此長時間 draft 仍會自動消失
  - 但 outbound queue、完整 control lifecycle、artifact 類型與集中化 config 仍未收斂
- [owner-runtime-contract.md](/Volumes/Data/Github/threadBridge/docs/plan/owner-runtime-contract.md)
  - owner/runtime boundary 的高層背景與收斂草稿
  - 角色與責任邊界現在以 `runtime-architecture.md` 為主文檔
  - 但 `hcodex` 長期 contract、observer attach contract、與 transport-neutral protocol 仍未完全收斂
- [app-server-ws-mirror-observer.md](/Volumes/Data/Github/threadBridge/docs/plan/app-server-ws-mirror-observer.md)
  - local/TUI mirror intake 已從 `hcodex ingress` 拆到獨立 app-server ws observer
  - observer 已不再直接做 Telegram interactive glue，而是發出 shared runtime interaction event
  - 但 public vocabulary、transport-neutral observer contract、以及 broader observability 收斂仍未完成
- [post-cli-runtime-cleanup.md](/Volumes/Data/Github/threadBridge/docs/plan/post-cli-runtime-cleanup.md)
  - CLI 時代的大部分 launch / vocabulary cleanup 已開始落地
  - 但 `hcodex` launch contract 仍保留 `launch_ticket + local hcodex-ws-bridge` compatibility boundary，不能再被誤判成可直接刪除
  - `workspace_status` 已補上 legacy `shared-runtime/*` / `local-session.json` migrate-read 與 canonical write-path 測試
  - 但 `workspace_status` / public naming / legacy compatibility policy 的 broader 收尾仍未完成
- [hcodex-launch-contract.md](/Volumes/Data/Github/threadBridge/docs/plan/hcodex-launch-contract.md)
  - 記錄 `hcodex` launch URL、local bridge、upstream Codex `--remote` 的實際契約
  - 明確固定兩個已修回歸：`invalid remote address ...?launch_ticket=...` 與 `failed to connect to remote app server`
- [hcodex-lifecycle-supervision.md](/Volumes/Data/Github/threadBridge/docs/plan/hcodex-lifecycle-supervision.md)
  - 記錄 `hcodex` 作為受管本地入口時，對 local Codex child lifecycle 的正式責任
  - 固定目前已確認的缺口：cleanup 不能只依賴 `run-hcodex-session` 的 happy path
- [hcodex-responsibility-matrix.md](/Volumes/Data/Github/threadBridge/docs/plan/hcodex-responsibility-matrix.md)
  - 將 `hcodex` 的長期責任收斂成 4 個核心詞：`launch / bridge / supervise / reconcile`
  - 區分哪些責任必須保留在 core、哪些只能暫時保留在周邊、哪些應移出 `hcodex` core

## 純草稿

- [telegram-webapp-observability.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-webapp-observability.md)
  - Telegram Web App 觀測面草稿
  - 本地 session-first observability API 與 workspace-card Sessions pane 已落地，但 Telegram Web App 本身仍未開始
  - 由於 Telegram Web App 依賴 HTTPS，近期已降為遠期可選載體，不再是本地 observability 的主路徑
- [llm-guidance-and-goals.md](/Volumes/Data/Github/threadBridge/docs/plan/llm-guidance-and-goals.md)
  - secondary LLM 設定、AI 建議與 AI 目標層草稿
- [desktop-runtime-tool-bridge.md](/Volumes/Data/Github/threadBridge/docs/plan/desktop-runtime-tool-bridge.md)
  - desktop runtime 作為跨沙盒 capability host / tool bridge / 自定義 webview service 草稿
- [optional-agents-injection.md](/Volumes/Data/Github/threadBridge/docs/plan/optional-agents-injection.md)
  - appendix 注入可選化草稿
- [runtime-transport-abstraction.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-transport-abstraction.md)
  - core runtime / adapter 抽象化草稿
  - owner 收斂與 shared control core 都應視為這條抽象化路線的已落地前置工作
  - 近期應先服務 Telegram 路徑收斂，而不是直接追求多 IM / 多 adapter 產品化
- [runtime-protocol-convergence.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-protocol-convergence.md)
  - `runtime-protocol` 的 rollout / convergence 草稿
  - 描述如何把 route、slash command、shared service、interaction event 收斂到同一份 protocol vocabulary
- [telegram-adapter-migration.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-adapter-migration.md)
  - Telegram adapter 遷移草稿
  - owner authority 與 shared runtime control 已先從 Telegram 路徑抽離，再做更完整的 adapter migration
  - 近期優先級應是補齊 Telegram 自身適配，而不是先做第二個 IM adapter 驗證
  - Telegram collaboration mode slash commands、`/launch`、`/execution_mode`、`/sessions`、`/session_log`、`/stop`，以及最小 `request_user_input` / `Implement this plan` 互動面已先行落地，且互動 UI 已改成 adapter-owned bridge
  - 近期 Telegram v0 剩餘能力面主要包括 Codex 工作模型設定、更完整的 Busy Gate follow-up control surface，以及 `main chat = control 面板` 下的 `forwarded input`

## 主規格

- [runtime-architecture.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-architecture.md)
  - 目標是當前架構的角色與責任主文檔
- [runtime-state-machine.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-state-machine.md)
  - 目標是未來的狀態語義主規格
- [message-queue-and-status-delivery.md](/Volumes/Data/Github/threadBridge/docs/plan/message-queue-and-status-delivery.md)
  - 目標是 Telegram delivery 主規格

目前這三份都還沒有完全變成實際代碼的唯一 source of truth。

其中 `runtime-architecture` 先固定 current architecture 的角色邊界與 temporary exception；`runtime-state-machine` 已開始同時影響 read-side state axes 與 repository 內部的 canonical transition path，但對外 event coverage / observability / full surface adoption 仍未完全收斂。

## 依賴關係

- `session-lifecycle`
  - 描述 thread / workspace / Codex thread 的生命週期
- `codex-busy-input-gate`
  - 描述 turn 互斥與 busy gate
- `codex-cli-telegram-status-sync-hooks`
  - 把舊的本地 CLI 狀態接到同一份 busy / title 模型
- `session-level-mirror-and-readiness`
  - 描述 local/TUI mirror、adoption、與 idle/free readiness 的現行模型
- `runtime-architecture`
  - 固定 current architecture 的角色與責任邊界
- `owner-runtime-contract`
  - 提供 owner/runtime boundary 的高層背景與收斂脈絡
- `hcodex-pre-refactor-history`
  - 記錄重構前 `hcodex` 的 shell / Python lifecycle 閉環背景
- `post-cli-runtime-cleanup`
  - 記錄 app-server / desktop owner 主模型成立後，剩餘 vocabulary、status surface 與 `hcodex` launch shim 的收尾工作
- `hcodex-lifecycle-supervision`
  - 記錄 `hcodex` local launcher / child supervision / teardown contract
- `hcodex-responsibility-matrix`
  - 記錄 `hcodex` core 的長期責任矩陣與邊界分類
- `working-session-observability`
  - 描述 working session 的可觀測入口、session timeline 與 artifact 關聯
- `topic-title-status`
  - 描述 title 應承載哪些狀態
- `runtime-state-machine`
  - 最終應把上面幾份文件的狀態語義統一
- `runtime-protocol-convergence`
  - 描述 `runtime-protocol` 從 read-side / SSE 雛形，收斂到完整 control / interaction contract 的 rollout 順序

## 備註

- 這個目錄現在同時包含已落地方案和未實作草稿，不能只看標題判斷成熟度。
- 如果某份文檔和代碼有衝突，先以代碼為準，再回來更新該文檔。
