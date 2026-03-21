# LLM 建議與目標層草稿

## 目前進度

這份文檔目前是純草稿，尚未開始實作。

目前代碼裡已經有的相關前置條件：

- `threadBridge` 已能保存 thread / workspace / Codex continuity
- final assistant reply 已有清楚的輸出節點
- local management API 已開始提供 setup / runtime / workspace query 與 control surface
- runtime 已有 `CODEX_MODEL` 與 image-provider 類設定，但尚未有「第二個 LLM」的正式設定模型

目前尚未完成：

- secondary LLM 的設定模型
- secondary LLM 的執行時機與責任邊界
- 「AI 建議」如何送回用戶的正式規格
- 「AI 目標」如何持久化、作用於 thread / workspace、以及如何影響 Codex 互動

目前新增確認的一個收斂方向是：

- `AI 建議` 的 delivery 應拆成分段模式，而不是只在 `manual` 與 `auto_append` 之間二選一
- 第一段應優先考慮以 Telegram inline button 形式組裝 AI 建議

## 問題

`threadBridge` 目前的主要對話鏈路是：

- 使用者輸入
- Codex 執行
- final reply 直接回到 Telegram 或其他 surface

這個模型簡單直接，但也有幾個限制：

- 無法對 Codex 的最終回覆再做一層可配置的後處理
- 無法根據用戶偏好或 workflow，額外生成「下一步建議」或「風險提醒」
- 若之後想接自定義 LLM，現在沒有一個正式邊界來定義它是在讀 Codex 回覆、讀 runtime event，還是直接接管主對話
- 若之後想讓「AI 目標」長期作用於某個 workspace 或 thread，目前也沒有穩定的 artifact / config model

因此這個問題不是單純「多加一個模型設定」，而是：

- `threadBridge` 是否要支援一個可選的 secondary LLM layer
- 這層要如何和 Codex、runtime protocol、delivery、adapter 分工

## 定位

這份文檔定義的是「可選的 secondary LLM guidance / goal layer」草稿。

它處理：

- secondary LLM 的設定模型
- Codex 回覆之後的 guidance / suggestion flow
- AI 目標如何作為 thread / workspace 的長期附加語義
- 這層如何和 runtime protocol / delivery / adapter 接起來

它明確不處理：

- 取代 Codex 作為主要執行引擎
- Telegram renderer / delivery 細節
- image provider 設定
- 多 provider prompt engineering 細節

換句話說，這層應被理解為：

- `Codex` 仍然是主工作代理
- `secondary LLM` 是可選的閱讀、建議、協調層

## 核心想法

### 1. 新增可配置的 secondary LLM API

threadBridge 應能保存一組與 Codex 分離的 LLM 設定。

初版至少要能表達：

- 是否啟用
- provider 名稱
- base URL
- API key 來源
- model
- timeout / rate limit 類保守執行參數
- system prompt / role prompt

這組設定不應和 `CODEX_MODEL` 混成同一件事。

## 2. secondary LLM 讀 Codex 回覆，生成 AI 建議

可接受的初版用途包括：

- 閱讀 final assistant reply
- 生成簡短的 follow-up suggestion
- 生成「你接下來可能想做的事」清單
- 生成風險提醒、遺漏點、驗證建議

這層不一定直接改寫 Codex 原始回覆。

比較穩定的初版應優先是：

- 保留 Codex 原始輸出
- secondary LLM 額外產生 `guidance` 類內容

### 3. 新增 AI 目標

除了單次建議，還可以讓某些長期目標掛在 thread / workspace 上，例如：

- 偏好更保守的 code review 建議
- 每次完成修改後都提醒是否缺少測試
- 遇到風險變更時主動給 rollback / migration 建議
- 對特定專案維持固定的產品/架構目標

這裡的 `AI 目標` 應理解為：

- 一組由用戶或系統附加的高層 instruction
- 不直接取代 Codex thread 的 continuity
- 但可被 secondary LLM 讀取，用來生成更符合目的的 guidance

## 建議的資料模型

### `SecondaryLlmConfig`

至少包含：

- `enabled`
- `provider`
- `base_url`
- `api_key_source`
- `model`
- `timeout_ms`
- `system_prompt`
- `delivery_mode`
  - `disabled`
  - `manual`
  - `inline_button`
  - `auto_append`

初版建議先做：

- global config
- machine-local secret 來源

之後再決定是否補：

- per-workspace override
- per-thread override

### `AiGoal`

至少包含：

- `goal_id`
- `scope`
  - `global`
  - `workspace`
  - `thread`
- `title`
- `instruction`
- `enabled`
- `created_at`
- `updated_at`
- `source`
  - `user`
  - `system`

### `GuidanceArtifact`

至少包含：

- `thread_key`
- `workspace_cwd`
- `current_codex_thread_id`
- `source_message_id` 或等價關聯鍵
- `codex_reply_digest`
- `guidance_text`
- `goal_ids`
- `generated_at`

這個 artifact 可以是 append-only log，也可以先是 ephemeral result；但它不應和 conversation 主記錄混成同一份 source of truth。

## 作用時機

初版建議只在下面時機觸發：

- Codex final assistant reply 已完成
- thread 目前不在 error / broken 收斂流程中
- secondary LLM config 已啟用

初版不建議直接掛在：

- preview delta
- 每次 tool event
- 每次 status update

理由是：

- 這會把 secondary LLM 變成高頻 background worker
- 成本、延遲、失敗面都會暴增

## 與用戶互動的方式

目前比較合理的方向，是把 guidance delivery 理解成分段模式，而不是一開始就只有「不送」或「直接附加正文」。

### 第一段：`inline_button`

第一段應優先考慮把 `AI 建議` 組裝成 Telegram inline button 或等價的顯式 action surface。

這一段的目標比較像：

- 讓使用者看到「可採取的下一步建議」
- 由使用者主動點擊、展開或採納
- 避免 secondary LLM 直接把 thread 內容變得更冗長

比較合理的第一段形態可能是：

- 一則簡短提示訊息
- 搭配 1 到數個 inline buttons
- 每個 button 對應一個 guidance suggestion / action candidate

這樣做的好處是：

- 比 `auto_append` 更克制
- 比純 `manual` 更可見
- 比直接把建議附在 final reply 後面，更不容易污染主對話內容

這也表示 `AI 建議` 在第一段不只是純文字 artifact，而可能是一種：

- `guidance + control surface`

### 之後的段落

後續若要做第二段，才再決定是否進入：

- 更完整的 auto-append guidance
- 或其他不只 Telegram 的通用 guidance surface

在第二段正式定義前，先不要把它寫死。

初版可接受的模式：

- `manual`
  - secondary LLM 產物只記錄，不自動發給用戶
- `inline_button`
  - secondary LLM 產物以顯式 suggestion buttons 呈現
- `auto_append`
  - Codex final reply 後，再附一段明確標記的 `AI 建議`

不建議的初版模式：

- 讓 secondary LLM 直接覆蓋 Codex 原始 final reply
- 讓 secondary LLM 直接充當新的主對話代理
- 讓 secondary LLM 在沒有明確標示的情況下假裝是 Codex 原始輸出

## 與其他計劃的關係

- [runtime-protocol.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-protocol.md)
  - 之後若這層落地，需要新增 secondary LLM config view、goal view、guidance event 或等價 control action
- [message-queue-and-status-delivery.md](/Volumes/Data/Github/threadBridge/docs/plan/message-queue-and-status-delivery.md)
  - 若 `AI 建議` 要送回 Telegram，應由 delivery 規格決定它屬於 `content`、`status`、`control`，還是另一種 payload
- [runtime-transport-abstraction.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-transport-abstraction.md)
  - secondary LLM guidance 比較接近 core/runtime-side augmentation，不應一開始就寫死成 Telegram-only feature
- [telegram-webapp-observability.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-webapp-observability.md)
  - 之後 observability 應能看見 guidance 何時生成、使用了哪些 goal、是否送達用戶

## 風險

- 若 secondary LLM 沒有清楚標記，使用者可能誤以為它就是 Codex 原始回答
- 若 AI 建議自動發送過於頻繁，會讓 thread 噪音上升
- 若 inline button 承載的建議語義不清，使用者可能誤把 suggestion 當成已執行 action
- 若 `AI 目標` 與 Codex 當前 task 目標衝突，可能生成低品質或互相矛盾的建議
- 若把 secrets、provider payload、完整對話上下文處理得太寬，會擴大敏感資訊暴露面
- 若這層直接長進 Telegram adapter，而不是 runtime / protocol 邊界，之後很難移植到 custom app 或 web 管理面

## 開放問題

- secondary LLM config 應先做 global，還是直接支援 per-workspace？
- `AI 建議` 的第一段預設應該是 `manual` 還是 `inline_button`？
- inline button 點擊後，應展開建議文字、觸發 action，還是只是把建議插回輸入框？
- 第二段是否應該是 `auto_append`，還是另一種更通用的 guidance surface？
- `AI 目標` 應該只是 guidance prompt，還是允許驅動某些 control action？
- secondary LLM 能讀多少上下文？
  - 只讀 final reply
  - 讀最近一次 user prompt + final reply
  - 讀更完整的 turn / tool context
- guidance artifact 應該持久化在哪一層？
  - bot-local `data/`
  - workspace artifact
  - management API 只做 ephemeral view

## 建議的下一步

1. 先把這層收斂成「Codex final reply 後的可選 guidance layer」，不要一開始就讓 secondary LLM 接管主對話。
2. 先定義最小 `SecondaryLlmConfig` 與 `AiGoal`。
3. 先把第一段 guidance delivery 寫成 `inline_button` 草稿，明確和 `manual`、`auto_append` 分開。
4. 再回來補 `runtime-protocol` 與 `message-queue-and-status-delivery` 的對接語義。
