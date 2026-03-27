# App-Server WS Mirror Observer 草稿

補充維護說明：

- [docs/mirror-function-notes.md](../../mirror-function-notes.md)
- [app-server-observer-upstream-capability-audit.md](app-server-observer-upstream-capability-audit.md)

## 目前進度

這份文檔目前已進入「部分落地」。

目前已確認的前提：

- `threadBridge` 已新增獨立的 app-server ws observer，mirror intake 不再只掛在 `rust/src/hcodex_ingress.rs`
- observer 已將 `request_user_input` / resolved-request / plan follow-up 轉成 adapter-neutral runtime interaction event
- observer turn finalization 已不再直接依賴 Telegram final reply helper；visible final text 組裝已改由 shared completion helper 承接
- `hcodex` ingress 目前承擔：
  - `hcodex` 連線入口
  - launch ticket 驗證
  - TUI session / turn metadata 追蹤
  - live request-response injection
- upstream `codex app-server` 已具備：
  - websocket transport
  - thread-scoped streaming notifications
  - 同一 thread 的多 subscriber 模型
  - running thread `resume` 後附加 subscription 與 pending server request replay

目前尚未完成：

- observer 目前仍建立在 `thread/resume` attach 語義上，而不是正式的 upstream subscribe API
- 少量文檔、legacy compatibility alias、與歷史分析仍沿用 `tui_proxy` 詞彙，尚未完全對齊新的 ingress/observer 分層
- broader session observability 與 transport-neutral observer contract 仍未完成

## 問題

目前 `threadBridge` 會讓人產生一個抽象混淆：

- `shared app-server` 是 workspace 的 canonical runtime backend
- 但 local/TUI mirror 的歷史心智仍常被描述成長在 `TUI proxy`

這使得歷史上的 `TUI proxy` 看起來像是在承擔 app-server ingress / event authority，實際上它只是 `hcodex` 這條本地 TUI 路徑的中介層；現在主要 read-side 已改由 observer 承接。

問題不只是命名，而是責任面被黏在一起：

- `mirror` 是 Codex runtime event 的消費與投影
- `TUI proxy` 則更像 `hcodex` 專有的接入與互動橋接層

只要 mirror 還主要依附在 proxy：

- mirror 會被理解成 `hcodex` 專屬副產品，而不是 shared runtime 的可觀測輸出
- `TUI proxy` 會持續背負非 TUI 專屬的 read-side 責任
- 未來若想讓其他 surface 從相同 session 事件流做觀測，容易再次走向 inline interception

## 背景與成因判斷

從目前 git 歷史看，這個結構高概率是早期 CLI 呼叫形式殘留的結果。

比較合理的歷史推斷是：

- 在舊模型裡，Codex 仍偏向由本地 CLI / TUI 路徑直接驅動
- 當時若要觀測 prompt、preview、final reply、process transcript，最容易的切入點就是包住本地互動入口
- 因此 mirror 很自然會長在 wrapper / proxy 類型的元件上

之後 `threadBridge` 已逐步重構到 shared `codex app-server` + websocket 的 runtime 形狀，但這條舊結構沒有完全清理：

- shared app-server 已成為 canonical backend
- `desktop runtime owner` 已成為 runtime authority
- `hcodex` 已成為 owner-managed local entrypoint
- 但 local/TUI mirror 仍主要停留在 proxy 這個 inline interception 點

所以這份文檔處理的不是「mirror 要不要存在」，而是：

- 在 app-server ws 方案已成立後，mirror 是否還應主要依附 `TUI proxy`

這份文檔的判斷是：

- 不應該長期如此
- 這屬於一次尚未清理完畢的架構收尾工作

## Git 歷史驗證

這不是純粹的事後猜測。git 提交順序本身就支持這個判斷。

### 1. CLI / hook 同步模型先存在

`d12a85d` (`feat(workspace): 實作本地 Codex CLI 與 Telegram 狀態同步機制`) 先引入的是：

- `codex_sync.py`
- workspace shell wrapper / hooks
- `workspace_status`
- `telegram_runtime/status_sync.rs`

這個階段的 mirror / 同步明顯是長在 CLI 包裝層與 workspace 事件流上，而不是 app-server ws observer。

### 2. shared app-server runtime 之後才落地

`6dc40c5` (`feat(workspace): 將執行環境從 Codex CLI 遷移至 app-server JSON-RPC`) 把執行主模型改成 app-server。

但接著 `9f60e40` (`feat(threadbridge): add shared app-server runtime foundation`) 仍保留了舊 `codex-sync` surface 作為 compatibility path，README 也明確記錄 shared-runtime migration 期間仍留著 legacy hooks/state files。

這代表：

- shared app-server runtime 已成為新 backend
- 但舊 CLI 同步思路並沒有在同一輪重構裡被完全移除

### 3. `TUI proxy` 是在 shared runtime 之後出現，且一開始就承接 mirror

`9f71c2e` (`feat(threadbridge): add tui proxy adoption flow`) 才新增後來已收斂為 `rust/src/hcodex_ingress.rs` 的 ingress 模組。

而這個初版 proxy 並不只是 relay：

- 它攔 `thread/start` / `thread/resume`
- 它攔 `turn/start` prompt
- 它追蹤 `agentMessage/delta`
- 它在 `turn/completed` 時寫回 mirror / status

這更像是把既有「從接入點攔截並同步」的思路，從 CLI/hook 路徑延伸到了新的 TUI ingress。

### 4. 文檔語言已切到 mirror/readiness，但 mirror intake 邊界沒有同步重構

`122a504` (`feat(threadbridge): replace cli model with local mirror readiness`) 已經把文檔主語義從舊 CLI/handoff 模型切到 mirror + readiness。

但這次變更主要是：

- 重寫文檔與語言模型
- 收斂 owner / readiness / local mirror 的表述

而不是把 mirror intake 從 proxy 正式抽成 app-server observer。

### 暫定結論

比較保守但準確的說法應是：

- `TUI proxy` 承接 mirror intake，高概率源自早期 CLI / hook 同步模型的延續
- shared app-server websocket runtime 落地後，這個 read-side responsibility 沒有在同一輪重構中被進一步抽離
- 因此它更像是過渡期殘留，而不是未來應長期保留的最終分層

## 目前 `TUI proxy` 的歷史成分拆解

從 git 歷史回看，目前 `TUI proxy` 裡的責任不應被視為同一種來源。

比較合理的拆法是三類：

### 1. 高概率屬於 CLI 架構遺留

這一類是早期 CLI / hook / handoff 時代就已經存在的產品語義，只是在 shared app-server ws 遷移後，沒有被完整抽回 runtime observer 層。

- mirror intake
  - local/TUI prompt mirror
  - assistant preview / final reply mirror
  - process transcript / plan transcript mirror
- handoff / adoption continuity
  - `tui_active_codex_thread_id`
  - adoption pending / local ownership 對齊

這些能力的共同特徵是：

- 都是從「接入點攔截並同步」的思路長出來的
- 在 CLI/hook 時代合理
- 到了 shared app-server runtime 時代，應更自然地收斂到 observer / projection 層

### 2. 高概率屬於 app-server ws 遷移後新增，但未完全收斂的 workaround

這一類不是 CLI 時代直接留下來的，而是 shared app-server ws + `hcodex` 接入成形後，為了讓現有路徑先工作而新增的過渡結構。

- `/thread/<thread_key>` path sideband identity
  - proxy 要求 path 攜帶 `thread_key`
  - `hcodex` resolver 再把它拼回 launch URL
- `hcodex-ws-bridge`
  - 為了把帶 path 的 upstream ws URL 再包成一個乾淨本地 ws URL
  - 讓 `codex --remote` 可以穩定接入
- 舊 proxy / observer 內的 Telegram interactive glue
  - `request_user_input` prompt 發送
  - resolved request UI 更新
  - `Implement this plan` follow-up prompt

這些能力比較像：

- 不是舊 CLI 模型直接殘留
- 但也不是 shared runtime 架構的理想最終形狀
- 它們反映的是 app-server ws 遷移完成後，interface shape 還沒完全收斂

目前這一塊已經有了新的收斂方向：

- observer 改成發出 shared `RuntimeInteractionEvent`
- Telegram prompt / callback / cleanup UI 由 adapter-owned `interaction_bridge` 消費
- `hcodex` ingress 只保留 request-response injection 與本地 TUI 路徑相關責任

### 3. 目前仍可視為合理保留在 ingress 的核心

如果這個 `hcodex` ingress 元件要繼續存在，較合理保留的責任應該是：

- `hcodex` 專有 ingress
- workspace-scoped listener / reuse / rebuild
- client <-> daemon websocket relay
- live request injection 到同一條本地 TUI 連線
- `thread_key` 與本地 TUI session 對齊

這一類的共同特徵是：

- 與 `hcodex` 這條本地 TUI 路徑直接相關
- 不要求 proxy 充當 mirror observer 或 Telegram adapter
- 即使未來 mirror observer 拆出去，這層仍然可以獨立存在

## 對目前重構判斷的影響

因此，若只用一句話描述目前狀態：

- 歷史上的 `TUI proxy` 不只是單純的 TUI bridge
- 它同時混合了：
  - `hcodex` ingress
  - CLI 時代延續下來的 mirror / adoption 語義
  - app-server ws 遷移後新增的 workaround / Telegram glue

所以真正需要被清理的，不是「proxy 這個元件是否存在」，而是：

- 哪些責任應回到 shared runtime observer
- 哪些責任應回到 Telegram adapter
- 哪些責任才是 proxy 自己真正應保留的核心

## 定位

這份文檔只處理一件事：

- 將 local/TUI session mirror 的主要 read-side intake，從歷史上的 `TUI proxy` 路徑遷移到獨立的 app-server ws observer

這份文檔不處理：

- mirror 內容本身的 user-facing 呈現規格
- Telegram renderer / preview 文案
- `request_user_input` 的最終 adapter UX
- 是否完全移除 `hcodex` ingress 元件
- upstream `codex app-server` websocket transport 的產品承諾調整

換句話說，這是一份 `mirror intake boundary` 草稿，不是新的整體 mirror 主規格。

## 方向

未來較乾淨的模型應該是：

- `shared app-server`
  - canonical runtime backend
  - canonical thread / turn / item / server-request event source
- `app-server ws mirror observer`
  - 純 read-side consumer
  - 連到同一個 workspace daemon
  - attach 指定 `thread.id`
  - 消費 turn / item / request lifecycle event
  - 寫入 threadBridge 的 transcript / workspace status / observability projection
- `TUI proxy`
  - `hcodex` 專有 ingress
  - `hcodex` session / thread_key 對齊
  - live request injection / bridge
  - 必要的本地 session adoption 輔助

這樣的分工會更接近現在實際 runtime 形狀：

- app-server 負責 authority
- observer 負責 mirror
- proxy 負責 `hcodex` 專屬接入

## 為何 app-server ws 協議足以承接 mirror

目前 upstream `codex app-server` 已提供大部分 mirror 所需材料：

- `thread/start` / `thread/resume` 後可接收 thread-scoped notifications
- `turn/started` / `turn/completed`
- `item/started` / `item/completed`
- `item/agentMessage/delta`
- `item/plan/delta`
- `turn/plan/updated`
- `userMessage` / `agentMessage` / `plan` / tool item 等 typed item payload
- thread-scoped pending server request replay

另外，server 內部已存在多 subscriber 模型：

- connection 可以附加到同一個 loaded thread
- 只有最後一個 subscriber 離開時 thread 才會 unload

因此如果只談 mirror intake：

- app-server ws 協議本身大體上已足夠

它現在缺的不是「能不能看見事件」，而是 threadBridge 尚未把 observer contract、subscription 語義、與 broader observability 收尾做完整。

## 為何歷史上會掛在 TUI proxy

早期把 mirror 掛在 `TUI proxy` 的價值主要是實作方便，而不是抽象正確：

- proxy 已經站在 `hcodex <-> daemon` 的中間
- 它天然拿得到 client request 與 daemon response
- 它已經知道 websocket path 對應哪個 `thread_key`
- 它已有 live channel 可把 Telegram 回應注回同一條 TUI session

所以把 mirror 先做在 proxy 裡是務實的，但這不代表它是最終邊界。

現在 shared app-server observer 已落地，剩下的問題已不是「要不要 observer」，而是如何把殘留 terminology 與 compatibility boundary 收斂乾淨。

## 建議的收斂後責任面

### 1. `hcodex` ingress 應保留

- `hcodex` 本地連線入口
- `thread_key` 與 live local TUI session 對齊
- 需要回注到 live TUI session 的 request/response bridge
- adoption / local ownership 輔助狀態

### 2. `hcodex` ingress 應逐步卸下

- preview mirror 的主 intake
- process transcript mirror 的主 intake
- final assistant visible reply mirror 的主 intake
- 對 app-server item 流的主要 read-side 解譯責任
- Telegram-specific prompt / follow-up UI

### 3. observer 已承接，後續應補完

- attach / detach thread subscription contract
- 以 session 為單位的 event 消費
- transcript mirror 寫入
- workspace status / observability projection
- 對 mirror 專用的 event compaction / dedupe 邏輯

## 風險與限制

### 1. websocket transport 目前仍是 experimental / unsupported

這代表：

- threadBridge 若把 observer 更深地建立在 ws transport 上，等於更明確依賴這條能力
- 這在架構上是合理的，但在產品風險上仍需要清楚承認

### 2. `thread/resume` 目前在語義上更像 attach+resume，而不是純 observer subscribe

也就是說：

- 今天雖然可以把它用作 observer attach
- 但 upstream 若將來提供更明確的 `thread/subscribe` API，threadBridge 應優先切過去

### 3. server request arbitration 仍需要明確策略

如果同一 thread 同時有：

- `hcodex` client
- mirror observer
- 其他 transport client

那麼 observer 應是純觀測者，不應回應 server request。

換句話說，mirror observer 不能只是「另一個普通 client」，而必須是明確的 read-mostly / no-reply 角色。

### 4. 某些 threadBridge 額外 metadata 目前仍從 client request 側攔截取得

例如：

- `turn/start` 當下的 prompt 快照
- 某些 threadBridge 自己想保留的 mode / routing metadata

這些欄位在 observer 化後需要重新決定：

- 能否從 app-server typed event 等價重建
- 是否應退回為非必要資訊
- 是否值得要求 upstream 暴露更合適欄位

## 與其他計劃的關係

- [session-level-mirror-and-readiness.md](../runtime-control/session-level-mirror-and-readiness.md)
  - 描述現行 mirror / adoption / readiness 模型
  - 這份文檔只處理它內部的 mirror intake 邊界重構
- [runtime-transport-abstraction.md](../runtime-control/runtime-transport-abstraction.md)
  - 這份文檔提供一個具體例子，說明哪些 read-side 責任應從 `hcodex` 專屬元件抽回 runtime 側
- [working-session-observability.md](../management-desktop-surface/working-session-observability.md)
  - observer 若落地，將直接影響 session timeline / records 的資料來源穩定性
- [runtime-protocol.md](../runtime-control/runtime-protocol.md)
  - observer 化之後，mirror event 與 session observability 更容易收斂成 transport-neutral runtime event

## 開放問題

- observer attach 應直接使用現有 `thread/resume`，還是先在 threadBridge 內明確包出 `observe_thread()` 語義等待未來 upstream 對應？
- observer 若與 `hcodex` 同時在線，哪些 event 需要 dedupe，哪些應完整保留？
- 目前 proxy 直接攔截 `turn/start` prompt 的行為，是否真的還有必要保留？
- 若 ws transport 未來仍維持 experimental，threadBridge 是否需要為 observer 明確標示 capability / degraded mode？

## 建議的下一步

1. 在文檔上正式承認：mirror 的主要 read-side intake 已移到 observer，而 `TUI proxy` 只保留歷史名稱與分析語境。
2. 補齊 observer 的 contract 文檔：
   - `initialize`
   - attach 指定 `thread.id`
   - 消費 `turn/*`、`item/*`、`serverRequest/resolved`
   - 寫入現有 transcript / workspace status
3. 持續驗證 observer 與 ingress 並存時的 mirror record、interaction event、與 dedupe 邊界。
4. 將 ingress 中與 mirror intake 直接相關的歷史說明與 terminology 逐步下沉到 observer 文檔。
5. 等 contract 收斂後，再重新命名或重述 `hcodex` ingress 的責任邊界，避免它繼續被理解成 app-server gateway。

這份 observer 重構在整體 post-CLI 清理中的優先級應固定為：

- 第一優先

原因不是它最容易，而是：

- 只要文檔與 compatibility 邊界仍把 mirror 理解成 `TUI proxy` / ingress interception 的副產品，整體 runtime 就仍殘留 CLI 時代的接入點心智
- 只有先把 shared `app-server ws` 做成主要 mirror event source，後續的 adoption 收斂、launch contract 收斂、以及 adapter/core 邊界整理才有穩定基礎

換句話說：

- 這份文檔不只是眾多 cleanup 子項之一
- 它實際上是讓 `threadBridge` 真正切齊 app-server ws 主模型的第一步
