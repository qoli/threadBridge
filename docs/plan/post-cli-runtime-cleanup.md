# Post-CLI Runtime Cleanup 草稿

## 目前進度

這份文檔目前已進入「部分落地」。

目前已確認：

- `threadBridge` 的 canonical runtime 已不是舊 CLI / hook 模型，而是 `desktop runtime owner + shared app-server + owner-canonical runtime health`
- 舊 CLI viewer、attach intent plumbing、`SessionAttachmentState` 正式欄位已被移除
- 但在 `workspace_status`、`runtime_protocol`、`hcodex` 啟動鏈、以及少量 repository compatibility 上，仍可看見 CLI / handoff 時代延續下來的語義與中間 shim
- Phase 1 已開始落地 vocabulary/public-surface 收斂：
  - public runtime health field 已從 `handoff_readiness` 改為 `runtime_readiness`
  - `workspace_status` 內部已開始把 `SessionStatusOwner` 收斂為非 ownership 語義的 `SessionActivitySource`
  - 舊序列化值 `bot` / `local` 與 `live_local_session_ids` 仍保留 deserialize compatibility
- Phase 2 已開始落地 launch / proxy cleanup：
  - `resolve_hcodex_launch.py` 已移除
  - `hcodex-ws-bridge` 已移除
  - workspace runtime state 已從 `tui_proxy_base_ws_url` 收斂到 `hcodex_ws_url`
  - `hcodex` launch contract 已改成 canonical ws endpoint + one-shot `launch_ticket`
  - local/TUI mirror intake 已開始從 proxy relay 熱路徑拆到獨立 app-server observer

目前尚未完成：

- 尚未把這些遺留分成「應保留的 local TUI core」與「應被移除或重命名的過渡語義」
- 尚未完全把 `runtime-observer` 狀態面、`local-tui-session.json`、以及 broader public vocabulary 收斂成更符合 owner-managed app-server runtime 的模型
- 尚未完成 launch-surface / observer 邊界的 public vocabulary 與 documentation rename 收尾

## Phase 1 目標終態

Phase 1 只處理 vocabulary、public surface、文檔對齊與 legacy read compatibility。

這一階段完成時應滿足：

- public/runtime-facing surface 不再使用 `handoff_readiness`
- management UI、desktop summary、runtime protocol 統一使用 `runtime_readiness`
- `workspace_status` 不再以 `SessionStatusOwner::{Local, Bot}` 描述 ownership，而改以 `SessionActivitySource` 描述 activity source
- `runtime-observer` 在文檔上被明確定位為 workspace-local observation surface，而不是 canonical authority
- legacy serialized fields 只保留 read compatibility，不重新出現在 write path 或 public payload

## Phase 1 compatibility policy

Phase 1 對 legacy field / artifact 採用保守但有界的策略：

- `SessionBinding`
  - 仍接受舊欄位 `codex_thread_id` / `selected_session_id`
  - 讀取後一律 normalize 到 `current_codex_thread_id`
  - 寫回時不再重新序列化舊欄位
- `workspace_status` session snapshots
  - 仍接受舊值 `owner=local|bot`
  - 讀取後一律映射到 `activity_source=local_tui|managed_runtime`
  - 新寫入只使用 `activity_source`
- `workspace aggregate`
  - 仍接受 `live_local_session_ids`
  - 新寫入只使用 `live_tui_session_ids`
- legacy attachment/handoff fields
  - repository 仍可 best-effort 忽略 `attachment_state`
  - 不重新引入 attachment/handoff public semantics

這表示 Phase 1 的原則是：

- 只讀舊格式
- 不寫回舊格式
- 不讓舊詞重新回到 public payload、UI、或主文檔語義

## Phase 1 明確不做（歷史）

- 不改 `tui_proxy_base_ws_url`
- 不改 `/thread/<thread_key>` sideband
- 不處理 `hcodex-ws-bridge` 存廢
- 不把 mirror intake 從 `TUI proxy` 拆到獨立 observer

以上限制只適用於當時的 Phase 1。後續實作已經跨入下一階段，這些項目現在都已開始被清理。

## 問題

目前 `threadBridge` 的 runtime 核心已經完成了一次大方向轉換：

- 正式 owner 已是 desktop runtime
- Telegram 已不再是 runtime owner
- workspace canonical backend 已是 shared `codex app-server`

但架構上仍有一批 read-side / control-side / launch-side 的殘留，仍在使用比較接近舊 CLI 時代的心智模型。

這些殘留不一定會立刻造成錯誤，但會持續帶來幾個問題：

- 文件和代碼會同時描述「owner-managed runtime」與「local/bot handoff」兩套語言
- 管理面與狀態面容易把 observation surface 誤認成 runtime authority
- `hcodex` 啟動鏈看起來比實際需要更複雜，讓 transport shape 像是 workaround 疊加
- 未來若要繼續做 adapter/core 邊界收斂，會先被舊命名與舊狀態模型拖住

所以這份文檔記錄的不是單一 bug，而是：

- 在 shared app-server / desktop owner 方案已成立後，哪些架構層仍保留 CLI 時代遺留語義

## 目前觀察到的遺留類型

### 1. 狀態模型仍帶有 CLI / local ownership 語義

最明顯的是 [`workspace_status.rs`](/Volumes/Data/Github/threadBridge/rust/src/workspace_status.rs)。

目前主寫入狀態面已收斂為：

- `.threadbridge/state/runtime-observer`
- `SessionActivitySource::{Tui, ManagedRuntime}`（兼容讀舊值 `local` / `bot`）
- `local-tui-session.json`
- `LocalSessionClaim`

具體可見：

- [`workspace_status.rs`](/Volumes/Data/Github/threadBridge/rust/src/workspace_status.rs#L15)
- [`workspace_status.rs`](/Volumes/Data/Github/threadBridge/rust/src/workspace_status.rs#L23)
- [`workspace_status.rs`](/Volumes/Data/Github/threadBridge/rust/src/workspace_status.rs#L91)

但目前仍保留 legacy read compatibility：

- `.threadbridge/state/shared-runtime`
- `local-session.json`

目前殘留的命名與資料模型更像是在描述：

- 本地 TUI / CLI 與 bot 誰持有 session

而不是在描述：

- desktop owner 管理下的 workspace runtime activity / observation surface

這表示 CLI 時代的 ownership vocabulary 雖然已不再是正式架構，但在 read-side status surface 上仍未完全退場。

### 2. 管理面曾以 `handoff_readiness` 作為主語義

這個缺口已在 Phase 1 開始收斂：[`runtime_protocol.rs`](/Volumes/Data/Github/threadBridge/rust/src/runtime_protocol.rs) 的 public runtime health 欄位已改成 `runtime_readiness`，同時保留既有 `ready` / `pending_adoption` / `degraded` / `unavailable` 狀態值。

具體可見：

- [`runtime_protocol.rs`](/Volumes/Data/Github/threadBridge/rust/src/runtime_protocol.rs#L29)
- [`runtime_protocol.rs`](/Volumes/Data/Github/threadBridge/rust/src/runtime_protocol.rs#L309)
- [`runtime_protocol.rs`](/Volumes/Data/Github/threadBridge/rust/src/runtime_protocol.rs#L720)
- [`runtime_protocol.rs`](/Volumes/Data/Github/threadBridge/rust/src/runtime_protocol.rs#L808)

在 today 的模型裡，這個詞已經有些語義偏移，因為：

- authority 是 desktop owner heartbeat
- canonical backend 是 shared app-server
- `hcodex` ingress 是 `hcodex` 專用 bridge，不是 handoff owner

所以這裡比較像是從「CLI handoff」重寫成「mirror/readiness」時，只換了一部分邏輯，沒有完成 vocabulary migration。

### 3. `hcodex` 啟動鏈仍保留多層 transition shim

目前 `hcodex` 的啟動主路徑已收斂為「找到 canonical ws endpoint 然後帶 `launch_ticket` 連上」：

1. `ensure-hcodex-runtime`
2. Rust launch resolver 取得 `hcodex_ws_url` 與一次性 `launch_ticket`
3. `run-hcodex-session`

具體可見：

- [`workspace.rs`](/Volumes/Data/Github/threadBridge/rust/src/workspace.rs#L69)
- [`workspace.rs`](/Volumes/Data/Github/threadBridge/rust/src/workspace.rs#L151)
- [`workspace.rs`](/Volumes/Data/Github/threadBridge/rust/src/workspace.rs#L183)
- [`workspace.rs`](/Volumes/Data/Github/threadBridge/rust/src/workspace.rs#L210)
- [`hcodex_runtime.rs`](/Volumes/Data/Github/threadBridge/rust/src/hcodex_runtime.rs#L36)
- [`app_server_runtime.rs`](/Volumes/Data/Github/threadBridge/rust/src/app_server_runtime.rs#L29)

這個形狀本身不一定錯，但它很像：

- shared app-server ws 遷移完成後，曾為兼容既有 `hcodex` 接入與 proxy path sideband 而保留下來的一串過渡式 shim；目前主路徑已完成 clean cutover，但文檔與少量 compatibility 描述仍待收尾

它的問題不是功能不能用，而是：

- transport shape 不夠 canonical
- `hcodex` 對真正 runtime contract 的依賴被包在多層 launcher / resolver / bridge 裡

### 4. repository 仍保留少量 attachment / handoff compatibility 尾巴

repository 主模型大致已切到現在的 session binding，但仍保留了一個明確的 legacy compatibility 測試：

- [`repository.rs`](/Volumes/Data/Github/threadBridge/rust/src/repository.rs#L1533)

它仍接受舊欄位：

- `attachment_state = "local_handoff"`  
  [`repository.rs`](/Volumes/Data/Github/threadBridge/rust/src/repository.rs#L1548)

這說明 attachment / handoff 語義已不是正式模型，但其資料痕跡仍是 today deserialization compatibility 的一部分。

## 目前可把遺留分成三種強度

如果只問「CLI 的影響還剩多少」，更準確的回答不應是單一百分比，而應是看它殘留在哪一層。

### A. 深層、仍主動影響 today runtime 語義的遺留

這一層不是單純名字沒改，而是仍直接影響 today 的控制與狀態判斷。

- `workspace_status` 的 legacy `shared-runtime/*` 讀兼容、`local-tui-session.json` / legacy `local-session.json`、`LocalSessionClaim`
- `tui_active_codex_thread_id`、adoption pending、以及 Telegram 下一次輸入時的 auto-adopt 流程
- Telegram 仍會透過 local-session claim 去判斷是否應把輸入改接到 live TUI session

具體可見：

- [`workspace_status.rs`](/Volumes/Data/Github/threadBridge/rust/src/workspace_status.rs#L15)
- [`workspace_status.rs`](/Volumes/Data/Github/threadBridge/rust/src/workspace_status.rs#L94)
- [`telegram_runtime/mod.rs`](/Volumes/Data/Github/threadBridge/rust/src/telegram_runtime/mod.rs#L975)
- [`telegram_runtime/mod.rs`](/Volumes/Data/Github/threadBridge/rust/src/telegram_runtime/mod.rs#L1001)

這代表 CLI/handoff 時代真正還活著的核心，不是 `codex-cli` 這個詞，而是：

- `本地 TUI session` 與 `Telegram canonical continuity` 之間仍存在一套顯式接管 / adoption 心智模型

### B. 中層、主要存在於 transport / launch 鏈上的 transition shim

這一層比較像 shared app-server ws 模型落地後，為了讓 `hcodex` 入口穩定工作而保留的過渡式包裝。

- 這條 transition shim 的主路徑已被移除
- 目前正式 launcher 只串接：
  - `ensure-hcodex-runtime`
  - Rust launch resolver
  - `run-hcodex-session`
- 舊的 `resolve_hcodex_launch.py`、`hcodex-ws-bridge`、以及 `tui_proxy_base_ws_url` sideband 已退回歷史/compatibility 語境

具體可見：

- [`workspace.rs`](/Volumes/Data/Github/threadBridge/rust/src/workspace.rs#L69)
- [`workspace.rs`](/Volumes/Data/Github/threadBridge/rust/src/workspace.rs#L150)
- [`workspace.rs`](/Volumes/Data/Github/threadBridge/rust/src/workspace.rs#L181)
- [`hcodex_runtime.rs`](/Volumes/Data/Github/threadBridge/rust/src/hcodex_runtime.rs#L36)
- [`app_server_runtime.rs`](/Volumes/Data/Github/threadBridge/rust/src/app_server_runtime.rs#L29)

這類殘留的意思比較像：

- runtime backend 已經是對的
- 但 local entrypoint 還沒有拿到最乾淨、最 canonical 的 launch contract

### C. 淺層、主要屬於 compatibility / fallback 的尾巴

這一層仍值得記錄，但不應被誤判成目前最主要的架構債。

- `SessionBinding` 仍接受舊欄位 `selected_session_id` / `codex_thread_id`
- repository 仍忽略舊 `attachment_state = local_handoff`
- `CodexRunner` 仍保留 `app_server_url=None -> stdio app-server` 的 fallback
- Telegram runtime 仍保留 `SelfManaged` mode，而不是只有 desktop owner 模式
- `min_chat_probe` 仍使用 `CodexWorkspace { app_server_url: None }` 走 stdio probe path

具體可見：

- [`repository.rs`](/Volumes/Data/Github/threadBridge/rust/src/repository.rs#L240)
- [`repository.rs`](/Volumes/Data/Github/threadBridge/rust/src/repository.rs#L1535)
- [`codex.rs`](/Volumes/Data/Github/threadBridge/rust/src/codex.rs#L179)
- [`telegram_runtime/mod.rs`](/Volumes/Data/Github/threadBridge/rust/src/telegram_runtime/mod.rs#L97)
- [`bot_runner.rs`](/Volumes/Data/Github/threadBridge/rust/src/bot_runner.rs#L24)
- [`min_chat_probe.rs`](/Volumes/Data/Github/threadBridge/rust/src/bin/min_chat_probe.rs#L66)

這些更接近：

- 非主路徑 fallback
- headless / probe / compatibility 保留面

而不是 today desktop-owner 主模型本身。

## Git 歷史驗證

這不是純粹從現況倒推的猜測。git 提交順序本身就支持「主模型已切換，但部分邊界尚未清理完畢」這個判斷。

### 1. 舊 CLI 同步與 handoff 模型先存在

較早的提交包括：

- `d12a85d` `feat(workspace): 實作本地 Codex CLI 與 Telegram 狀態同步機制`
- `1e0a7b0` `feat(threadbridge): add exclusive cli handoff attach`
- `ca9ea28` `feat(threadbridge): add managed hcodex mirror handoff`

這代表 CLI / hook / handoff 世界觀先形成，再慢慢遷移。

### 2. shared app-server runtime 之後才落地

`9f60e40` `feat(threadbridge): add shared app-server runtime foundation` 才是 shared app-server runtime foundation 成形的關鍵點。

接著 `36a7bfb` `refactor(threadbridge): remove codex sync bootstrap layer` 移除了 `tools/codex_sync.py` 這類早期 bootstrap layer。

這表示：

- canonical backend 已切到 app-server
- 但 CLI 時代的一些語義層並沒有在同一輪裡一起清乾淨

### 3. mirror/readiness 的文檔語義之後才補上

`122a504` `feat(threadbridge): replace cli model with local mirror readiness` 明確把文檔和部分狀態語言改寫成 mirror/readiness。

但今天仍可看到：

- `workspace_status` 仍用 `Local/Bot`
- `runtime_protocol` 曾使用 `handoff_readiness`

這說明當時更像是：

- 先把主模型往新語義改
- 再逐步收尾舊 vocabulary 與舊狀態面

### 4. 一部分遺留已被正式移除

以下提交代表並不是所有舊模型都還在：

- `d786026` `feat(threadbridge): remove viewer runtime leftovers`
- `4fcdad4` `refactor(threadbridge): drop legacy attach intent plumbing`
- `1c0838d` `refactor(repository): 移除已棄用的 SessionAttachmentState 列舉與欄位`

因此更準確的說法是：

- CLI 時代的顯性元件已清掉一批
- 但 vocabulary、status surface、launch transport shape 仍留有 transition debt

## 定位

這份文檔只處理：

- shared app-server / desktop owner 模型成立之後，剩餘的 post-CLI vocabulary、status surface、launch surface 清理

這份文檔不處理：

- mirror intake observer 化細節  
  這由 [app-server-ws-mirror-observer.md](/Volumes/Data/Github/threadBridge/docs/plan/app-server-ws-mirror-observer.md) 處理
- Telegram adapter 的完整抽象化路線  
  這由 [runtime-transport-abstraction.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-transport-abstraction.md) 與 [telegram-adapter-migration.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-adapter-migration.md) 處理
- working session observability 的最終 UI / API 形狀
- `hcodex` ingress 是否還需進一步縮減 responsibility

換句話說，這份文檔處理的是：

- app-server / desktop owner 模型已成立之後，剩下哪些詞彙、artifact 與 launch path 還在替舊模型背債

## 建議的收斂方向

### 1. 將狀態面從 `Local/Bot` 收斂到更中性的 runtime observation vocabulary

較合理的方向應是：

- 把 `.threadbridge/state/runtime-observer/*` 明確描述為 observation / activity surface，並把 `shared-runtime/*` 降為 legacy read compatibility
- 讓 `SessionActivitySource` 不再承載舊 local-vs-bot ownership 世界觀
- 重新評估 `local-tui-session.json` 是否應長期保留，或被更一般的 local TUI activity record 取代

### 2. 將 `runtime_readiness` 收斂到 owner-managed runtime readiness vocabulary

較合理的方向應是：

- 對外 view / recovery hint 用更符合 today 模型的詞
- 將「pending adoption」與「runtime degraded」拆成更清楚的不同類型狀態
- 讓 management surface 不再把 workspace readiness 建立在 handoff 概念上

### 3. 將 `hcodex` 啟動 contract 收斂到更直接的 workspace runtime contract

較合理的方向應是：

- 明確定義 `hcodex` 應依賴的 canonical launch contract
- 重新檢視 `/thread/<thread_key>` path sideband 是否仍必要
- 重新檢視 `hcodex-ws-bridge` 是否只是 transition shim，或是否應被更正式的 endpoint contract 取代

### 4. 將 compatibility 邊界固定在明確的 migration policy

例如：

- repository 對 legacy serialized fields 的兼容要保留多久
- 什麼時候可以停止接受 `attachment_state`
- 哪些 artifact 仍需 best-effort 讀舊格式，哪些應直接拒絕

## 與其他計劃的關係

- [session-level-mirror-and-readiness.md](/Volumes/Data/Github/threadBridge/docs/plan/session-level-mirror-and-readiness.md)
  - 描述現行 shared runtime + mirror + adoption 模型
- [app-server-ws-mirror-observer.md](/Volumes/Data/Github/threadBridge/docs/plan/app-server-ws-mirror-observer.md)
  - 只處理 mirror intake boundary，屬於本文件的一個子債務
- [runtime-protocol.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-protocol.md)
  - Phase 1 已同步改成 `runtime_readiness`；後續若要再拆 readiness 子狀態，這份文件需要同步更新
- [workspace-runtime-surface.md](/Volumes/Data/Github/threadBridge/docs/plan/workspace-runtime-surface.md)
  - 後續若要改 `.threadbridge/state/runtime-observer/*` 的定位或命名，這份文件需要同步更新
- [runtime-transport-abstraction.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-transport-abstraction.md)
  - post-CLI 清理是 transport/core 邊界更乾淨之前的前置收尾工作之一

## 開放問題

1. `workspace_status` 應該被視為 owner-canonical health 的附屬 observation surface，還是未來某種 session activity registry？
2. `runtime_readiness` 是否還需要再拆成更明確的 readiness 與 adoption 衍生欄位？
3. `local-tui-session.json` 是否真的還有獨立存在的必要，還是可被更一般的 session activity record 取代？

## 建議的下一步

1. 繼續把 `mirror observer` 與 broader post-CLI cleanup 分成兩條獨立重構線，不再混成同一件事。
2. 補齊 `workspace-runtime-surface` 與相關文檔，把 `.threadbridge/state/runtime-observer/*` 明確寫成 workspace-local observation surface。
3. 補完 `hcodex` canonical launch contract 的文檔與 compatibility policy，確認 legacy sideband 已不再是正式主路徑。
4. 在 repository 層定一個明確的 legacy field compatibility policy，避免 attachment/handoff 類歷史欄位永久滯留。

## 符合 app-server ws 主模型的推進順序

如果假設現有文檔語義都已對齊，後續工程不應再以「先清 CLI 詞彙」為主，而應以「把 app-server ws 做成唯一 canonical runtime contract」為主。

較合理的順序是：

1. 先完成 `app-server ws mirror observer`
   - 讓 shared `app-server` 成為 preview / process / final mirror 的主要事件來源
   - 不再把 `hcodex` ingress 當成主要 read-side intake
2. 再收斂 live session / adoption 判斷
   - 讓 Telegram 與其他 surface 不再依賴 `local-tui-session.json` / local claim 去決定 canonical continuity
   - adoption 若仍保留，也應退回成明確 control flow，而不是隱含 ownership 心智
3. 再收斂 `hcodex` launch contract
   - owner 應直接提供更 canonical 的 remote ws contract
   - legacy `/thread/<thread_key>` sideband 應只留在歷史/compatibility 語境，不再作為正式 contract
4. 最後再清 compatibility / fallback 尾巴
   - `selected_session_id` / `codex_thread_id`
   - `attachment_state`
   - `SelfManaged` bot path
   - `app_server_url=None -> stdio` probe / fallback

這個排序的核心判斷是：

- 先把真正的 authority 與 event source 做對
- 再清理為了舊模型而存在的狀態面與 launch shim
- 最後才移除非主路徑兼容層

如果反過來先清 fallback 或字彙殘留，只會讓文檔變乾淨，但不會讓 today runtime 更接近 app-server ws 主模型。
