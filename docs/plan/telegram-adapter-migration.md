# Telegram Adapter 遷移草稿

## 目前進度

這份文檔目前仍是草稿，尚未正式進入遷移階段。

目前已有少量前置收斂：

- final reply renderer 已有較清楚的 Telegram 專用邊界
- topic title watcher / busy gate 開始把平台表現和狀態來源分開

但整體架構仍未完成 Telegram adapter 化。

## 問題

如果方向是把 `threadBridge` 變成透明協議層，那目前最大的實際問題不是理論，而是：

- 現有產品入口就是 Telegram
- 許多功能已經圍繞 Telegram thread、message、preview、callback 在運作
- 若直接大幅抽象，很容易影響現有可用性

所以需要一份遷移草稿，回答：

- 如何在不打壞現有 Telegram UX 的前提下，把 Telegram 從產品核心邊界降成 adapter

## 方向

遷移策略應該是：

- 先抽語意
- 再搬責任
- 最後做第二個 adapter 驗證

不建議一開始就：

- 先做完整 custom app
- 或先大規模重寫所有 Telegram runtime 模組

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
- image upload / pending batch
- topic title 更新
- markdown renderer

這一階段的成果應該是一張責任表，而不是立即重構。

### Phase 2：定義 Telegram Adapter 邊界

為 Telegram 明確建立 adapter 語意。

Telegram adapter 應只負責：

- 解析 Telegram update
- 轉成 `InputEvent`
- 訂閱 `RuntimeEvent`
- 把事件渲染回 Telegram

core runtime 應負責：

- 驗證 busy gate
- 決定是否開始 turn
- 控制 Codex thread
- 發出 preview / tool / final / error 事件

### Phase 3：收斂平台專用 renderer

把 Telegram-specific 表示層都集中。

建議收斂成單獨邊界的內容：

- Telegram markdown / HTML formatter
- preview message edit policy
- topic title renderer
- Telegram media send helper

這樣 custom app 未來才不需要重用 Telegram 表示層。

### Phase 4：做第二個最小 adapter

不需要直接做完整產品級 custom app。

先做一個最小驗證面即可，例如：

- CLI adapter
- local HTTP demo adapter
- 簡單 WebSocket event viewer

目的不是做新 UI，而是驗證：

- core runtime 是否真的不依賴 Telegram
- protocol 是否足夠支撐另一個 client

### Phase 5：再決定 custom app 的正式形態

當第二個 adapter 跑通後，再決定：

- custom app 是本地桌面 app
- 本地 Web App
- 行動端 app
- 或其他嵌入式 client

## 與現有計劃的關係

這份遷移草稿和下面幾份直接相關：

- [session-lifecycle.md](/Volumes/Data/Github/threadBridge/docs/plan/session-lifecycle.md)
  - 決定 core runtime 的 thread / binding 模型
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

## 開放問題

- `/bind_workspace`、`/new`、`/reconnect_codex` 是否應被統一表示成 control action？
- custom app 是否需要 topic/title 這種概念，還是只需要 thread label？
- preview 在 custom app 裡應該是 delta stream、replace stream，還是 terminal-style replay？
- Telegram adapter 是否仍然是預設 entrypoint，還是未來要支援多 adapter 同時註冊？

## 建議的下一步

1. 先列一份目前 Telegram-only 行為清單。
2. 把 command、preview、renderer、title update 分別標記為 adapter 或 core。
3. 定義最小 protocol 後，讓 Telegram 先改走新邊界。
4. 做一個最小第二 adapter 驗證整個抽象。
