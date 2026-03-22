# Session Sync Manual Test

這份文檔是 `threadBridge` 目前 local/TUI mirror、adoption、與 idle/free readiness 的手動測試清單。

它描述的是已落地行為，不是舊 `CLI owner / handoff` 模型。

## 先記住語義

- `hcodex`
  - 受管本地 TUI 入口
- `mirror`
  - 把 local/TUI prompt、assistant final、process transcript 映射回 Telegram
- `pending_adoption`
  - 本地 TUI session 已可採納，但 Telegram 尚未切換 continuity
- `idle/free`
  - Telegram 可安全發起下一個 turn

目前已落地的是：

- `hcodex` 是受管本地入口
- owner 是 desktop runtime，不是本地 CLI
- owner thread 透過 `thread_key` 決定
- Telegram 會看到 local/TUI mirror 與 rolling draft
- `/workspace_info` 會暴露 `thread_key`、selected session、marker、adoption 與 owner 狀態

目前沒有落地的是：

- raw `codex` 作為受管入口
- 完整 token / delta 級別 live streaming

## 前置條件

1. bot 已重啟到包含 desktop owner、TUI proxy、mirror、draft 的版本
2. 你有一個已綁定 workspace 的 Telegram thread
3. 該 workspace 已安裝 `.threadbridge/` runtime surface

以下示例假設 workspace 是：

```bash
/Volumes/Data/Github/codex
```

## 測試 0: 先拿到 `thread_key`

在對應的 Telegram thread 執行：

```text
/workspace_info
```

檢查點：

1. 回覆裡應包含：
   - `thread_key`
   - `workspace`
   - `current_codex_thread_id`
   - `tui_active_codex_thread_id`
   - `adoption_state`
   - `marker`
   - `current_owner`
2. 如果 workspace 有多個 active bound threads，後面的 `hcodex` 需要帶：

```bash
--thread-key <thread-key>
```

## 測試 1: 用 `hcodex` 啟動受管本地 session

在本地終端執行：

```bash
cd /Volumes/Data/Github/codex
source ./.threadbridge/shell/codex-sync.bash
type hcodex
hcodex
```

如果這個 workspace 有多個 active bound threads，改為：

```bash
hcodex --thread-key <thread-key>
```

檢查點：

1. `type hcodex` 應顯示 `hcodex is a shell function`
2. 在 TUI 裡送一條簡短 prompt，例如：

```text
hi
```

3. owner thread 的 Telegram topic title 會顯示 busy 狀態
4. `/workspace_info` 會顯示：
   - `tui_active_codex_thread_id`
   - `current_owner`
   - `marker`

## 測試 2: local/TUI 文本鏡像到 owner thread

保持剛才的 `hcodex` session 活著，在 TUI 裡送兩條簡短 prompt，例如：

```text
這個項目是做什麼的？
```

```text
再用一句話總結
```

檢查點：

1. owner thread 應收到：
   - `👤` header 的 user mirror 文本
   - `🤖` header 的 final assistant 文本
2. Telegram draft 應顯示：
   - line1：`●` / `○`
   - line2：目前 preview / completed 文本
3. 其他同 workspace thread 不應收到這些鏡像文本

## 測試 3: process transcript 與 draft

在 TUI 裡送一條會觸發 plan 或 tool 的 prompt，例如：

```text
先看最近 2 個 commit，再用一句話總結。
```

檢查點：

1. Telegram draft 應先顯示 assistant preview 文本
2. draft 應保持兩行格式：
   - line1：`●` / `○`
   - line2：preview 正文
3. final assistant 回覆應以 `🤖` header 發送
4. management transcript API 應可讀到 `delivery=process` 的條目

## 測試 4: adoption / reject

讓本地 TUI 產生一個待採納 session，然後回到 Telegram。

檢查點：

1. management UI 會顯示 `pending_adoption`
2. Telegram callback 採納後：
   - `current_codex_thread_id` 會切到 `tui_active_codex_thread_id`
   - 系統提示使用 `❗️` header
3. Telegram callback 拒絕後：
   - `current_codex_thread_id` 保持原 session
   - 系統提示使用 `❗️` header

## 測試 5: idle/free readiness

在本地 TUI turn 跑完後，再從 Telegram 發送普通文字。

檢查點：

1. 若 session 已 idle/free，Telegram 請求不應被 busy gate 擋住
2. 若 session 仍 busy，Telegram 應收到 `❗️` header 的 busy/system 提示
3. readiness 判斷不再依賴舊 `CLI owner` 或 `/attach_cli_session`

## 預期總結

這輪手動測試驗證的是：

- `desktop runtime owner + local/TUI mirror + idle/free readiness` 已取代舊 handoff 模型
- Telegram 文本格式已收斂成：
  - `👤`
  - `🤖`
  - `❗️`
  - `●` / `○`
- draft 與 final reply 的顯示語義已比舊 plain-text path 更一致
