# Telegram Web App 觀測面草稿

## 目前進度

這份文檔目前仍是草稿，尚未開始實作 Web App。

目前已經有的前置能力：

- bot-local `data/`
- debug log
- workspace shared status
- Telegram topic title 狀態同步
- 本地 management API 的 query / SSE 骨架

目前仍缺：

- Web App UI
- 以 [working-session-observability.md](/Volumes/Data/Github/threadBridge/docs/plan/working-session-observability.md) 為基礎的 thread / turn / tool timeline 展示
- 面向 observability 的 thread summary API、working-session timeline、與 artifact view

目前新增確認的限制是：

- Telegram Web App 依賴 HTTPS
- 這和 `threadBridge` 目前偏本地 desktop runtime / local management UI 的部署模型有明顯摩擦
- 若用 Cloudflare Tunnel 或類似服務把本地觀測面暴露成 HTTPS，會引入新的公開面與安全風險

因此目前較合理的優先級調整是：

- local desktop runtime / 本地 web 管理面 / 受管 webview
  - 作為主要 observability 載體
- Telegram Web App
  - 降為遠期可選載體，而不是近期主路徑

## 問題

`threadBridge` 現在最大的實際痛點之一，不只是「如何執行」，而是「如何觀測」。

目前可以看到的資訊分散在幾個地方：

- Telegram thread 裡的最終回覆
- `data/debug/events.jsonl`
- 每個 thread 底下的 `conversations.jsonl`
- workspace 內 `.threadbridge/tool_results/`
- Codex app-server 過程中的串流事件

這些資訊都存在，但對使用者來說不在同一個可操作的觀測面裡。

結果就是：

- 出錯時很難知道卡在哪一層
- 很難區分「Telegram 沒收到」「Codex 沒回」「tool 執行失敗」「session broken」
- 排查通常還是要回到本機看 log 檔案

## 問題補充：HTTPS 與公開面

Telegram Web App 的最大產品摩擦不是 UI，而是部署模型。

它要求 HTTPS，這代表如果觀測面仍是 machine-local server，通常就要在下面幾種路線裡選一個：

- 自行處理本地 HTTPS 憑證與信任鏈
- 經由區網 / 反向代理額外轉發
- 經由 Cloudflare Tunnel 或其他公開 tunnel 服務暴露出去

對 `threadBridge` 目前的本地 owner 模型來說，這幾條路都不算便宜：

- 本地 HTTPS 會讓 setup、裝置信任、除錯與跨網段存取都更麻煩
- tunnel 會把原本 machine-local 的觀測面變成可被外部路由到的公開入口
- 即使有 auth，攻擊面與資料暴露風險也會明顯上升

而 observability 畫面裡很可能包含：

- prompt
- assistant reply
- tool request / result
- workspace path
- 錯誤與執行診斷

這些都不適合輕率暴露成公開 Web surface。

## 方向

若保留這份文檔，它比較合理的方向應改成：

- Telegram Web App 作為遠期可選 observability 載體

而近期主路徑應是：

- 由 desktop runtime 持有本地 observability UI
- 透過現有 local management API 與受管 web 管理面承接 working session 紀錄入口
- 若需要更原生的封裝，再考慮 desktop 內嵌 webview，而不是先做 Telegram Web App

核心想法：

- Telegram bot 繼續作為主要聊天入口
- desktop runtime / 本地 web 管理面作為主要觀測與診斷面
- Telegram Web App 若存在，應只是額外載體，不是 observability 的前提

## 為什麼 Telegram Web App 不再適合作為近期主路徑

它仍然有產品優點：

- 對使用者來說入口自然，就在 Telegram 裡
- 可以跟目前 thread / topic 的操作模型對齊
- 適合做狀態頁、日志頁、turn timeline、artifact 檢視
- 不需要另外記住一個管理後台網址

但目前缺點更實際：

- HTTPS 不是可忽略的部署細節，而是架構前提
- local-only runtime 會因此被迫面對憑證、反向代理、或公開 tunnel
- 對 observability 這種高敏感資料面來說，公開入口風險過高

因此這條線目前更適合作為：

- 產品形態探索
- 遠期載體選項

而不是：

- working session observability 的近期主落地方向

真正要補的是：

- runtime observability
- 執行狀態可見性
- thread 級問題排查能力

## 定位補充

這份文檔現在更適合被視為：

- Telegram Web App 作為遠期 observability 載體的產品草稿

而不是：

- 近期主路徑
- session timeline / session record model 的主規格

working session 的資料模型、timeline record、artifact 關聯，之後應引用：

- [working-session-observability.md](/Volumes/Data/Github/threadBridge/docs/plan/working-session-observability.md)

## 要觀測什麼

### Thread 層

每個 Telegram thread 至少應該能看到：

- thread key
- Telegram topic title
- workspace path
- 當前 `current_codex_thread_id`
- session 是否 broken
- 最後一次驗證時間
- 最後一次使用時間

### Turn 層

每次 turn 應該能看到：

- 使用者輸入
- 是否成功進入 Codex turn
- 預覽事件流
- tool 執行事件
- 最終 assistant 回覆
- 失敗原因

### Tool 層

每次 tool 相關操作應該能看到：

- 寫入了哪個 request file
- 執行了哪個 wrapper
- 對應 result file 路徑
- 是否有 outbox 項目準備送回 Telegram

### 系統層

還需要一個比較底層的觀測面：

- `events.jsonl` 的 thread 過濾視圖
- message / image / command / callback 事件
- bot 端錯誤
- session broken 原因
- reconnect / reset 歷史

## 可能的畫面結構

### 1. Thread 總覽頁

顯示：

- thread title
- workspace
- Codex thread id
- binding 狀態
- 最近幾次 turn 狀態
- 最近錯誤摘要

### 2. Turn 時間線頁

把一次 turn 拆成時間線：

- user message
- thread/resume
- turn/start
- item.started / item.completed
- tool call
- assistant message delta
- final response

這一頁會是最有價值的觀測面。

### 3. 日志檢視頁

用 filter 檢視：

- system
- user
- assistant
- error
- tool

可以依 thread key、時間、事件種類過濾。

### 4. Artifact 檢視頁

顯示：

- `conversations.jsonl`
- `session-binding.json`
- `state/pending-image-batch.json`
- `state/images/analysis/*.json`
- workspace `.threadbridge/tool_results/*`

重點不是完整檔案管理，而是讓排查時能快速打開與 thread 最有關的檔案。

### 5. Terminal 模擬器頁

Telegram Web App 其實也可以承載 terminal 風格的觀測面。

但這裡應該先明確區分三個層級，不要一開始就把它理解成完整遠端 shell。

#### Level 1: Log Replay Terminal

把已有資料重新排版成 terminal 風格顯示：

- command output
- assistant message delta
- tool 執行結果
- system log

本質上是 terminal 樣式的日志重播，不是真正的 terminal。

優點：

- 最容易實作
- 安全性高
- 很適合先補 observability

缺點：

- 不能互動
- 不是即時 PTY 畫面

#### Level 2: Read-only Live Terminal

把正在執行中的 PTY output 即時串流到 Web App，但不接受使用者輸入。

使用者可以看到：

- command 正在輸出什麼
- tool 呼叫過程
- 某些 runtime 執行中的即時日志

優點：

- 很接近真正的 terminal 體驗
- 仍然以觀測為主，風險比互動 terminal 低很多

缺點：

- 需要後端處理 live stream
- 需要處理連線中斷與補幀問題

#### Level 3: Interactive Terminal

Web App 將使用者輸入送回 backend，再寫進對應 PTY。

這已經不是單純 observability，而是遠端操作面。

優點：

- 能直接介入正在執行的 task
- 對 debug 某些卡住的 command 可能很有幫助

缺點：

- 權限與安全風險最高
- 必須有完整審計
- 需要處理 session 隔離、輸入控制、terminal resize、關閉行為

目前不建議作為第一階段目標。

## 資料來源

Telegram Web App 不應該直接去讀本地檔案系統。

比較合理的方式是：

- threadBridge runtime 提供只讀 API
- Web App 只調用這些 API
- API 負責把本地資料整理成可展示的 view model

建議資料來源：

- `metadata.json`
- `session-binding.json`
- `conversations.jsonl`
- `data/debug/events.jsonl`
- workspace `.threadbridge/tool_results/`

## 建議 API 面

目前已落地的是通用 management API / SSE；這份文檔提的仍是更偏 observability 的 thread summary API 與 session/timeline API。

可以先想成下面幾類只讀 API：

- `GET /api/threads`
  - 列出 thread 摘要
- `GET /api/threads/:threadKey`
  - 取 thread 詳細資訊
- `GET /api/threads/:threadKey/logs`
  - 取 `conversations.jsonl` 與 thread 過濾後的 event view
- `GET /api/threads/:threadKey/turns`
  - 取 turn timeline
- `GET /api/threads/:threadKey/artifacts`
  - 取主要 artifact 摘要

如果要進一步支援診斷，也可以補：

- `POST /api/threads/:threadKey/reconnect`
- `POST /api/threads/:threadKey/reset`

但初期建議先做只讀觀測，不要一開始就做控制面。

## 與現在架構的關係

這個想法不需要推翻目前架構，反而能直接利用現在已有資料：

- bot-local `data/`
- runtime appendix 與 `.threadbridge/`
- app-server 事件
- 已存在的 thread / binding 模型

真正缺的是：

- 把資料整理成可觀測模型
- 提供統一 UI
- 定義 terminal 相關資料是 replay、live stream，還是真的 PTY

## 風險

- 如果直接暴露太多原始日志，資訊量會過大
- 如果 Web App 同時承擔控制面，容易變複雜
- 如果 thread 過濾與 event 關聯做不好，畫面會不可信
- 需要注意敏感資訊顯示範圍，例如 token、絕對路徑、provider payload

## 分階段建議

### Phase 1

先做最小只讀觀測：

- thread 列表
- thread 詳情
- `conversations.jsonl`
- `session-binding.json`
- 近期錯誤摘要
- log replay terminal

### Phase 2

補 turn timeline：

- app-server 事件重組
- preview / tool / final response 時間線
- 失敗原因可視化
- read-only live terminal

### Phase 3

補 artifact 檢視與 thread 操作：

- tool request / result 摘要
- image analysis artifact
- reconnect / reset 按鈕
- 評估是否需要 interactive terminal

## 開放問題

- Web App 是否要嵌在 bot 進程內，還是獨立一個本地 HTTP server？
- thread timeline 要從 `events.jsonl` 重建，還是直接在 runtime 裡另外保存結構化 turn event？
- 要不要做即時串流，還是先只做 refresh 型頁面？
- Web App 應該是維護者工具，還是最終使用者也會直接使用？
- terminal 模擬器是否只用於觀測，還是未來真的要允許輸入？
- 如果做 live terminal，底層資料來源是既有日志流，還是真正 PTY stream？

## 建議的下一步

1. 先把 [working-session-observability.md](/Volumes/Data/Github/threadBridge/docs/plan/working-session-observability.md) 收斂成 session-level observability 的最小資料模型。
2. 決定是否先做一個本地只讀 API。
3. 先畫出 Thread 詳情頁和 Turn 時間線頁的最小欄位。
4. 把 terminal 能力拆成 replay / read-only live / interactive 三層，不要混在一起。
5. 明確區分「聊天面」與「觀測面」，不要一開始混在一起。
