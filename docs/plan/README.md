# Plan Index

這個目錄用來放 `threadBridge` 的設計草稿、已落地方案與後續重構方向。

## 閱讀方式

- 先看「已落地 / 部分落地 / 純草稿」區分，不要把所有文件都當成同一成熟度。
- 再看「主規格」與「依賴關係」。
- 單篇文檔內的 `目前進度` 是這次整理後的最新狀態註記。

## 已落地

- [codex-cli-telegram-status-sync-hooks.md](/Volumes/Data/Github/threadBridge/docs/plan/codex-cli-telegram-status-sync-hooks.md)
  - 已完成 v1
  - Bash wrapper、Codex hooks、notify、workspace shared status、topic title watcher、busy gate 都已落地

## 部分落地

- [telegram-markdown-adaptation.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-markdown-adaptation.md)
  - final reply 的 Telegram HTML renderer、plain-text fallback、attachment fallback 已落地
- [codex-busy-input-gate.md](/Volumes/Data/Github/threadBridge/docs/plan/codex-busy-input-gate.md)
  - v1 忙碌閘控已落地
  - 但 queue 模型與更完整的狀態語義仍未收斂
- [topic-title-status.md](/Volumes/Data/Github/threadBridge/docs/plan/topic-title-status.md)
  - 已落地 `workspace/title + cli/bot/broken suffix`
  - context ratio 仍未實作
- [session-lifecycle.md](/Volumes/Data/Github/threadBridge/docs/plan/session-lifecycle.md)
  - `/new_thread`、`/bind_workspace`、`/new`、`/reconnect_codex` 的基本生命週期已存在
  - 更完整的 runtime 主模型仍待收斂

## 純草稿

- [runtime-state-machine.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-state-machine.md)
  - 狀態語義主規格草稿
- [message-queue-and-status-delivery.md](/Volumes/Data/Github/threadBridge/docs/plan/message-queue-and-status-delivery.md)
  - Telegram outbound delivery 主規格草稿
- [telegram-webapp-observability.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-webapp-observability.md)
  - Telegram Web App 觀測面草稿
- [optional-agents-injection.md](/Volumes/Data/Github/threadBridge/docs/plan/optional-agents-injection.md)
  - appendix 注入可選化草稿
- [runtime-transport-abstraction.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-transport-abstraction.md)
  - core runtime / adapter 抽象化草稿
- [runtime-protocol.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-protocol.md)
  - runtime 協議草稿
- [telegram-adapter-migration.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-adapter-migration.md)
  - Telegram adapter 遷移草稿

## 主規格

- [runtime-state-machine.md](/Volumes/Data/Github/threadBridge/docs/plan/runtime-state-machine.md)
  - 目標是未來的狀態語義主規格
- [message-queue-and-status-delivery.md](/Volumes/Data/Github/threadBridge/docs/plan/message-queue-and-status-delivery.md)
  - 目標是 Telegram delivery 主規格

目前這兩份都還沒有變成實際代碼的唯一 source of truth。

## 依賴關係

- `session-lifecycle`
  - 描述 thread / workspace / Codex thread 的生命週期
- `codex-busy-input-gate`
  - 描述 turn 互斥與 busy gate
- `codex-cli-telegram-status-sync-hooks`
  - 把本地 CLI 狀態接到同一份 busy / title 模型
- `topic-title-status`
  - 描述 title 應承載哪些狀態
- `runtime-state-machine`
  - 最終應把上面幾份文件的狀態語義統一

## 備註

- 這個目錄現在同時包含已落地方案和未實作草稿，不能只看標題判斷成熟度。
- 如果某份文檔和代碼有衝突，先以代碼為準，再回來更新該文檔。
