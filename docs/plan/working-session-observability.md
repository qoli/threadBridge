# Working Session 可觀測紀錄草稿

## 目前進度

這份文檔已從純草稿進入「部分落地」，但仍不是完整主規格。

目前已實作：

- `transcript-mirror.jsonl` 已開始按 `session_id` 保存 session mirror
- `TranscriptMirrorEntry` 已有：
  - `timestamp`
  - `session_id`
  - `origin`
  - `role`
  - `delivery`
  - `phase`
  - `text`
- management API 已提供 `GET /api/threads/:thread_key/transcript`
- `runtime_protocol` 已新增：
  - `WorkingSessionSummaryView`
  - `WorkingSessionRecordView`
- management API 已提供：
  - `GET /api/threads/:thread_key/sessions`
  - `GET /api/threads/:thread_key/sessions/:session_id/records`
- management UI 已有：
  - transcript observability pane，可查看 `final` / `process` transcript
  - workspace card 內的 `Sessions` pane，可直接打開 session summary 與 records timeline
- `Sessions` / `Transcript` / `Launch Output` / `Advanced Workspace Details` 的展開狀態現在會在 refresh 後保留
- process transcript 已開始覆蓋 `Plan` / `Tool` 摘要
- session summary / records 已開始從 transcript mirror、recent session history、workspace session-status、以及 session binding broken 狀態即時計算

目前尚未完成：

- `GET /api/threads/:thread_key/sessions/:session_id` 單一 summary route 尚未存在
- `GET /api/threads/:thread_key/sessions/:session_id/artifacts` 尚未存在
- tool input / result / request file / result file 的結構化 artifact 關聯仍未收斂
- 獨立 observability page / route 尚未實作；目前仍是 workspace card 內的 pane
- retention、redaction、以及 mode-aware observability 邊界

目前新增確認的部署方向是：

- working session observability 應優先落在 machine-local 的 desktop runtime / 本地 web 管理面
- 不應把 Telegram Web App 視為近期前提

目前新增確認的近期落地方向是：

- 不先做完整 observability 頁面，也不先擴大 thread-level transcript feed
- 先把 `runtime-protocol` 與這份文檔接起來，補出 session-first 的正式 query surface
- 先讓 management API / web 管理面可以直接打開 `session_id`，而不是讓前端自行從 thread transcript 分組

## 問題

現在已經可以看到一些 transcript，但還缺少「某一個 working session 到底發生了什麼」的直接入口。

目前資訊分散在：

- thread 級 `transcript-mirror.jsonl`
- `conversations.jsonl`
- `session-binding.json`
- workspace `.threadbridge/tool_requests/`
- workspace `.threadbridge/tool_results/`
- `data/debug/events.jsonl`

這些資料雖然存在，但還沒有被整理成同一個 session 級觀測模型。

結果就是：

- 很難從管理面直接打開「目前正在工作的 session」
- 很難在同一個畫面依時間順序看到：
  - user prompt
  - assistant final reply
  - process transcript
  - tool use
  - tool artifact
  - 失敗原因
- 現有 `GET /api/threads/:thread_key/transcript` 比較像 transcript feed，不是完整 session 觀測入口

## 定位

這份文檔定義的是 `working session observability` 主草稿。

它處理的是：

- desktop runtime / web 管理面的 session 級觀測入口
- session summary / session detail / session records 的資料模型
- 單一 session 內 user / assistant / plan / tool / error 的時間線語義
- session 與 artifact 的關聯方式

它明確不處理：

- Telegram Web App 的產品外殼
- archive / restore / reconnect 這類 control action 本身
- interactive terminal
- transport-neutral thread / workspace state 主規格

也就是說：

- [runtime-protocol.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-protocol.md)
  - 定義 runtime query / control / event 的主線命名
- [working-session-observability.md](/Volumes/Data/Github/threadBridge/docs/plan/working-session-observability.md)
  - 定義 session 級 observability view
- [telegram-webapp-observability.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-webapp-observability.md)
  - 之後若存在 Telegram Web App，應把它當成遠期可選 UI 載體，而不是近期前提

## Working Session 的語義

這裡的 `working session` 指的是：

- 一個具體的 `session_id`
- 它對應某次 Codex app-server / 受管 TUI 的工作連續體
- 它可能來自：
  - Telegram turn
  - 本地 `hcodex`
  - TUI adoption 前後的同一條 continuity

對 observability 來說，session 是比 thread 更細的一級單位：

- thread
  - 長期 continuity / binding 容器
- session
  - 某次具體工作的觀測單位

這樣管理面才能回答：

- 目前正在跑的是哪個 session
- 最近完成的是哪個 session
- 某個 session 裡到底做了哪些 tool use

## 核心原則

- `working session` 應成為管理面的第一級可打開實體，而不只是 transcript 裡的一個欄位。
- session timeline 必須以 `session_id` 為主鍵聚合，而不是讓 UI 自己從 thread transcript 猜。
- `final transcript`、`process transcript`、tool artifact 應該能在同一個 session detail 中並列，而不是分散在不同頁面。
- 觀測面預設只讀；control action 可以連結出去，但不和 session record 模型混在一起。
- 每條 session record 都應能指出自己的 source of truth，而不是只剩一段渲染後文本。
- 敏感資料顯示必須有 redaction 規則，不能把 provider payload / token / 全量檔案內容直接當成預設 UI。
- 近期 observability 應建立在 machine-local surface 上，不應為了 Telegram Web App 而先把本地資料面公開成 HTTPS 入口。

## 載體策略

就目前部署模型來看，較合理的優先順序是：

1. desktop runtime 持有的本地 web 管理面
2. 受管 desktop webview 或等價的 machine-local shell
3. 遠期才評估 Telegram Web App 或其他需要 HTTPS 的遠端載體

原因不是 UI 偏好，而是安全與部署成本：

- Telegram Web App 需要 HTTPS
- 這通常意味著本地憑證、反向代理、或公開 tunnel
- 若透過 Cloudflare Tunnel 等服務暴露 observability 面，等於把高敏感 session 資料放進新的公開攻擊面

對 working session 來說，這類資料可能包括：

- user prompt
- assistant reply
- tool request / result
- workspace path
- 執行錯誤與診斷

因此比較合理的做法是先把 session observability 固定為 machine-local capability，再決定是否值得做遠端載體。

## 入口形狀

在 desktop runtime / web 管理面中，至少應有一個明確入口：

- workspace detail 裡的 `Current Session`
- recent sessions list
- `Open Session Log` / `View Session Timeline`

這個入口要能直接打開某一個 `session_id`，而不是先讓使用者理解：

- `transcript`
- `mirror`
- `events.jsonl`
- `tool_results`

這些底層 artifact 名稱。

## 建議的資料模型

### 1. `WorkingSessionSummaryView`

至少包含：

- `session_id`
- `thread_key`
- `workspace_cwd`
- `started_at`
- `updated_at`
- `run_status`
- `origins_seen`
  - 例如 `telegram` / `tui` / `local`
- `record_count`
- `tool_use_count`
- `has_final_reply`
- `last_error`

這個 view 用來承接：

- current session 卡片
- recent sessions list
- session picker

### 2. `WorkingSessionRecordView`

這是 session detail 的核心時間線 item。

至少包含：

- `timestamp`
- `session_id`
- `kind`
  - `user_prompt`
  - `assistant_final`
  - `process_plan`
  - `process_tool`
  - `error`
- `origin`
- `role`
- `summary`
- `text`
- `delivery`
  - 若來自 transcript mirror，保留 `final` / `process`
- `phase`
  - 若來自 process transcript，保留 `plan` / `tool`
- `source_ref`

其中：

- `kind`
  - 是 session detail 用的 UI-facing 類型
- `delivery` / `phase`
  - 是保留下來的底層 debug metadata

目前已落地的 v1 仍刻意保持最小：

- `kind` 先由現有 transcript mirror 與 binding/session-status 衍生
- `source_ref` 目前只標示簡短來源，例如 `transcript_mirror` / `session_binding`
- `tool_name` / `tool_detail` / `artifact_refs` 尚未成為正式 wire 欄位

### 3. `SessionArtifactRef`

至少包含：

- `label`
- `artifact_kind`
  - `tool_request`
  - `tool_result`
  - `reply_attachment`
  - `image_analysis`
  - `generated_image`
- `path`
- `exists`
- `size_bytes`

artifact ref 的目標不是做完整檔案瀏覽器，而是讓 session detail 能回答：

- 這次 tool use 寫了哪個 request file
- result file 在哪裡
- 最後送回 Telegram 的 artifact 是什麼

## 記錄範圍

單一 working session 至少應能展示下面幾類記錄。

### User Prompt

- 使用者輸入的 prompt
- 來源可來自 Telegram 或本地 TUI
- 應保留 `origin`

### Assistant Final Reply

- 最終 assistant 回覆
- 若有 Telegram attachment fallback，也應能看到對應 artifact ref

### Process Transcript

- `Plan`
- `Tool`
- 其他之後正式納入 process transcript 的事件

這一層目前已部分存在，但仍只有摘要文本，沒有完整 session detail 能力。

### Tool Use

觀測面不應只停在「Tool: cargo test」這種摘要。

至少還應補足：

- tool 名稱或 wrapper 名稱
- 開始時間
- 完成時間或失敗
- 相關 artifact
  - request file
  - result file
  - outbox payload

如果短期內無法穩定保存完整 input / output，初版也應至少把：

- tool name
- tool detail
- artifact refs
- exit / result summary

整理進 session record。

### Error / Status

對 session 排查最有價值的錯誤也應能落在同一條時間線上，例如：

- resume 失敗
- workspace `cwd` 驗證失敗
- tool wrapper 失敗
- Telegram delivery fallback

但這些錯誤不應只靠掃整份 `events.jsonl` 才能知道。

## 資料來源與所有權

初版建議的 source of truth：

- `transcript-mirror.jsonl`
  - session timeline 的主要來源
- `conversations.jsonl`
  - 補充 user / assistant 對話歷史
- `session-binding.json`
  - 補充 thread 與目前 continuity 的關係
- workspace `.threadbridge/tool_requests/`
  - 補 request artifact
- workspace `.threadbridge/tool_results/`
  - 補 result artifact
- `data/debug/events.jsonl`
  - 只補 observability 專用的錯誤與診斷，不作 primary timeline source

web UI 不應直接讀這些檔案。

應由 desktop runtime / management API 提供整理後的只讀 view。

## 建議的 API

目前已存在：

- `GET /api/threads/:thread_key/transcript`

但這條 API 應視為：

- transcript feed
- 或 session observability 的底層材料

而不是完整 session 入口本身。

目前已存在：

- `GET /api/threads/:thread_key/sessions`
  - 取 recent sessions summary
- `GET /api/threads/:thread_key/sessions/:session_id/records`
  - 取 session timeline

目前尚未做：

- `GET /api/threads/:thread_key/sessions/:session_id`
  - 取單一 session summary
- `GET /api/threads/:thread_key/sessions/:session_id/artifacts`
  - 取與 session 關聯的 artifact refs

若未來管理面完全收斂成 workspace-first，也可以再補等價的 workspace route。

目前已落地的 v1 也明確保留：

- `GET /api/threads/:thread_key/transcript`
  - 繼續作為 raw/debug transcript feed
  - 不再被視為 session observability 的唯一入口

## 近期建議的最小落地切片

這裡先明確記錄目前已採用的 v1 決策：

- 先做 session-first API，而不是繼續停留在 thread transcript feed
- 先做 workspace card 內的最小 `Sessions` pane，而不是完整 observability 頁面
- 不先引入新的 event store 或 artifact browser

原因是：

- 目前代碼已經有 `session_id`、`transcript-mirror.jsonl`、process/final transcript 區分、以及 management transcript read API
- 但 UI 若想回答「現在是哪個 session 在跑」、「這次 session 做了哪些 tool」、「這次 session 的錯誤與 artifact 是什麼」，仍需要自己從 thread feed 推導
- 這代表真正缺的不是更多 transcript，而是把既有資料整理成 session-first view

這個最小切片目前已落地：

1. 在 `runtime_protocol` 中新增 `WorkingSessionSummaryView`
2. 在 `runtime_protocol` 中新增 `WorkingSessionRecordView`
3. 補 `GET /api/threads/:thread_key/sessions`
4. 補 `GET /api/threads/:thread_key/sessions/:session_id/records`
5. 初版先直接重用既有 `TranscriptMirrorEntry` 與 session-status / binding / recent history，不要求先引入新的 event store

這個切片的目標不是一步到位做完整 session detail，而是先把：

- `session_id`
- `run_status`
- `origin`
- `delivery`
- `phase`
- `last_error`
- refresh 後仍可維持展開的 session UI 入口

整理成管理面可以直接消費的 read-only view。

換句話說，`GET /api/threads/:thread_key/transcript` 在近期仍保留，但應明確視為底層材料，而不是 session observability 的最終入口。

## 與其他計劃的關係

- [session-level-mirror-and-readiness.md](/Volumes/Data/Github/threadBridge/docs/plan/session-level-mirror-and-readiness.md)
  - 定義 mirror 與 shared runtime 的現行模型
  - 這份文檔消費它輸出的 transcript / mirror，不重複定義 continuity
- [runtime-protocol.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-protocol.md)
  - 應對外暴露 session summary / record 的 query 命名
- [macos-menubar-thread-manager.md](/Volumes/Data/Github/threadBridge/docs/plan/macos-menubar-thread-manager.md)
  - web 管理面可把 session observability 作為 workspace detail 的一個 pane / route
- [telegram-webapp-observability.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-webapp-observability.md)
  - 若未來仍做 Telegram Web App，應直接引用這份 session view model，而不是再自行定義 timeline 結構

## 開放問題

- session timeline 要完全從現有 transcript mirror 衍生，還是要新增更結構化的 event store？
- tool use 是否要保存更完整的 input / output，還是先只保存摘要與 artifact refs？
- `events.jsonl` 裡哪些錯誤值得提升為正式 session records？
- execution mode 是否需要影響 observability retention 或 redaction 深度？
- current session 與 recent sessions 的排序，應以 `updated_at`、最後 event，還是最後 final reply 為準？
- 是否要讓同一個 session 同時呈現 Telegram delivery 結果，還是只觀測 Codex/runtime/tool 面？
- 若未來仍要支持 Telegram Web App，該如何避免它成為需要公開 tunnel 的本地資料入口？

## 建議的下一步

1. 先把 `session_id` 從 transcript/mirror 提升為 management API 可直接查詢的一級實體。
2. 在 `runtime_protocol` 中新增 `WorkingSessionSummaryView` 與 `WorkingSessionRecordView`，避免 UI 自行從 thread feed 分組。
3. 下一步再決定是否需要 `GET /api/threads/:thread_key/sessions/:session_id` 單獨 summary route。
4. 繼續保留 timeline 裡的 `origin` / `delivery` / `phase`，不要過早把 process/final 差異抹平。
5. 之後再把 tool request/result/outbox 與 transcript mirror 建立最小 artifact 關聯，評估是否需要更完整的 session detail、live stream 或 terminal replay。
