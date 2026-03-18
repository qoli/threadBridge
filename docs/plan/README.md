# 計劃草稿

這個目錄用來放 `threadBridge` 後續工作的設計草稿。

目前包含：

- [runtime-state-machine.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-state-machine.md)
  - `threadBridge` 狀態語義的主規格
  - 統一定義 `lifecycle_status`、`binding_status`、`run_status`
- [session-lifecycle.md](/Volumes/Data/Github/threadBridge/docs/plan/session-lifecycle.md)
  - 重新定義現在的 session / thread / workspace 綁定模型
  - 後續應引用 `runtime-state-machine` 的狀態語義
- [codex-busy-input-gate.md](/Volumes/Data/Github/threadBridge/docs/plan/codex-busy-input-gate.md)
  - 討論 Codex 執行中時，如何阻止同一 Telegram thread 繼續送入新輸入
  - 後續應引用 `runtime-state-machine` 的 `run_status`
- [codex-cli-telegram-status-sync-hooks.md](/Volumes/Data/Github/threadBridge/docs/plan/codex-cli-telegram-status-sync-hooks.md)
  - 用 Bash wrapper、Codex hooks、notify 把本地 CLI 狀態同步回 Telegram
  - 定義共享 workspace status surface 與 topic title / busy gate 的整合
- [topic-title-status.md](/Volumes/Data/Github/threadBridge/docs/plan/topic-title-status.md)
  - 討論如何把 Telegram topic title 當成工作狀態欄
  - 後續應引用 `runtime-state-machine` 的狀態軸
- [message-queue-and-status-delivery.md](/Volumes/Data/Github/threadBridge/docs/plan/message-queue-and-status-delivery.md)
  - Telegram adapter 的 outbound delivery v1 規格
  - 只處理 preview / final / status / edit 送信邊界，不處理 input queue
- [telegram-webapp-observability.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-webapp-observability.md)
  - 用 Telegram Web App 補上 Codex 執行觀測面
- [telegram-markdown-adaptation.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-markdown-adaptation.md)
  - 討論如何讓 threadBridge 的輸出穩定適配 Telegram markdown 表示
  - 目前已部分實作：final assistant reply 已有 Telegram HTML renderer、plain-text fallback、attachment fallback
- [optional-agents-injection.md](/Volumes/Data/Github/threadBridge/docs/plan/optional-agents-injection.md)
  - 討論將 workspace `AGENTS.md` appendix 注入改成可選能力
- [runtime-transport-abstraction.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-transport-abstraction.md)
  - 將 threadBridge 收斂為核心 runtime 與可插拔 transport adapter
- [runtime-protocol.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-protocol.md)
  - 定義 core runtime 與 Telegram / custom app 之間的透明事件協議
  - 後續若定義 `ThreadStateView`，應對齊 `runtime-state-machine`
- [telegram-adapter-migration.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-adapter-migration.md)
  - 規劃如何把現有 Telegram bot 重構成一個 adapter，而不是整個產品邊界

這些文件大多仍是草稿，不代表已經完全定案或完整實作。

目前可以先按下面的主從關係理解：

- `runtime-state-machine.md`
  - 狀態語義主規格
- `message-queue-and-status-delivery.md`
  - Telegram delivery 主規格

目前相對進度最高的是：

- [telegram-markdown-adaptation.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-markdown-adaptation.md)
  - 已有部分程式碼落地，尤其是 final assistant reply 的 Telegram renderer 路徑
