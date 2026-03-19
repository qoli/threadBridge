# Session Sync Manual Test

這份文檔是 `threadBridge` 目前 CLI / Telegram 信息同步能力的手動測試清單。

它描述的是已落地行為，不是願景設計稿。

## 先記住語義

- `· cli`
  - 只有 owner thread 會顯示
  - 表示 `hcodex` 現在是 live，Telegram 只是 viewer
- `· attach`
  - 當前 Telegram thread 已接管原 CLI session
  - 表示 Telegram 現在是 live，本地終端只跑 `threadbridge_viewer`
- `· cli!`
  - workspace 的 CLI owner 狀態不可信或衝突
  - 例如 owner claim 缺失、registry 對不上、或出現多個 live CLI sessions

目前已落地的是：

- `hcodex` 是受管本地 CLI 入口
- owner thread 透過 `thread_key` 決定
- `.cli` 狀態下，Telegram viewer 只看 `CLI user + Codex final`
- `/attach_cli_session` 會 kill `codex` TUI，然後在同一個終端進入 `reedline` 只讀 viewer
- `.attach` 狀態下，本地 viewer 只看 attach 之後的 `Telegram user + Codex final`
- viewer 命令：
  - `r` / `resume`
  - `q` / `quit`
  - `help`
  - `reload`
- `/thread_info` 會暴露 `thread_key`、selected session、marker 和 owner 狀態

目前還沒有落地的是：

- CLI token / delta 級別 live streaming 到 Telegram
- Telegram preview draft live 進入本地 viewer
- raw `codex` 作為受管入口

## 前置條件

1. bot 已重啟到包含 `hcodex` / viewer / mirror 代碼的版本
2. 你有一個已綁定 workspace 的 Telegram thread
3. 該 workspace 已安裝 `.threadbridge/` runtime surface

以下示例假設 workspace 是：

```bash
/Volumes/Data/Github/codex
```

## 測試 0: 先拿到 `thread_key`

在對應的 Telegram thread 執行：

```text
/thread_info
```

檢查點：

1. 回覆裡應包含：
   - `thread_key`
   - `workspace`
   - `selected_session_id`
   - `attachment_state`
   - `marker`
   - `owner_thread`
   - `owner_session_id`
2. 如果 workspace 有多個 active bound threads，後面的 `hcodex` 需要帶：

```bash
--thread-key <thread-key>
```

## 測試 1: 用 `hcodex` 啟動受管 CLI owner

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
2. 在 CLI 裡送一條簡短 prompt，例如：

```text
hi
```

3. 只有 owner thread 的 Telegram topic title 變成 `· cli`
4. 其他同 workspace thread 不應顯示 `· cli`

這一步驗證的是：

- `hcodex` 而不是 raw `codex` 在管理受管 CLI
- owner thread 由 `thread_key` 明確決定
- `.cli` 表示 `hcodex live / Telegram viewer`

## 測試 2: CLI 文本鏡像到 owner thread

保持剛才的 `hcodex` session 活著，在 CLI 裡送兩條簡短 prompt，例如：

```text
這個項目是做什麼的？
```

```text
再用一句話總結
```

檢查點：

1. owner thread 應收到：
   - `CLI: <你的 prompt>`
   - `Codex: <最終回覆>`
2. 其他同 workspace thread 不應收到這些鏡像文本
3. 若你在 Telegram thread 執行 `/thread_info`：
   - `marker` 應是 `.cli`
   - `is_owner_thread` 應是 `yes`

這一步驗證的是：

- `CLI -> Telegram` mirror routing 只走 owner thread
- routing 依賴 `thread_key` owner claim，不依賴 workspace 廣播
- owner thread 只應看到 `CLI:` user 行與 `Codex:` final；不做 token / delta streaming

## 測試 3: `/attach_cli_session` 進入 `· attach`

在 owner thread 執行：

```text
/attach_cli_session
```

如果同一 workspace 出現多個 live CLI sessions，bot 會列出可選 session id。這時改為：

```text
/attach_cli_session <session-id>
```

檢查點：

1. Telegram topic title 從 `· cli` 變成 `· attach`
2. 原本的 `codex` TUI 會被結束
3. 同一個本地終端不回到普通 prompt，而是直接進入 `threadbridge_viewer` 的 `reedline` viewer
4. bot 回覆裡應給出：

```bash
hcodex resume <session-id> --thread-key <thread-key>
```

這一步驗證的是：

- attach 是排他式 handoff
- `codex` TUI 被 kill 後，由 viewer 在同一終端接棒
- `.attach` 表示 `Telegram live / 本地 viewer`

## 測試 4: `.attach` 狀態下 Telegram 接手輸入窗口

現在 viewer 已在本地終端中打開，不要執行 `resume`。改到 Telegram owner thread 發送普通文字，例如：

```text
用一句話說明這個 repo 是做什麼的
```

檢查點：

1. 這條 Telegram 請求不應被 owner gate 擋住
2. viewer 應顯示：
   - `Telegram: <你的輸入>`
   - `Codex: <最終回覆>`
3. `· attach` 標記應保留

這一步驗證的是：

- Telegram 已接管原 CLI session
- `Telegram -> viewer` 文本鏡像已生效
- viewer 只顯示普通文字消息與 assistant 最終回覆；不顯示命令、系統事件或圖片分析內部 prompt

## 測試 5: viewer 用 `r` / `resume` 回到本地 CLI

在 viewer prompt 裡輸入：

```text
r
```

檢查點：

1. viewer 應退出
2. 同一終端直接執行：

```bash
hcodex resume <session-id> --thread-key <thread-key>
```

3. Telegram topic title 從 `· attach` 回到 `· cli`
4. 在 Telegram owner thread 執行 `/thread_info`：
   - `marker` 應回到 `.cli`
   - `attachment_state` 應回到 `none`

這一步驗證的是：

- viewer 的正式恢復命令是 `r` / `resume`
- resume 會把 ownership 奪回給本地 CLI

## 測試 6: owner gate 生效

當前已回到 `.cli` 狀態。不要再 attach，直接在 Telegram owner thread 發一條普通文字，例如：

```text
你現在看到什麼上下文？
```

檢查點：

1. 這條 Telegram 請求應被擋住
2. 提示應要求你先執行 `/attach_cli_session`

這一步驗證的是：

- `.cli` 狀態下，本地 CLI 持有輸入權
- Telegram 不能直接往這個 selected session 發新 turn

## 測試 7: 多 thread workspace 下的顯式 `thread_key`

如果同一 workspace 綁了多個 active Telegram threads：

1. 在任一 thread 執行 `/thread_info`，拿到不同的 `thread_key`
2. 本地終端執行：

```bash
cd /Volumes/Data/Github/codex
source ./.threadbridge/shell/codex-sync.bash
hcodex
```

預期：

1. `hcodex` 應拒絕直接啟動
2. 它應要求你顯式傳：

```bash
hcodex --thread-key <thread-key>
```

這一步驗證的是：

- 多 thread workspace 下，owner thread 必須顯式決定
- 不允許模糊推斷 mirror target

## 測試 8: `.cli!` 衝突態

這個狀態通常只在異常或繞過受管入口時出現，例如：

- 直接用 raw `codex`
- 人工改動 `.threadbridge/state/codex-sync/cli-owner.json`
- workspace 內出現多個 live CLI sessions

檢查點：

1. 同 workspace 的 active threads 應顯示 `· cli!`
2. 此時不要依賴 owner-based mirror routing
3. `/attach_cli_session` 只能靠顯式 session id 選擇，不應假設 owner thread

這一步主要是確認衝突標記，不要求你日常主動製造。

## 失敗時先看什麼

### `hcodex` 啟不來

先檢查：

```bash
cd /Volumes/Data/Github/codex
source ./.threadbridge/shell/codex-sync.bash
type hcodex
```

如果 `type hcodex` 不是 shell function，說明 shell snippet 沒載入。

### 沒看到 `· cli`

先檢查：

```bash
tail -f .threadbridge/state/codex-sync/events.jsonl
cat .threadbridge/state/codex-sync/cli-owner.json
```

如果 `events.jsonl` 沒事件，代表 `hcodex` 沒進同步鏈。  
如果 `cli-owner.json` 不存在或 `thread_key` 不對，代表 owner claim 沒建立對。

### CLI 文本沒有鏡像到 Telegram

先看：

```bash
cat .threadbridge/state/codex-sync/cli-owner.json
rg -n 'user_prompt_submitted|turn_completed' .threadbridge/state/codex-sync/events.jsonl
```

以及 Telegram thread：

```text
/thread_info
```

檢查：

1. owner claim 的 `thread_key` 是否就是當前 thread
2. `marker` 是否是 `.cli`
3. thread 是否是 active，而不是 archived
4. `events.jsonl` 裡是否真的有 `user_prompt_submitted`

如果只有 `turn_completed`、沒有 `user_prompt_submitted`，owner thread 目前只會看到 `Codex:` final，不會補 `CLI:` user 行。這是刻意的嚴格行為，不會從 `input-messages` 或 rollout 歷史回補。

### `/attach_cli_session` 後沒有進 viewer

先看：

```bash
cat .threadbridge/state/codex-sync/attach-intent.json
```

如果 attach intent 還留著，表示 handoff 已下發，但原終端沒有正常執行 `hcodex` 的接棒邏輯。常見原因是：

- 不是用 `hcodex` 啟動的
- 終端裡不是當前 shell snippet 版本

### viewer 裡按 `r` 沒回到 CLI

先確認：

1. viewer 啟動命令裡的 `thread_key` 與 `session_id` 正確
2. `.threadbridge/shell/codex-sync.bash` 仍存在
3. 你沒有手動刪除 workspace 的 runtime surface

## 本文檔對應的能力邊界

這份手測清單對應的是 threadBridge 目前已落地的能力：

- `hcodex`
- owner claim / `thread_key`
- `.cli` / `.cli!` / `.attach`
- `/thread_info`
- `.cli` 下的 `CLI user + assistant final` 鏡像到 owner thread
- `/attach_cli_session` handoff
- `.attach` 下 viewer 顯示 `Telegram user + assistant final`
- viewer `r` -> `hcodex resume`

它不驗證以下尚未落地能力：

- CLI token / delta live streaming 到 Telegram
- Telegram preview draft live 進入 viewer
- raw `codex` 受管接入 threadBridge lifecycle
