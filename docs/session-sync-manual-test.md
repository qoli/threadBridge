# Session Sync Manual Test

這份文檔是 `threadBridge` 目前 CLI / Telegram session 同步能力的手動測試清單。

它描述的是已落地行為，不是願景設計稿。

## 先記住語義

- `· cli`
  - 同一個 workspace 裡存在 live CLI session，而且本地 CLI 目前持有輸入權
- `· attach`
  - 當前 Telegram thread 已經接管原 CLI session，Telegram 現在持有輸入權

目前已落地的是：

- Telegram 可以 attach 到 live CLI session
- attach 時會自動終止本地 `codex` TUI
- attach 後，Telegram 新輸入會沿用同一個 Codex `thread.id`
- 本地重新 `codex resume <session-id>` 後，ownership 會回到 CLI

目前還沒有落地的是：

- CLI 正在進行中的 `item` / `delta` live 鏡像到 Telegram
- Telegram 發出的 turn 即時出現在 CLI TUI 畫面裡

## 前置條件

1. bot 已經重啟到包含 session sync 代碼的版本
2. 你有一個已綁定 workspace 的 Telegram thread
3. 該 workspace 已安裝 `.threadbridge/` runtime surface

以下示例假設 workspace 是：

```bash
/Volumes/Data/Github/codex
```

## 測試 1: CLI session 被偵測為 `· cli`

在本地終端執行：

```bash
cd /Volumes/Data/Github/codex
source ./.threadbridge/shell/codex-sync.bash
type codex
codex
```

檢查點：

1. `type codex` 應顯示 `codex is a shell function`
2. 進入 CLI 後，在 TUI 內送一條簡短 prompt，例如：

```text
hi
```

3. 對應的 Telegram topic title 應變成 `· cli`

這一步只是在驗證：

- 本地 Bash wrapper 生效
- CLI hook / notify 生效
- Telegram 看見同 workspace 的 live CLI session

## 測試 2: attach 後變成 `· attach`

在剛才對應的 Telegram thread 裡執行：

```text
/attach_cli_session
```

如果同一個 workspace 只有一個 live CLI session，bot 會直接 attach。

如果有多個 live CLI sessions，bot 會列出可選的 session id。這時改為：

```text
/attach_cli_session <session-id>
```

檢查點：

1. attach 成功後，topic title 應從 `· cli` 變成 `· attach`
2. 本地 `codex` TUI 應被結束，terminal 回到 prompt
3. bot 應回覆已 attach 到指定 live CLI session，並給出：

```bash
codex resume <session-id>
```

## 測試 3: `.attach` 狀態下，Telegram 可以接手當輸入窗口

attach 成功後，本地 CLI TUI 已退出，這時 Telegram 是唯一輸入窗口。

然後在 Telegram 同一個 thread 發送普通文字，例如：

```text
用一句話說明這個 repo 是做什麼的
```

檢查點：

1. 這條 Telegram 請求不應被 busy gate 擋住
2. 它應正常完成
3. `· attach` 標記應保留

這一步驗證的是：

- selected session 已被 Telegram 接管
- Telegram 可以接力原 CLI session 繼續對話

## 測試 4: selected-session busy gate 生效

在本地 CLI 裡送一條稍微慢一點的 prompt，例如：

```text
讀一下 repo 根目錄的 AGENTS.md，總結成 8 點
```

在它還沒完成時，立刻去 Telegram 同一個 thread 再發一條普通文字。

檢查點：

1. 這次 Telegram 應被 busy gate 擋住
2. 提示應該是 selected session 正在跑 turn，而不是整個 workspace 忙碌

這一步驗證的是 attach 後的 busy gate 只看同一個 selected session。

## 測試 5: 同 workspace 不同 session 時是 `· cli`

開第二個本地終端，再啟一個新的 CLI session：

```bash
cd /Volumes/Data/Github/codex
source ./.threadbridge/shell/codex-sync.bash
codex
```

在這個新 CLI session 裡也送一條簡短 prompt，讓它成為 live session。

然後觀察一個還沒有 attach 到這個新 session 的 Telegram thread。

檢查點：

1. 這個 thread 應顯示 `· cli`
2. 它不應自動變成 `· attach`
3. 只有你執行：

```text
/attach_cli_session
```

或：

```text
/attach_cli_session <session-id>
```

後，才會變成 `· attach`

這一步驗證的是：

- `.cli` 和 `.attach` 是 ownership 標記，不是單純忙碌標記
- attach 必須顯式完成，而且會接管 CLI 輸入權

## 失敗時先看什麼

### 看不到 `· cli`

先檢查：

```bash
cd /Volumes/Data/Github/codex
source ./.threadbridge/shell/codex-sync.bash
type codex
tail -f .threadbridge/state/codex-sync/events.jsonl
```

如果 `type codex` 不是 shell function，代表 wrapper 沒生效。

如果 `events.jsonl` 沒有新事件，代表 CLI 沒走進同步鏈。

### `/attach_cli_session` 說沒有 live CLI session

先檢查：

```bash
cat .threadbridge/state/codex-sync/current.json
ls .threadbridge/state/codex-sync/sessions
```

如果 `live_cli_session_ids` 是空的，說明目前沒有被 registry 視為 live 的 CLI session。

### `.attach` 之後 Telegram 仍被擋住

先看 selected session 的 phase 是否還在：

- `turn_running`
- `turn_finalizing`

這兩個狀態下，Telegram 新 turn 會被擋住，這是預期行為。

## 本文檔對應的能力邊界

這份手測清單對應的是 threadBridge 目前已落地的 session sync 能力：

- session registry
- selected session binding
- `/attach_cli_session`
- `.cli` / `.attach` title
- selected-session busy gate
- Telegram 往已 attach session 發 turn

它不驗證以下尚未落地能力：

- CLI turn 的完整 live event stream 同步到 Telegram
- Telegram turn 的 live UI 反映到 CLI TUI
