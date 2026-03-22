# Message Queue And Status Delivery 草稿

## 目前進度

這份文檔目前仍是設計草稿，沒有完整落地。

目前代碼裡已存在的相關能力：

- preview draft
- final assistant reply
- plain system / control message
- restore page message edit
- media batch control message edit
- workspace outbox deliver
- preview draft 已接入 `sendMessageDraft + HTML parse mode`
- preview / final / plain system text 已開始收斂到同一套 line1 符號、line2+ 內容的 Telegram 呈現

目前尚未實作這份文檔想要的內容：

- 顯式 outbound delivery lane 模型
- persistent outbound queue
- 正式的 content / draft / status / edit delivery 規格
- Telegram 互動 control surface 規格
- media batch control message 的 action lifecycle 規格
- Telegram 文件 / 媒體大小上限規格

## 問題

`threadBridge` 現在已經有多條 Telegram 送信 surface：

- preview draft
- final assistant reply
- plain system / control message
- restore page message edit
- media batch control message edit
- workspace outbox deliver

但目前還沒有一份專門描述 outbound delivery 的主規格。結果是：

- preview、final、status、edit 的關係只能從程式碼猜
- 哪些訊息應該 FIFO、哪些可以 coalesce，沒有清楚語義
- 哪些 Telegram 按鈕 / 鍵盤屬於 status，哪些屬於 control，沒有清楚語義
- attachment / media 若超過 Telegram 限制時，缺少一致的 preflight 與 fallback 語義
- 之後要做 busy gate、history、Web App observability 時，很難知道「送信層」的責任邊界

## 定位

這份文件只規範 Telegram adapter 的 outbound delivery v1。

明確不處理：

- transport-neutral runtime protocol
- inbound user input queue
- history / unread pagination
- cross-machine delivery

但這份文件可以明確規範 Telegram 端的互動控制 surface，例如：

- 執行中階段用來替換使用者輸入面的 `ReplyKeyboardMarkup`
- turn 結束後用來恢復正常輸入面的 `ReplyKeyboardRemove`

也可以明確規範 Telegram 對文件 / 媒體大小上限的 adapter 行為，例如：

- 送出前先做本地檔案大小檢查
- 超限時要走哪種降級路徑

## 核心原則

- `threadBridge` 的 delivery lane 以 `thread_key` 為分區，而不是 per-chat 或 per-user。
- content 與 status 不是同一種 payload，應該明確分開。
- preview 仍然是 Telegram draft surface，但 renderer 已可與 final reply 共用。
- v1 不引入 persistent outbound queue。

## Queue Partition

每個 `thread_key` 一條 outbound delivery lane。

原因：

- `threadBridge` 的主要互動單位是 Telegram topic / thread
- 不同 topic 不應互相阻塞
- `coco` 的 per-user queue 適合它自己的 session 模型，但不適合 `threadBridge` 現在的 topic-bound runtime

## Delivery Item 類型

### `content`

真正送給使用者閱讀或下載的內容。

包括：

- final assistant text reply
- plain fallback reply
- overflow notice
- `reply.md` attachment
- workspace outbox text
- workspace outbox media / document

其中 media / document 類型應再區分：

- 可直接送到 Telegram 的 payload
- 超過 Telegram 上限、需要 fallback 的 payload

另外也應承認一種更輕量的 artifact content：

- URL-form artifact
  - 例如 diff viewer URL

### `draft`

preview draft surface。

包括：

- `sendMessageDraft`
- draft heartbeat 更新
- draft 狀態文字更新

### `status`

短期狀態提示，但不是最終內容。

包括：

- busy / unavailable 類 plain text 提示
- restore 成功提示
- media batch 狀態提示

### `control`

使用者可直接採取動作的 Telegram 互動面。

包括：

- `running` 階段的 `ReplyKeyboardMarkup`
- turn 結束時的 `ReplyKeyboardRemove`
- `STOP ai 回應` 這類 control action
- 之後若 secondary LLM guidance 落地，`AI 建議` inline buttons 也可能屬於這一類 surface

### `edit`

對已存在 Telegram message 的更新。

包括：

- restore page `editMessageText`
- media control message update

## 現有 Telegram Surfaces

### Preview Draft

- 使用 `sendMessageDraft`
- 已接入 final reply 共用的 HTML render 路線
- draft-specific 差異主要保留在 heartbeat、節流、截斷與 update lifecycle
- heartbeat 與狀態更新屬於 `draft`

### Final Assistant Reply

- 使用 final reply renderer
- 優先 Telegram HTML
- 失敗時退回 plain text
- 過長時改成 notice + `reply.md`
- 屬於 `content`
- 但 `reply.md` attachment 仍應受 Telegram 文件大小上限規則約束
- 若某個 artifact 的主要價值是讓使用者打開外部 viewer，則不一定要優先走 attachment

### Plain Control / System Message

- 經由 `send_scoped_message`
- 不做 rich-text rendering
- 屬於 `status` 或 `content`，取決於用途

### Busy / Running Control Surface

- busy gate 的提示訊息屬於 `status`
- `running` 階段替換用戶鍵盤的 `ReplyKeyboardMarkup` 屬於 `control`
- turn 結束後送出的 `ReplyKeyboardRemove` 屬於 `control`
- 同一個 thread 在 `running` 結束後，應能明確恢復正常輸入面
- 這條 control 的核心目標是降低自由輸入，而不是提供附著在訊息上的 inline button 體驗

### Restore / Media Control Message

- 這些是已存在訊息的更新
- 屬於 `edit`

### Pending Image Batch Control

- 圖片暫存後的 batch 提示屬於 `status`
- batch 訊息上的 `直接分析`、`取消這批圖片` 屬於 `control`
- 同一則 media batch control message 的內容更新屬於 `edit`

這樣切分的理由是：

- 「有一批圖片待分析」是狀態提示，不是最終內容
- 「直接分析 / 取消這批圖片」是使用者可採取的動作
- 同一則 Telegram 訊息後續從 pending 改成 canceled / analyzed，應被視為 edit lifecycle，而不是新 content

### AI Guidance Inline Buttons

若之後 secondary LLM 要把 `AI 建議` 先以 inline button 形式提供給用戶，這一類 payload 比較接近：

- `control`

而不是單純：

- `content`

原因是：

- 第一段目標是讓使用者挑選、展開或採納建議
- 不是直接把完整 guidance 文本混進主回覆
- 它比較像「可採取的 suggestion surface」

## Ordering 規則

### 同一個 `thread_key`

- `content` 必須 FIFO
- `content` 不允許被 `status` 插隊
- final reply 發送前，preview heartbeat 與 typing heartbeat 必須停止
- overflow notice 必須先於 `reply.md` attachment 發出
- `control` 不得暗中表達 queued user input；它只能操作目前已存在的 runtime 狀態

### `draft`

- `draft` 可 coalesce
- 同一個 draft key 永遠只保留最新一版 render
- draft 發送失敗不應阻塞 final reply

### `status` 與 `edit`

- `status` 可以做 latest-wins 合併
- `edit` 以 target message 為 key，較舊的待送 edit 可被覆蓋
- `edit` 不得重排已經發出去的 `content`
- media batch control message 在同一 batch lifecycle 內應盡量重用同一則 message，而不是每次狀態變化都新增一則新訊息

### `control`

- `control` 以 target thread 或 target status message 為 key
- 舊的 running keyboard 應可被新的 running keyboard 取代
- turn 結束後，必須送出對應的 `ReplyKeyboardRemove`
- `ReplyKeyboardMarkup` 的顯示與移除都應記入同一套 control lifecycle
- forum topic 場景下要明確定義它對同 chat 其他 topic 的影響與限制
- pending image batch 的 inline buttons 不得暗示 queue 已建立；它們只能作用於當前尚未分析的 batch
- `取消這批圖片` 應在 batch 已不存在、已分析、或 batch id 不匹配時回覆明確但簡短的結果，而不是靜默失敗

## Parse / Preview 規則

- preview 已不再是 plain text draft
- `sendMessageDraft` 已接入 `parse_mode=HTML`，並與 final reply 共用 HTML render policy
- final reply 才走 Telegram HTML renderer
- 所有文字 send / edit 路徑都關閉 link preview
- restore page、media control 仍不應隱式套用 final reply 的 render policy

## Telegram 文件上限語義

Telegram delivery 不應假設 document / media 只要本地存在就能送出。

建議明確定義一層 adapter-aware 的檔案上限規格，至少涵蓋：

- `reply.md` attachment
- workspace outbox `document`
- workspace outbox `photo`
- 未來其他由工具或 runtime 產生的 Telegram 檔案

### 建議的 v1 方向

- 在 Telegram send 之前先做本地檔案大小 preflight
- 大小門檻應集中在 Telegram adapter config，而不是散落在各個 callsite
- 檔案超限時不要盲送再等 API 失敗
- 應回到明確的 fallback / 拒絕路徑

### 可接受的 fallback 類型

- 發送簡短 status，說明該檔案超過 Telegram 上限
- 若存在較小替代品，改送替代品
- 若是 final reply attachment，改送更短的 notice 或其他可接受 payload
- 保留工作區相對路徑或 artifact 說明，讓使用者知道內容仍已生成
- 若 artifact 已有穩定 viewer URL，可改送 URL，而不是繼續嘗試 document upload

### 明確不應做的事

- 在多個送信 helper 裡各自硬編碼不同上限
- 先嘗試上傳再把 Telegram API failure 當作正常分支
- 把超限檔案 silently drop 掉

## Diff 類 payload 的特殊語義

`diff` 不一定適合被當成普通文字，也不一定適合默認走 document attachment。

對 Telegram 來說，更合理的方向可能是：

- 若 diff 已有穩定 viewer / artifact URL，優先送 URL
- Telegram 端把它視為一種顯式 artifact content
- 不把大型 diff 直接塞進訊息正文
- 也不要求 renderer 自行從一般 Markdown 內文猜出哪些 link 是 diff

這意味著 delivery 層之後可能要承認一種顯式 payload，例如：

- `diff_url`

它的語義會更接近：

- 「這裡有可打開的 diff」

而不是：

- 「這裡有一份要直接在 Telegram 裡讀完的文字或文件」

## Failure Semantics

### Preview

- draft 發送失敗只記 log
- 不重試成普通 message
- 不阻塞後續 final reply

### Final Reply

- HTML 送信失敗時，retry 一次 plain text
- plain text 若仍失敗，視為 final delivery failure
- attachment cleanup 是 best-effort
- 若 attachment 因 Telegram 文件上限無法送出，應走明確 fallback，而不是只留下底層 API 錯誤

### Attachment / Media 類

- 應先做大小檢查，再決定是否送出
- 若超限，應記錄結構化 log，並進入預定 fallback
- 同一類型 payload 的超限行為應保持一致，不要 `reply.md`、workspace outbox document、tool 產物各做各的

### Edit 類

- restore page 或 media control edit 失敗時記 log
- 失敗不影響已送出的 final content

### Control 類

- reply keyboard 顯示或移除失敗時記 log
- control action 處理失敗時，應回覆明確但簡短的 Telegram 錯誤提示
- `STOP ai 回應` 若已來不及生效，也應給使用者一個明確結果，而不是靜默失敗
- `取消這批圖片` 若已來不及生效，也應明確告知目前 batch 已分析、已不存在或已被替換

## Pending Image Batch Control v1 建議

既然現在已經有 media batch control message，就應把它當成正式 control lifecycle，而不只是單一 `直接分析` 按鈕。

建議的 v1：

- pending batch 訊息提供兩個 inline actions：
  - `直接分析`
  - `取消這批圖片`
- `直接分析`
  - 只在 batch 仍存在且尚未進入分析時可生效
- `取消這批圖片`
  - 只清除目前 pending batch，不回溯刪除已保存的 analysis artifact
  - 成功後把原 control message edit 成 canceled / cleared 狀態，避免使用者誤以為仍可分析
- 若 pending batch 已被新 batch 取代，舊按鈕應視為 stale action，回覆簡短錯誤並避免錯誤操作到新 batch

這條規格的目標是：

- 降低誤傳圖片造成的後續干擾
- 避免 pending batch 在 UI 上看起來像不可逆的半完成請求
- 讓 media control message 與 restore page 一樣，具有明確的 status + control + edit 分工

## Telegram Busy Control v1 建議

建議先把 busy gate 的互動控制面定義成：

- 主要控制面：`ReplyKeyboardMarkup`
  - 在 `running` 階段直接替換用戶鍵盤
  - 適合放 `STOP`、`顯示狀態`、`等完成`
- 退出控制面：`ReplyKeyboardRemove`
  - 在完成、失敗、stop 收斂後恢復正常輸入面

理由：

- 這條 UX 的目標是替換輸入面，而不是在訊息上附加一個按鈕
- `ReplyKeyboardMarkup` 比較接近這個目標
- 但它有殘留在 chat 的風險，所以必須把移除語義一起納入規格

## 不在這份文件裡解決的事

下面這些要在其他計劃裡定義，不在這裡偷帶：

- 同一 thread 的新 user input 是否排隊
- `running` 狀態下是 reject 還是 queue
- `/history`、unread range、history pagination
- runtime-generic `RuntimeEvent` stream

## 與其他計劃的關係

- `runtime-state-machine`
  - 定義 thread 本體狀態；這份只定義 delivery lane
- `codex-busy-input-gate`
  - 定義 inbound gate；這份不把 outbound lane 等同成 input queue
  - 但 busy gate 需要的 Telegram control surface 應由這份 delivery 文檔承接
- `telegram-markdown-adaptation`
  - final reply renderer 屬於這份 delivery 文檔裡的 `content` 規則
  - `reply.md` attachment 的大小上限 fallback 也應依附這份 delivery 規則，而不是由 renderer 單獨決定
- `runtime-protocol`
  - 若 diff URL 之後成為正式 artifact content，protocol 也需要有對應的表達方式

## 暫定結論

`threadBridge` 的 Telegram delivery v1 應理解成：

- per-thread outbound lane
- preview draft 與 final reply 分離
- `content`、`draft`、`status`、`control`、`edit` 分類明確
- file / media size limit 是 Telegram adapter delivery 規則的一部分
- queue 是 outbound delivery queue，不是 user input queue
