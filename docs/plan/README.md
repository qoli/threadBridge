# Plan Registry

這個目錄是 `threadBridge` 的設計註冊表。

它不是單純 backlog，也不是只放未來草稿；它同時記錄：

- 已落地的當前方案
- 正在收斂的部分落地方案
- 已退役但仍有參考價值的歷史方案
- 尚未實作的設計草稿

如需新增新想法或整理既有 plan，先看 [authoring-guide.md](authoring-guide.md)。
如需先對齊角色邊界與 ownership，再看 [runtime-architecture.md](runtime-control/runtime-architecture.md)。

## 閱讀方式

- 這份 registry 只用 `status` 分組：`已落地` / `部分落地` / `已退役` / `純草稿`
- owner 由文檔所在 folder 表達，不再由 README 手填重複記錄
- `unknown-owner/` 是尚未安全掛靠文檔的 quarantine owner，不是新的成熟度分組
- `doc kind`、`primary spec`、`depends_on`、`current answer` 都只是條目屬性，不再另開分組
- `doc kind` 只回答這是 `spec`、`plan` 還是 `historical`，不替代成熟度
- 若單篇文檔和代碼衝突，先以代碼為準，再回來修文檔

若你要快速定位：

- 看角色邊界：先讀 [runtime-architecture.md](runtime-control/runtime-architecture.md)
- 看狀態語義：先讀 [runtime-state-machine.md](runtime-control/runtime-state-machine.md)
- 看 Telegram delivery：先讀 [message-queue-and-status-delivery.md](telegram-adapter/message-queue-and-status-delivery.md)

## 已落地

- [session-level-mirror-and-readiness.md](runtime-control/session-level-mirror-and-readiness.md)
  - doc kind: `plan`
  - shared app-server daemon、`./.threadbridge/bin/hcodex`、hcodex ingress、mirror、adoption、auto-adopt 已落地
  - desktop runtime 已成為正式 owner 啟動模型，headless 啟動路徑已退場
  - `hcodex` self-heal 已移除，缺少 desktop owner 時會明確失敗
  - workspace heartbeat / runtime health 已改成以 desktop owner heartbeat 為主 authority
  - 舊 `CLI owner / handoff` 概念已退出現行模型，主語義改為 local/TUI mirror + idle/free readiness
  - process transcript 已正式區分 final / process，並補上 management transcript read API、session summary / records API、web observability pane，以及 Telegram rolling preview 摘要
  - `codex plan` mirror、plan-only final reply fallback、Telegram preview process transcript 已落地
  - Telegram `Questions` / `Implement this plan` 已改成 observer / ingress 發出 adapter-neutral interaction event，再由 Telegram interaction bridge 消費

## 已退役

- [codex-cli-telegram-status-sync-hooks.md](runtime-control/codex-cli-telegram-status-sync-hooks.md)
  - doc kind: `historical`
  - current answer: [runtime-architecture.md](runtime-control/runtime-architecture.md), [runtime-state-machine.md](runtime-control/runtime-state-machine.md), [message-queue-and-status-delivery.md](telegram-adapter/message-queue-and-status-delivery.md)
  - 已完成 v1
  - Bash wrapper、Codex hooks、notify、workspace shared status、topic title watcher、busy gate 都曾落地
  - 現在已退役，只保留作為舊模型參考
- [hcodex-pre-refactor-history.md](hcodex-local-ingress-launcher/hcodex-pre-refactor-history.md)
  - doc kind: `historical`
  - current answer: [hcodex-launch-contract.md](hcodex-local-ingress-launcher/hcodex-launch-contract.md), [hcodex-lifecycle-supervision.md](hcodex-local-ingress-launcher/hcodex-lifecycle-supervision.md), [hcodex-responsibility-matrix.md](hcodex-local-ingress-launcher/hcodex-responsibility-matrix.md)
  - 記錄重構前 `hcodex` / shell wrapper / `codex_sync.py` 的歷史模型
  - 固定「舊模型雖髒，但本地 `codex` child lifecycle 閉環較強」這個背景結論

## 部分落地

- [runtime-architecture.md](runtime-control/runtime-architecture.md)
  - doc kind: `spec`
  - primary spec: `yes`
  - current architecture 的角色與責任主文檔
  - 固定 `desktop runtime owner`、shared `runtime_control`、observer、`hcodex`、Telegram adapter、management / desktop surface 的邊界
  - 已補記 shared `DeliveryBusCoordinator` 是 `runtime_control` 子域，而不是新的 canonical actor
  - 目前沒有 active temporary exception；已補記 observer final reply composition 例外已退出 active list
  - 若未來再出現跨層捷徑，需在主文檔明確登記 code anchor、必要性與退出方向
- [runtime-responsibility-drift-audit.md](runtime-control/runtime-responsibility-drift-audit.md)
  - doc kind: `plan`
  - depends_on: [runtime-architecture.md](runtime-control/runtime-architecture.md)
  - 以 `runtime-architecture` 為中心的 current-code drift audit
  - 目前已確認 5 個 responsibility drift 功能點，其中 `status_sync` 的 TUI mirror draft write/heartbeat 已升格為 active drift
- [runtime-state-machine.md](runtime-control/runtime-state-machine.md)
  - doc kind: `spec`
  - primary spec: `yes`
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
- [runtime-protocol.md](runtime-control/runtime-protocol.md)
  - doc kind: `spec`
  - 本地 management API 已開始承接它的 view / action 命名
  - local HTTP + SSE 已從草稿變成實際 transport
  - 近期已再補上 runtime-owner reconcile、managed Codex build defaults、workspace launch config、continue-current launch control、thread transcript read API，以及 session summary / session records read API
  - `POST /api/threads/:thread_key/actions` 已開始對外承接 `set_thread_collaboration_mode` 與 `interrupt_running_turn`
  - `GET /api/threads` 已開始對外暴露 canonical `lifecycle_status`，並補齊 `chat_id` / `message_thread_id` / `session_broken_reason` / `last_verified_at` / `last_codex_turn_at`
  - `ThreadStateView` / `ManagedWorkspaceView` 已開始暴露 `current_collaboration_mode`
  - runtime health 已改成 owner-canonical，`workspace_state` 僅保留 debug/observation 語義
  - `runtime_protocol` 共享 view builder 已開始把 `ThreadStateView` / `ManagedWorkspaceView` / `ArchivedThreadView` / `RuntimeHealthView` / `WorkingSession*View` 從 transport 層抽離
  - broken thread count、workspace recovery hint、以及 working session broken error 聚合，已開始改以 canonical `binding_status` 判定
  - public view 已開始移除 `session_broken` / `last_error` alias；對外主判斷改固定收斂到 `binding_status` / `run_status` / `session_broken_reason`
  - `GET /api/events` 已開始輸出 typed SSE event，而不是每輪都推整包 snapshot
  - `managed_codex_changed` 已開始作為獨立 typed SSE event 發出
  - web UI 已開始直接套用 top-level typed SSE payload，並只對 transcript / sessions 做 targeted refetch
  - 但 protocol 仍未收斂成完整 transport-neutral 契約，尤其更細的 observability record 仍未走完整增量 event 模型
- [runtime-protocol-convergence.md](runtime-control/runtime-protocol-convergence.md)
  - doc kind: `plan`
  - `runtime-protocol` 的 rollout / convergence 草稿
  - `RuntimeControlActionRequest` / `Result` / `Envelope` 已補上 `set_thread_collaboration_mode` 與 `interrupt_running_turn`
  - `RuntimeInteractionKind` 已進入 shared protocol vocabulary，`RequestUserInput` / `RequestResolved` / `TurnCompleted` 也已有 shared event lane
  - management action route 與 Telegram slash command 已開始對齊 `start_fresh_session`、`repair_session_binding`、`set_workspace_execution_mode`、`launch_local_session`、`set_thread_collaboration_mode`、`interrupt_running_turn`
  - 但 interaction event 仍是平行 typed family，adopt/reject/archive/restore 等 capability 也還沒完全收進 unified action route
- [session-lifecycle.md](runtime-control/session-lifecycle.md)
  - doc kind: `plan`
  - depends_on: [runtime-architecture.md](runtime-control/runtime-architecture.md), [runtime-state-machine.md](runtime-control/runtime-state-machine.md)
  - `/add_workspace`、`/new_session`、`/repair_session` 的正式生命週期已存在
  - `current_codex_thread_id` 已成為 canonical pointer，`tui_active_codex_thread_id` / adoption 也已進入正式 runtime
  - Telegram thread 內的一般輸入、圖片分析、session-control gate、以及 stale busy reconciliation 已開始直接讀 canonical state
  - `current_codex_thread_id` 的存在已不再被視為等於「目前一定可直接 resume」；usable continuity 仍取決於 canonical `binding_status`
  - canonical continuity mutation 已開始透過 repository 內部的共用 transition path 收斂
  - workspace runtime ensure、session bind/new/repair、以及 Telegram-to-live-TUI routing 已進一步抽成 shared `runtime_control` service
  - 已新增記錄：Telegram desktop launch command 應作為獨立 control surface，而不是改寫 `/new_session`
  - 剩餘工作主要是兼容層與狀態語義收尾
- [codex-busy-input-gate.md](runtime-control/codex-busy-input-gate.md)
  - doc kind: `plan`
  - depends_on: [runtime-state-machine.md](runtime-control/runtime-state-machine.md)
  - v1 忙碌閘控已落地
  - Telegram 文字 turn / 圖片分析已改成 background 執行，後續普通文字依 `running_input_policy` 走 `reject | queue | steer`
  - `/stop` 已作為第一個正式 busy control action 落地，並開始使用 session turn id 走 app-server interrupt
  - bot 啟動時的 stale busy reconciliation 已開始落地
  - 已新增 Telegram running input policy 設計：`reject | queue | steer`，預設為 `steer`；`steer` 對應 app-server `turn/steer`，`queue` 需 one-slot pending input 與取消/覆蓋語義
  - 已新增記錄一個明確 bug：斜線命令 `STOP` / `/stop` 後，thread 目前可能卡到需要重啟 bot 才恢復響應
  - 但 queue 模型、更完整的狀態語義、`STOP 並插入發言` / `序列發言` 這類 follow-up 控制面、更乾淨的 ingress / dispatcher 邊界，以及更完整的 stale busy owner 模型仍未收斂
- [codex-execution-modes.md](runtime-control/codex-execution-modes.md)
  - doc kind: `plan`
  - execution mode 已進入正式 runtime 模型，不再只是草稿命題
  - workspace-local `workspace-config.json`、`ExecutionMode` enum、session execution snapshot 已落地
  - management API / launch-config / web UI 已開始暴露 workspace mode、current session mode 與 `mode_drift`
  - `hcodex` 與 Telegram turn/resume 已開始按 workspace mode 收斂到 `full_auto` 或 `yolo`
  - Telegram 已補上 `/get_workspace_execution_mode` / `/set_workspace_execution_mode` command surface，但 user-facing naming、owner vocabulary、以及 `Codex 工作模型` / 自定義 Codex config 是否與 mode 分離對外暴露，仍未收斂
- [runtime-data-root.md](runtime-control/runtime-data-root.md)
  - doc kind: `plan`
  - bot-local runtime state 已有 shared `data_root_path` plumbing，但預設值原本仍寫死 repo-local `./data`
  - 現在已固定成雙模式：debug build 預設 `./data`，release build 預設平台 local app-data dir，且兩者不搬移、不複製、不互相探測
  - `DATA_ROOT` / `DEBUG_LOG_PATH` override 與 `BOT_DATA_PATH` compatibility 已被保留；README、helper script 與 maintainer guide 也已改成 mode-aware 語義
- [workspace-runtime-surface.md](runtime-control/workspace-runtime-surface.md)
  - doc kind: `plan`
  - `.threadbridge/`、workspace-local runtime skill source、`.codex/skills/threadbridge-runtime` discovery symlink、`hcodex`、tool request/result lane 已形成實際 workspace runtime surface
  - 但按 project type / workspace profile 選擇啟用 tools 的模型仍未收斂
- [workspace-runtime-skill.md](runtime-control/workspace-runtime-skill.md)
  - doc kind: `plan`
  - baseline 已落地：runtime capability documentation 由 `.threadbridge/skills/threadbridge-runtime/` 承載，並透過 `.codex/skills/threadbridge-runtime` symlink 讓 Codex 發現；普通 workspace ensure 不再注入 project `AGENTS.md`
- [post-cli-runtime-cleanup.md](runtime-control/post-cli-runtime-cleanup.md)
  - doc kind: `plan`
  - CLI 時代的大部分 launch / vocabulary cleanup 已開始落地
  - 但 `hcodex` launch contract 仍保留 `launch_ticket + local hcodex-ws-bridge` compatibility boundary，不能再被誤判成可直接刪除
  - `workspace_status` 已補上 legacy `shared-runtime/*` / `local-session.json` migrate-read 與 canonical write-path 測試
  - 但 `workspace_status` / public naming / legacy compatibility policy 的 broader 收尾仍未完成
- [codex-plan-mirror.md](app-server-observer/codex-plan-mirror.md)
  - doc kind: `plan`
  - `codex plan` mirror 子規格
  - upstream `item/plan/delta` / finalized `plan` item 已確認存在
  - `threadBridge` 已消費 live `item/plan/delta`，並補上 plan-only final reply fallback
  - 已新增記錄一個明確 bug：mirror 路徑下，`proposed_plan` / finalized plan 仍可能對使用者不可見
- [app-server-ws-mirror-observer.md](app-server-observer/app-server-ws-mirror-observer.md)
  - doc kind: `plan`
  - local/TUI mirror intake 已從 `hcodex ingress` 拆到獨立 app-server ws observer
  - observer 已不再直接做 Telegram interactive glue，而是發出 shared runtime interaction event
  - 但 public vocabulary、transport-neutral observer contract、以及 broader observability 收斂仍未完成
- [telegram-markdown-adaptation.md](telegram-adapter/telegram-markdown-adaptation.md)
  - doc kind: `plan`
  - final reply 的 Telegram HTML renderer、plain-text fallback、attachment fallback 已落地
  - `reply.md` attachment 的 Telegram 文件大小 preflight 與 warning fallback 已開始落地
  - 但更完整的 artifact / URL fallback 仍待收斂
- [topic-title-status.md](telegram-adapter/topic-title-status.md)
  - doc kind: `plan`
  - 已落地 `workspace/title + broken suffix`
  - 已有獨立的 conversation-based title generation flow，且新產生的 topic rename service message 會 best-effort cleanup
  - 尚未把 `自動生成` / `使用資料夾名稱` 收斂成正式的 Telegram inline title-source control surface
  - context ratio 仍未實作
- [message-queue-and-status-delivery.md](telegram-adapter/message-queue-and-status-delivery.md)
  - doc kind: `spec`
  - primary spec: `yes`
  - Telegram outbound delivery 主規格已從純草稿進入部分落地
  - workspace outbox `surface`、最小檔案大小 preflight、photo -> document fallback、以及 oversized attachment/document warning path 已開始落地
  - 用戶 -> bot 的文件輸入仍未正式支持；bot -> 用戶的 attachment / outbox document 已存在但仍屬局部 delivery path
  - workspace outbox v1 目前只正式承諾 `content` / `status`；其他 `surface` 仍是保守兼容值
  - local/TUI mirror draft 已開始保留 upstream `turn_id`，並用 turn-bound draft claim 做單 turn 去重
  - 已新增記錄一個明確責任債務：`status_sync` 目前仍同時承擔 mirror consume、draft write、與 heartbeat
  - 但 outbound queue、完整 control lifecycle、artifact 類型與集中化 config 仍未收斂
- [telegram-adapter-migration.md](telegram-adapter/telegram-adapter-migration.md)
  - doc kind: `plan`
  - Telegram adapter 遷移草稿
  - owner authority 與 shared runtime control 已先從 Telegram 路徑抽離，再做更完整的 adapter migration
  - 近期優先級應是補齊 Telegram 自身適配，而不是先做第二個 IM adapter 驗證
  - Telegram collaboration mode slash commands、`/launch_local_session`、`/get_workspace_execution_mode`、`/set_workspace_execution_mode`、`/sessions`、`/session_log`、`/stop`，以及最小 `request_user_input` / `Implement this plan` 互動面已先行落地，且互動 UI 已改成 adapter-owned bridge
  - `current_collaboration_mode` 已持久化到 session binding，且 management / runtime public view 也已開始同步暴露
  - 近期 Telegram v0 剩餘能力面主要包括 Codex 工作模型設定、更完整的 Busy Gate follow-up control surface，以及 `main chat = control 面板` 下的 `forwarded input`
- [macos-menubar-thread-manager.md](management-desktop-surface/macos-menubar-thread-manager.md)
  - doc kind: `plan`
  - `threadbridge_desktop`、macOS-first tray menu、workspace-first browser management UI 已開始落地
  - pick-and-add、adopt / reject TUI、runtime-owner reconcile、launch config 等 control 已進入 management API
  - managed Codex source build / cache refresh / build defaults 已進入 management API
  - tray menu 已收斂成 `New Session` 與 `Continue Telegram Session`
  - `threadbridge_desktop` 已開始以 menubar-only 形態啟動，bundle `LSUIElement` 與 runtime `Accessory` activation policy 已落地
  - tray workspace label 現在已改成顯示 workspace execution mode，而不是 handoff `ready/degraded` 文案
  - management health view 已改成 owner heartbeat 為主的 desktop-first 模型
  - management UI 已補上 transcript observability pane、workspace-card `Sessions` pane 與 inline records timeline，且 adoption/repair action 已改成 owner-canonical 語義
  - management UI 已補上 workspace execution mode 切換、mode drift 提示，以及 mode-aware launch/resume commands
  - web 管理面新增確認的 UI 收斂方向是本地 vendored、無 build 的 Tabler 風格 CSS 重構，且 dark mode 固定跟隨系統
  - 目前新增確認的收斂方向是 `workspace = thread` 主模型、desktop-only 啟動、可選 `Launch at Login`，以及移除暫不可用的 onboarding
- [working-session-observability.md](management-desktop-surface/working-session-observability.md)
  - doc kind: `plan`
  - desktop runtime / web 管理面的 session 級 observability 已進入部分落地
  - `WorkingSessionSummaryView` / `WorkingSessionRecordView`、`GET /api/threads/:thread_key/sessions`、`GET /api/threads/:thread_key/sessions/:session_id/records` 已落地
  - management UI 已可在 workspace card 的 `Sessions` pane 中直接打開 session timeline
  - artifact refs、獨立 observability page、retention / redaction 邊界仍未收斂
- [owner-runtime-contract.md](desktop-runtime-owner/owner-runtime-contract.md)
  - doc kind: `plan`
  - owner/runtime boundary 的高層背景與收斂草稿
  - 角色與責任邊界現在以 `runtime-architecture.md` 為主文檔
  - 但 `hcodex` 長期 contract、observer attach contract、與 transport-neutral protocol 仍未完全收斂
- [app-server-ws-backend.md](desktop-runtime-owner/app-server-ws-backend.md)
  - doc kind: `plan`
  - 描述 `app-server-ws-backend` 作為 `desktop runtime owner` 受管 backend plane 的 today reality 與 target vision
  - 固定它是 `threadBridge` today runtime 的核心 Codex backend substrate，且 workspace-scoped backend worker 已落地為 `app_server_ws_worker`
  - 補清它與 owner、shared `runtime_control`、observer、`hcodex ingress`、Telegram / management surface 的邊界
  - 已新增記錄一條方向：若 workspace ws runtime 長期預先 ensure，`threadbridge_desktop` 佔用可能過高，後續應評估按需啟動
- [app-server-ws-backend-progress-2026-03-27.md](desktop-runtime-owner/app-server-ws-backend-progress-2026-03-27.md)
  - doc kind: `historical`
  - backend worker 主線進度快照（2026-03-27）
  - 匯總 worker-first run authority、interaction response/ingress 下沉、local runtime helper 收斂，以及 worker mode 驗證 runbook
- [app-server-ws-backend-progress-2026-03-28.md](desktop-runtime-owner/app-server-ws-backend-progress-2026-03-28.md)
  - doc kind: `historical`
  - observer contract 收斂進度快照（2026-03-28）
  - 記錄 `subscribeThread/unsubscribeThread` 落地、`observeThread` 移除、以及顯式 detach + timeout fallback
- [macos-public-release-track.md](desktop-runtime-owner/macos-public-release-track.md)
  - doc kind: `plan`
  - macOS public release 已不再只是純草稿：release data-root gate 已落地，repo 已新增 `scripts/release_threadbridge.sh`
  - 固定 `local_threadbridge.sh = dev helper`、`release_threadbridge.sh = shell release orchestrator`
  - 已新增 `scripts/release_rc.sh` 作為日常 RC wrapper，並已有 `0.1.0-rc.2` replacement RC notes
  - 私有 ignored fastlane 只作 Apple bootstrap / `match` helper，不再承擔正式 notarize happy path
  - 第一輪 RC 先收斂到 GitHub draft prerelease；Homebrew tap 仍待 dedicated repo 建立後再補回
  - 2026-03-31 broken release blocker 已有 root-cause 方向與 replacement RC 路徑；目前仍待 replacement artifact smoke、GitHub draft 發佈驗證與結果回寫
- [hcodex-launch-contract.md](hcodex-local-ingress-launcher/hcodex-launch-contract.md)
  - doc kind: `plan`
  - 記錄 `hcodex` launch URL、local bridge、upstream Codex `--remote` 的實際契約
  - 明確固定兩個已修回歸：`invalid remote address ...?launch_ticket=...` 與 `failed to connect to remote app server`
- [hcodex-lifecycle-supervision.md](hcodex-local-ingress-launcher/hcodex-lifecycle-supervision.md)
  - doc kind: `plan`
  - 記錄 `hcodex` 作為受管本地入口時，對 local Codex child lifecycle 的正式責任
  - 固定目前已確認的缺口：cleanup 不能只依賴 `run-hcodex-session` 的 happy path
- [hcodex-responsibility-matrix.md](hcodex-local-ingress-launcher/hcodex-responsibility-matrix.md)
  - doc kind: `plan`
  - 將 `hcodex` 的長期責任收斂成 4 個核心詞：`launch / bridge / supervise / reconcile`
  - 區分哪些責任必須保留在 core、哪些只能暫時保留在周邊、哪些應移出 `hcodex` core

## 純草稿

- [app-server-observer-upstream-capability-audit.md](app-server-observer/app-server-observer-upstream-capability-audit.md)
  - doc kind: `plan`
  - 盤點 `/Volumes/Data/Github/codex` 內 upstream app-server observer 相關原生能力
  - 作為後續切分 `AppServerMirrorObserverManager` 前的 capability audit，而不是直接的重構方案
- [telegram-webapp-observability.md](telegram-adapter/telegram-webapp-observability.md)
  - doc kind: `plan`
  - Telegram Web App 觀測面草稿
  - 本地 session-first observability API 與 workspace-card Sessions pane 已落地，但 Telegram Web App 本身仍未開始
  - 由於 Telegram Web App 依賴 HTTPS，近期已降為遠期可選載體，不再是本地 observability 的主路徑
- [multi-bot-token-support.md](telegram-adapter/multi-bot-token-support.md)
  - doc kind: `plan`
  - Telegram adapter 多 bot token 能力草稿
  - 記錄目前單一 `TELEGRAM_BOT_TOKEN` / 單 bot polling / 單 bot setup 模型的限制
  - 已補記一個明確產品判斷：`threadBridge` 應正式支持多個 Telegram bot token
  - 聚焦 machine-local bot registry、per-bot authorized users、thread bot identity 與 management setup 收斂方向
- [codex-agent-install-and-first-setup.md](management-desktop-surface/codex-agent-install-and-first-setup.md)
  - doc kind: `plan`
  - 給外部 Codex agent 的安裝與首次 Telegram setup 引導草稿
  - 聚焦 desktop-first 啟動、management setup checkpoint、control chat ready，與 first workspace bind 的最短成功路徑
- [llm-guidance-and-goals.md](unknown-owner/llm-guidance-and-goals.md)
  - doc kind: `plan`
  - 目前掛在 `unknown-owner/`，作為尚未安全掛靠到 canonical role 的 quarantine 草稿
  - secondary LLM 設定、AI 建議與 AI 目標層草稿
- [desktop-runtime-tool-bridge.md](desktop-runtime-owner/desktop-runtime-tool-bridge.md)
  - doc kind: `plan`
  - desktop runtime 作為跨沙盒 capability host / tool bridge / 自定義 webview service 草稿
- [optional-agents-injection.md](runtime-control/optional-agents-injection.md)
  - doc kind: `plan`
  - appendix 注入可選化草稿
- [runtime-support-installer-refactor.md](runtime-control/runtime-support-installer-refactor.md)
  - doc kind: `plan`
  - runtime support seed、workspace installer、legacy migration 邊界重整草稿
  - 目標是把 runtime artifact layout 變更限制在 manifest / installer，而不是牽動 owner、control、management 與 adapter 層
- [runtime-transport-abstraction.md](runtime-control/runtime-transport-abstraction.md)
  - doc kind: `plan`
  - core runtime / adapter 抽象化草稿
  - owner 收斂與 shared control core 都應視為這條抽象化路線的已落地前置工作
  - 近期應先服務 Telegram 路徑收斂，而不是直接追求多 IM / 多 adapter 產品化

## 備註

- 這個 registry 只允許 `status` 作為分組軸。
- owner 由 folder path 表達；README 不再重複維護 `owner role` 欄位。
- `unknown-owner/` 是 owner quarantine，不是 status bucket。
- `doc kind`、`primary spec`、`depends_on`、`current answer` 都是條目屬性。
- 如果某份文檔和代碼有衝突，先以代碼為準，再回來更新該文檔。
