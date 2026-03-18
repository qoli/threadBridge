# 計劃草稿

這個目錄用來放 `threadBridge` 後續工作的設計草稿。

目前包含：

- [session-lifecycle.md](/Volumes/Data/Github/threadBridge/docs/plan/session-lifecycle.md)
  - 重新定義現在的 session / thread / workspace 綁定模型
- [codex-busy-input-gate.md](/Volumes/Data/Github/threadBridge/docs/plan/codex-busy-input-gate.md)
  - 討論 Codex 執行中時，如何阻止同一 Telegram thread 繼續送入新輸入
- [topic-title-status.md](/Volumes/Data/Github/threadBridge/docs/plan/topic-title-status.md)
  - 討論如何把 Telegram topic title 當成工作狀態欄
- [telegram-webapp-observability.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-webapp-observability.md)
  - 用 Telegram Web App 補上 Codex 執行觀測面
- [telegram-markdown-adaptation.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-markdown-adaptation.md)
  - 討論如何讓 threadBridge 的輸出穩定適配 Telegram markdown 表示
- [optional-agents-injection.md](/Volumes/Data/Github/threadBridge/docs/plan/optional-agents-injection.md)
  - 討論將 workspace `AGENTS.md` appendix 注入改成可選能力
- [runtime-transport-abstraction.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-transport-abstraction.md)
  - 將 threadBridge 收斂為核心 runtime 與可插拔 transport adapter
- [runtime-protocol.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-protocol.md)
  - 定義 core runtime 與 Telegram / custom app 之間的透明事件協議
- [telegram-adapter-migration.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-adapter-migration.md)
  - 規劃如何把現有 Telegram bot 重構成一個 adapter，而不是整個產品邊界

這些文件目前都是草稿，不代表已經定案或已經實作。
