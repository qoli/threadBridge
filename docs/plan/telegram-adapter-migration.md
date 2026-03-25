# Telegram Adapter 遷移草稿

## 目前進度

這份文檔目前仍是草稿，但 Telegram v0 的第一批 adapter surface 已經部分落地。

目前已有的前置收斂：

- final reply renderer 已有較清楚的 Telegram 專用邊界
- topic title watcher / busy gate 開始把平台表現和狀態來源分開
- desktop runtime 已成為正式 owner 啟動模型，Telegram 不再是共享 runtime 的正式 owner
- shared `runtime_control` 已把 workspace runtime ensure、session bind/new/repair、與 Telegram-to-live-TUI routing 從 adapter helper 抽出
- Telegram collaboration mode command surface 已落地：
  - `/plan_mode`
  - `/default_mode`
  - `current_collaboration_mode` 已開始持久化到 session binding
- Telegram interactive response 已有最小 v1：
  - direct bot path 可顯示並回覆 `request_user_input`
  - 本地 / TUI session 產生的 `request_user_input` 會透過 shared runtime interaction event mirror 回 Telegram
  - plan-only turn 結束後可透過同一條 adapter-owned interaction bridge 送出 `Implement this plan?` prompt
  - secret input 仍不支持
- Telegram session-first observability slash commands 已落地：
  - `/sessions`
  - `/session_log <session_id>`
- Telegram desktop launch control 已落地：
  - `/launch new`
  - `/launch current`
  - `/launch resume <session_id>`
- Telegram execution mode control 已落地：
  - `/execution_mode`
- Busy Gate follow-up control 已先落地第一個正式 action：
  - `/stop`
  - 目前是單獨 interrupt current turn；`STOP 並插入發言` / `序列發言` 仍未做

但整體架構仍未完成 Telegram adapter 化。

目前新增確認的優先級調整是：

- 近期不以「多 IM / 第二個聊天平台」為主要牽引目標
- `threadBridge` 目前沒有明確要接其他 IM 的產品計畫
- 因此更合理的近期方向是先把 Telegram adapter 做完整，再回頭看是否值得做更廣泛的 adapter 驗證

目前也新增記錄一組 Telegram-specific 的近期能力想法：

- Telegram observability 已先接上已落地的 session-first API，而不只停留在 thread transcript feed
- Telegram 已有 execution mode 設定入口；Codex 工作模型入口仍未做
- Telegram collaboration mode 設定入口已先行落地，且應持續和 execution mode 分開
- Telegram 已承接最小 v1 的 app-server / TUI 互動式回應面：
  - `request_user_input`
  - post-plan `Implement this plan?`
  - 後續仍可再擴成更一般的 interrupt / questionnaire surface
- Telegram desktop launch control surface 已先落地：
  - 用 slash command 觸發 desktop endpoint 的 `launch new` / `launch current` / `launch resume`
  - 這條能力不應被表達成 `codex / hcodex` 二選一
  - 也不應回頭改寫 `/new_session` 的 continuity 語義
- Telegram 之後可評估支持 `forwarded input`
  - 背景是現在採用 `Telegram thread = 工作 thread` 模型後，`main chat` 更像 control 面板，普通輸入空間變得不自然
  - 因此可考慮允許用戶在 `main chat` 透過轉發訊息，把內容投遞到目標 workspace thread 當成輸入
- 這些都先視為 Telegram adapter 能力面，不先提升成 transport-neutral core 語義

## 問題

如果方向是把 `threadBridge` 變成透明協議層，那目前最大的實際問題不是理論，而是：

- 現有產品入口就是 Telegram
- 許多功能已經圍繞 Telegram thread、message、preview、callback 在運作
- 若直接大幅抽象，很容易影響現有可用性

所以需要一份遷移草稿，回答：

- 如何在不打壞現有 Telegram UX 的前提下，把 Telegram 從產品核心邊界降成 adapter

## 方向

遷移策略長期上仍應是：

- 先抽語意
- 再搬責任
- 最後做第二個 adapter 驗證

目前新增確認的一點是：

- owner 責任收斂已先行落地，接下來才適合做更完整的 Telegram adapter 遷移

原因是：

- 若沒有先把 shared runtime control 從 Telegram helper 抽出，adapter migration 只會是換殼
- 目前 owner 與 shared control core 都已成立，後續焦點應轉到 Telegram 自身缺的 control / observability / delivery / 設定面

不建議一開始就：

- 先做完整 custom app
- 或先大規模重寫所有 Telegram runtime 模組

近期更務實的意思是：

- 先補齊 Telegram 自己還缺的 control / observability / delivery / 設定面
- 先讓 Telegram 成為一個完整、乾淨、邊界更明確的 adapter
- 而不是為了抽象而提早追求第二個 IM adapter

## 近期 Telegram v0 能力面

這一節只記錄近期較值得優先處理的 Telegram 能力，不直接等同於整體 adapter migration 完成。

### 1. Session-first observability

- 本地 `session-first observability` 已部分落地：
  - `GET /api/threads/:thread_key/sessions`
  - `GET /api/threads/:thread_key/sessions/:session_id/records`
  - workspace-card `Sessions` pane
- Telegram 相關 observability 已開始建立在 session-first API 之上
- 不應讓 Telegram surface 長期依賴 thread transcript feed 自行分組 `session_id`
- 換句話說，Telegram 若要補 observability / debug 能力，應直接消費既有的正式 session query surface，而不是再定義自己的 session timeline 模型

### 2. Model / Mode control surface

- Telegram 之後可補上 Codex 工作模型設定入口
- Telegram 已補上 execution mode 設定入口
- Telegram 已補上 collaboration mode 設定入口
- 這兩者應視為不同控制面：
  - `Codex 工作模型`
    - 回答「用哪個模型」
  - `execution mode`
    - 回答「以什麼 approval / sandbox contract 執行」
- `collaboration mode`
  - 回答「這一輪 / 這個 session 是普通模式還是 Plan mode」
- 尤其要避免把 `Plan / Normal` 誤表達成 execution mode；它和 `full_auto / yolo` 不是同一層語義
- 它們若要在 Telegram 露出，都應只是 runtime protocol control action 的 adapter surface

### 2.1. Interactive response / elicitation surface

- Telegram 已承接最小 v1 的互動式回應：
  - direct bot path 可顯示互動式問題 / 選項並回寫回答
  - 本地 / TUI session 產生的 `request_user_input` 可用同一種 Telegram surface mirror 回來
  - plan-only turn 可送出 `Implement this plan?` callback
- 這代表 Telegram 已不再只承接普通文字 turn、preview、final reply、plan/tool process transcript，也開始具備最小互動式 session adapter 能力
- 新增確認的一條 Telegram UX 決策是：
  - `request_user_input` 在完成或收到 upstream `serverRequest/resolved` 時，不再新增一條獨立 completion message
  - 目前做法是收斂既有 prompt surface；adapter 仍可對原 prompt 做完成狀態更新
- 目前仍未完成的部分包括：
  - secret input 仍不支持
  - 更一般的 interrupt / questionnaire surface 仍未 formalize
  - Telegram observability / debug UI 仍未接到這條互動流程

### 2.2. Desktop launch control surface

- Telegram 已補上一條獨立的 slash command，用來驅動 desktop endpoint 的本地 launch 行為
- 它目前只承接既有 desktop launch control 的 adapter surface，而不是重新定義 runtime continuity
- 較合理的動作集合是：
  - `launch new`
  - `launch current`
  - `launch resume <session_id>`
- 這條 control surface 的重點是：
  - 讓 Telegram 可以要求 desktop runtime 打開受管本地入口
  - 而不是讓使用者在 Telegram 中選 `codex` 或 `hcodex`
- 近期不應做的事情是：
  - 暴露 `codex / hcodex` 切換
  - 把它包裝成 `/new_session` 的別名
  - 讓 Telegram launch action 直接覆蓋 `current_codex_thread_id`

也就是說，這條能力應被理解成：

- Telegram adapter 的 desktop launch control

而不是：

- session lifecycle mutation
- local runtime implementation choice switch

### 2.5. Busy Gate follow-up control surface

- Telegram busy gate 不應永遠只剩下單純 reject 文案
- 第一個正式 control action `/stop` 已落地
- 之後可補兩種更明確的 follow-up 動作：
  - `STOP 並插入發言`
  - `序列發言`
- 這兩者都屬於 Telegram adapter 的 control surface
- 但它們依賴的 runtime 語義不同：
  - `STOP 並插入發言`
    - 仍屬單 active turn 模型
  - `序列發言`
    - 已開始接近顯式 queue 模型

因此較合理的收斂順序應是：

1. `STOP`
2. `STOP 並插入發言`
3. `序列發言`

### 3. `forwarded input`

- 這是一個 Telegram-only 的輸入補充模式
- 目標不是做 bot 輸出轉發，而是補目前 `main chat = control 面板` 下的輸入不便
- 使用者可在 `main chat` 轉發一則訊息，將它投遞到某個目標 workspace thread 作為輸入
- 這個能力近期先視為 Telegram adapter input surface，不先抽象成 transport-neutral runtime feature

## Telegram-only surfaces

這裡列的是近期已知、但仍未 formalize 的 Telegram-specific surface。

- `forwarded input`
- topic title
- preview draft
- interactive response / elicitation UI
- desktop launch slash command
- Telegram-specific render / callback / media send policy

其中 `forwarded input` 目前更值得優先考慮的是作為輸入能力，而不是 bot 輸出轉發能力。

也就是說，若之後真的支持 Telegram 轉發，應先把它當成 Telegram adapter 的輸入能力來設計，等它在 Telegram 內的語義收斂後，再決定是否值得提升成更一般的 runtime control / delivery 概念。

## 遷移目標

最終狀態應該是：

- Telegram 是 `threadBridge` 的一個 client adapter
- Telegram 不再擁有核心 runtime 語意
- core runtime 可以在沒有 Telegram 的情況下被其他宿主重用

## 建議的遷移階段

### Phase 1：邊界盤點

先盤點目前 Telegram-specific 邏輯有哪些。

至少應列出：

- 哪些模組只是在做平台輸入輸出
- 哪些模組其實在做 thread lifecycle / state machine
- 哪些流程把 Telegram 假設帶進了 Codex runtime

特別需要釐清：

- preview draft 更新
- slash command 與 control action
- desktop launch slash command 和 `/new_session` / adoption flow 的邊界
- image upload / pending batch
- topic title 更新
- markdown renderer
- `forwarded input` 的語義
- `main chat` 作為 control 面板時，`forwarded input` 要如何指向目標 workspace thread

這一階段的成果應該是一張責任表，而不是立即重構。

### Phase 2：先收斂 owner authority

在更完整的 adapter migration 之前，先把 shared runtime 的 owner 責任收斂。

至少應先回答：

- 誰能正式 ensure / repair app-server
- 誰能正式 ensure / rebuild `hcodex` ingress
- 哪些路徑只能讀 owner state，而不能自行補拉 runtime

這一步的目標不是 UI 遷移，而是把 runtime authority 從 Telegram 路徑抽出去。

目前這一階段的已知結果應視為：

- desktop runtime 是正式 owner
- Telegram 透過 shared `runtime_control` 讀 owner state 或送 control action
- `hcodex` 不再自補 shared runtime

### Phase 3：定義 Telegram Adapter 邊界

為 Telegram 明確建立 adapter 語意。

Telegram adapter 應只負責：

- 解析 Telegram update
- 轉成 shared runtime control / input request
- 訂閱 `RuntimeEvent` / `RuntimeInteractionEvent`
- 把事件渲染回 Telegram

core runtime 應負責：

- 驗證 busy gate
- 決定是否開始 turn
- 控制 Codex thread
- 發出 preview / tool / final / error 事件

### Phase 4：收斂平台專用 renderer

把 Telegram-specific 表示層都集中。

建議收斂成單獨邊界的內容：

- Telegram markdown / HTML formatter
- preview message edit policy
- topic title renderer
- Telegram media send helper

這樣 custom app 未來才不需要重用 Telegram 表示層。

### Phase 5：做第二個最小 adapter

不需要直接做完整產品級 custom app。

先做一個最小驗證面即可，例如：

- CLI adapter
- local HTTP demo adapter
- 簡單 WebSocket event viewer

目的不是做新 UI，而是驗證：

- core runtime 是否真的不依賴 Telegram
- protocol 是否足夠支撐另一個 client

但這一階段目前應降為遠期驗證，而不是近期主線。

### Phase 6：再決定 custom app 的正式形態

當第二個 adapter 跑通後，再決定：

- custom app 是本地桌面 app
- 本地 Web App
- 行動端 app
- 或其他嵌入式 client

## 與現有計劃的關係

這份遷移草稿和下面幾份直接相關：

- [session-lifecycle.md](/Volumes/Data/Github/threadBridge/docs/plan/session-lifecycle.md)
  - 決定 core runtime 的 thread / binding 模型
- [session-level-mirror-and-readiness.md](/Volumes/Data/Github/threadBridge/docs/plan/session-level-mirror-and-readiness.md)
  - owner 收斂是把 Telegram 去 owner 化、退回 adapter 的前置條件
- [codex-busy-input-gate.md](/Volumes/Data/Github/threadBridge/docs/plan/codex-busy-input-gate.md)
  - busy gate 應該是 core 語意，不是 Telegram-only 行為
- [telegram-markdown-adaptation.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-markdown-adaptation.md)
  - 應明確歸屬於 Telegram adapter，而不是 core
- [telegram-webapp-observability.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-webapp-observability.md)
  - 若觀測面最終不只服務 Telegram，它的資料模型也應依附 protocol，而不是 Telegram 型別

## 建議的程式碼邊界方向

可以預期未來會走向：

- `core/`
  - runtime state machine
  - codex orchestration
  - repository facade
  - workspace runtime
- `adapters/telegram/`
  - update parsing
  - message rendering
  - callback routing
  - media send

不一定要立刻調整實體目錄，但語意上應朝這個邊界收斂。

## 風險

- 如果先搬檔案再想語意，會只是形式重組
- 若 Telegram command 行為沒有先重新表達成 control action，很難抽出 adapter
- 若 preview 機制仍綁在 Telegram message edit，上層協議會失真
- 若互動式回應長期只停在目前最小 v1，而沒有 formalize 成更一般的 control surface，adapter 邊界仍會模糊

## 開放問題

- `/bind_workspace`、`/new`、`/reconnect_codex` 是否應被統一表示成 control action？
- custom app 是否需要 topic/title 這種概念，還是只需要 thread label？
- preview 在 custom app 裡應該是 delta stream、replace stream，還是 terminal-style replay？
- Telegram adapter 是否仍然是預設 entrypoint，還是未來要支援多 adapter 同時註冊？
- 近期 Telegram 是否應補上 Codex 工作模型與 execution mode 的設定入口？
- collaboration mode 是否應進一步進入更公開的 runtime protocol / management views，而不只留在 session binding 與 Telegram adapter？
- 互動式回應近期是否只先停在已落地的 `request_user_input` / plan prompt v1，還是要一併設計更一般的 interrupt / questionnaire surface？
- Busy Gate 下的新輸入是否應正式支持：
  - `STOP 並插入發言`
  - `序列發言`
  - 還是只先停在 `STOP`？
- `forwarded input` 若要支持，近期是否只先支持把 forwarded message 當成輸入，而不先支持 bot 輸出轉發到其他 Telegram thread / chat？
- 若支持 `forwarded input`，目標 thread 應如何決定：
  - 由 `main chat` 的顯式 command / button 先選定目標 thread
  - 還是由轉發時附帶某種 thread handle / reply target
  - 還是維持某個「目前選中的 workspace thread」狀態？
- 若支持 `forwarded input`，forward metadata 應保留到什麼程度，哪些部分只留在 adapter，哪些部分需要進入 runtime event / delivery model？

## 建議的下一步

1. 先把 owner authority 從 Telegram / `hcodex` 路徑中收斂出來。
2. 再列一份目前 Telegram-only 行為清單。
3. 把 command、preview、renderer、title update、`forwarded input` 分別標記為 adapter 或 core。
4. 定義最小 protocol 後，讓 Telegram 先改走新邊界，並優先補齊 Telegram 自己的 observability / control / delivery surface。
5. 等 Telegram adapter 足夠完整後，再決定 `forwarded input` 是否只留在 adapter，或需要進一步掛進更正式的 delivery model。
