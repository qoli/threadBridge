# Codex 執行中阻止新輸入

## 目前進度

這份 Plan 的 `v1` 已經部分落地，不再是純草稿。

目前已實作：

- Telegram 文字訊息 busy gate
- 圖片保存後延後分析的 busy gate
- `/new_session`、`/repair_session` 受 busy 狀態保護
- `/stop` 已作為第一個正式 busy control action 落地
  - Telegram 可對目前 running turn 送出 app-server `turn/interrupt`
  - direct bot turn 與 shared TUI ingress 都已開始把 `turn_id` 寫入 workspace status，供 `/stop` 定位目前 active turn
- busy reject / command reject 文案已開始提示 `/stop`
- busy 狀態已經不只看 bot 本身，也會讀 workspace shared status
- Telegram 文字 turn 與圖片分析改成 background 執行；handler 會先寫入 busy，再快速返回
- 因此同一 Telegram chat / topic 的後續輸入，現在會在 busy gate 被明確拒絕，而不再主要表現成長 handler 造成的隱性串行化
- bot 啟動時已開始做 startup stale busy reconciliation，會回收上一個進程留下的 bot-owned stale busy
- busy gate 的診斷輸出已和 title suffix 解耦
  - `busy` 不再透過 Telegram topic title 呈現
  - title 只保留 `broken` 這類較 durable 的狀態

目前尚未實作：

- 顯式 queue 模型
- Telegram running input policy
  - `reject`
  - `queue`
  - `steer`
- 更完整的 `runtime-state-machine` 對齊
- Web App 觀測面上的正式狀態展示
- Telegram 互動式 busy control surface
  - 更完整的執行中提示 / callback / keyboard affordance
  - Busy Gate 下的新輸入策略
    - `STOP 並插入發言`
    - `序列發言`
- pending image batch 的顯式取消 control
  - 使用者若誤傳圖片，目前只能放著待分析，缺少直接丟棄這批圖片的 Telegram 動作

目前新增記錄的一個明確 bug 是：

- 使用者送出斜線命令 `STOP` / `/stop` 後，Telegram thread 目前可能無法自行恢復正常響應
- today 症狀是：往往需要重啟 bot，該 thread 才會重新接受後續輸入
- 這代表目前 interrupt request、busy gate release、或 Telegram adapter 後續收尾之間，至少有一段 state reconciliation 仍不可靠

目前新增固定的一條收斂方向是：

- Codex native busy truth 應回到 `app-server-ws-backend`
- `threadBridge` 不應再把 workspace snapshot / shared status / continuity 狀態拼成單一 Codex busy authority
- Telegram Busy Gate 應只翻譯 backend truth 到產品層 reject / control / prompt
- app-server `turn/steer` 已確認是 Codex `rust-v0.99.0` 起的原生 active-turn input API，若要支援 running 時追加訊息，應把它建模成明確 policy，而不是把所有 running 文字都暗中視為 queue 或 steer

目前已知邊界：

- `teloxide` 預設仍按 `ChatId` 分發 update；在 forum topic 場景下，同一個 supergroup 內的 topic 共享同一個 chat id
- 現在靠「先寫 busy、再把長 turn 丟到 background」已經能讓後續輸入命中 reject，但底層 dispatcher 仍不是 thread-aware 的併發模型
- 也就是說，目前已經解決了「同 chat 連發看起來像排隊」這個主要 UX 問題，但 Telegram ingress 語義仍未被正式抽象成獨立層
- bot-owned current-Codex-thread gate 目前主要仍信任 workspace 內的 per-session snapshot
  - `bot_turn_started` 會把 session 狀態寫成 `turn_running`
  - 雖然目前已補上 startup reconciliation，但仍沒有完整的 lease / heartbeat owner 模型
  - 也就是說，「bot crash 後永久卡死」這個最糟情況已開始有修復路徑，但 stale busy recovery 的語義仍未完全收斂

也要明確承認一個 today 問題：

- 目前 Busy Gate 仍是 CLI 架構遷移後逐步拼接出的 composite gate
- 它混合了 app-server active turn、workspace snapshot、continuity pointer、與 Telegram 產品層行為
- 長期不應繼續把這些混成同一個 Codex busy source of truth
- `/stop` 的 happy path 雖已存在，但 interrupt 後若 thread 仍卡在不可響應狀態，代表產品層目前仍不能保證 `STOP -> 可再次發言` 這條最基本閉環

## 背景

現在同一個 Telegram thread 在 Codex 尚未完成當前 turn 時，仍然可以繼續收到新的文字或圖片輸入。

這會帶來幾個問題：

- 使用者以為訊息會排隊，但實際上目前沒有清楚的 Telegram-thread scoped 執行閘控語義
- 同一個 Telegram thread 可能在前一個 active turn 尚未結束時，再次觸發新的 Codex 執行
- 預覽訊息、圖片分析、工具輸出與 conversation log 的時序會變得不穩定
- 之後如果要做 Web App 觀測，也很難定義 thread 當前究竟是 `idle`、`running`、還是 `queued`

## 目標

為每個 Telegram thread 建立明確的「執行中」狀態。

這份文檔若未特別註明：

- `thread`
  - 指 `Telegram thread`
- `目前正在執行的 turn`
  - 指 `current_codex_thread_id` 對應的 active Codex turn

當同一個 Telegram thread 已有一個 Codex turn 正在執行時：

- 阻止同一個 Telegram thread 的新文字訊息直接送入 Codex
- 阻止同一個 Telegram thread 的新圖片分析直接啟動
- 給使用者一個清楚且一致的 Telegram 提示
- 讓使用者在 Telegram UI 上能直接採取下一步動作，而不只是被動看到 reject 訊息
- 讓後續觀測面可以正確顯示 thread 的 busy 狀態

這裡的「顯示 thread 的 busy 狀態」不再包含 title suffix。

這份文檔新增固定一條分層：

- backend native truth
  - 只回答 `thread_id` 是否 busy、目前 active `turn_id` 是誰、是否可 interrupt、目前 turn phase 是什麼
- `threadBridge` product gate
  - 決定 Telegram 要不要 reject
  - 決定圖片是暫存還是立即分析
  - 決定 `/stop`、queue、或其他 follow-up control 如何呈現

## 建議方向

第一階段不做排隊，先做硬性阻止。

但 Busy Gate 的 authority 應改成：

- 先以 `current_codex_thread_id` 查 backend native busy truth
- 再由 Telegram adapter / shared runtime 把這個 truth 翻譯成 reject / control / prompt
- 而不是先看 workspace snapshot，再倒推出 Codex 應該正在跑

建議語義：

- 同一個 Telegram thread 同一時間只允許一個 active Codex turn
- 如果目前 `current_codex_thread_id` 對應的 active turn 正在執行，新進文字訊息直接回覆 busy 提示，不寫入 Codex turn
- 如果目前 `current_codex_thread_id` 對應的 active turn 正在執行，新進圖片只允許保存為待處理素材，不能立即啟動分析
- `/new_session`、`/repair_session` 這類命令也應該定義是否受 busy 狀態保護
- busy gate 應明確區分：
  - `reject` 新輸入
  - `control` 目前正在執行的 turn
  - `prompt` 使用者下一步可能想做的事
- busy gate 與 title status 應明確分層：
  - busy gate 負責 runtime blocking
  - title 只承載相對穩定的 durable state

也要固定以下幾條約束：

- Telegram Busy Gate 只翻譯 `current_codex_thread_id` 的 backend busy truth
- `tui_active_codex_thread_id` 不應自動混進 Telegram Busy Gate 的 truth source
- workspace shared status / session snapshot 仍可作 observability、debug、兼容資料
- 但它們不應再被描述成 Codex native busy authority

如果 backend busy API 不可用，語義應是：

- 這代表 `app-server-ws-backend` 已發生 lifecycle / runtime 問題
- Telegram 不應假裝知道 busy 或 idle
- 這時應回 runtime error / degraded / unavailable，而不是 fallback 到 derived snapshot 猜測 truth

同時，busy gate 不能只考慮「正常完成時如何釋放」，也要定義 crash recovery。

建議補上 bot-owned busy snapshot 的失效語義：

- 如果 `owner = bot` 且 phase 仍是 `turn_running` / `turn_finalizing`，不能無限期把 snapshot 視為真實執行中
- threadBridge 重啟後，應能辨識「這是上一個 bot 進程留下的 stale busy」
- current-Codex-thread gate 應有明確的自我修復路徑，而不是要求使用者等待一個永遠不會完成的 turn

可接受的 v1 方向至少應落地其中一種：

- `lease / heartbeat`
  - bot 在執行 turn 期間週期更新 bot-owned session snapshot
  - gate 讀取時若超過閾值未更新，就把它視為 stale busy 並清回 idle / broken
- `startup reconciliation`
  - bot 啟動時掃描所有 bot-owned running session
  - 若它們不對應到當前進程可恢復的 active turn，就主動寫回 `bot_turn_failed` 或等價的 recovered state
- `instance ownership`
  - bot-owned snapshot 帶上 bot instance id 或類似 lease owner
  - 新進程只在確認 owner 已失效時接管或清除 stale busy

這裡的重點不是要把 `running` 變成長期 persistent source of truth，而是避免 crash 後的 stale gate 讓 thread 永久卡住。

對 `/stop` 也要補上一條同等明確的要求：

- `STOP` 成功、失敗、逾時 fallback、以及 backend 已中止但 adapter 尚未收尾，最終都必須把 Telegram thread 收斂回可再次互動的狀態
- 不能接受「interrupt 已送出，但 thread 仍需重啟 bot 才恢復」這種產品行為
- 若 backend truth 無法確認已回到 idle，應回 degraded / broken，而不是讓 Telegram thread 靜默卡死

## 提示文案方向

建議先使用明確、低歧義文案：

- `Codex 仍在處理上一個請求，請等待目前回合完成後再發送新訊息。`
- 如果是圖片分析按鈕或圖片輸入，可以補充：
  - `圖片已保存，但目前不會立即分析。`

如果 thread 目前存在待分析圖片批次，也應補一類文案：

- `這批圖片已排入待分析。`
- `如果這是誤傳，請直接取消這批圖片，再重新上傳。`

## Telegram 互動控制面

busy gate 的 Telegram UX 不應只有純文字拒絕。

當 thread 進入 `running` 時，Telegram 端應考慮提供一個輕量但明確的控制面，至少支援：

- 停止目前 AI 回應
- 顯示目前狀態
- 提示使用者可在完成後再送出的 follow-up 類型
- 讓使用者決定新輸入要採：
  - `STOP 並插入發言`
  - `序列發言`

### 建議的 v1 方向

- 主要控制面使用 `ReplyKeyboardMarkup`
  - 在 `running` 階段暫時替換使用者鍵盤
  - 讓輸入框前的預設操作收斂成少數幾個明確動作
  - 例如 `STOP`、`顯示狀態`、`等完成`
- 結束 `running` 後必須明確送出 `ReplyKeyboardRemove`
  - 避免 busy 階段的限制性鍵盤殘留在後續正常對話
- `InlineKeyboardMarkup` 不是這條 UX 的主方向
  - inline button 比較像附著在訊息上的操作
  - 但這裡真正想要的是直接影響使用者輸入面

這樣切分的原因是：

- busy gate 是 Telegram-thread scoped runtime 語義
- 這條方案的目標不是多一個按鈕，而是用 UX 引導減少自由輸入
- `ReplyKeyboardMarkup` 比較接近「在執行中暫時替換輸入面」
- 但它仍只是 UX 層限制，不應取代真正的 server-side busy gate

## Telegram Running Input Policy

這裡新增一個獨立於 execution mode / collaboration mode 的 Telegram adapter 設定：

```text
running_input_policy = reject | queue | steer
```

定位：

- 這是 Telegram ingress UX policy，不是 workspace execution mode。
- 它只決定同一 Telegram workspace thread 在 `run_status=running` 時，普通文字輸入如何處理。
- 它不改變 `run_status` 的 canonical values；v1 仍只有 `idle | running`，不因 `queue` policy 就新增 canonical `queued`。
- 它不應放進 `.threadbridge/state/workspace-config.json`；較合理的 owner 是 bot-local thread metadata，並可有 bot-wide default。

建議預設值：

- `reject`

原因：

- 它維持既有 v1 行為。
- 它不會把使用者的下一個任務誤解成對目前 active turn 的修正。
- 它不需要先引入 queue artifact、取消語義或 Codex 版本能力檢查。

### `reject`

語義：

- running 時普通文字不送入 Codex。
- Telegram 回覆明確 busy 提示。
- 使用者仍可走 `/stop` 或等目前 turn 完成。

適用場景：

- 保守預設。
- Codex backend 不支援 `turn/steer`。
- backend busy truth 不可用或 degraded，無法安全判斷 active turn。

### `queue`

語義：

- running 時普通文字保存為下一個待送 turn。
- 目前 active turn 不受影響。
- active turn 完成且 thread 仍為 `active + healthy` 後，再以 `turn/start` 啟動新的 turn。

v1 限制：

- 每個 Telegram thread 最多一則 pending queued input。
- 第二則 pending input 必須明確拒絕或明確覆蓋；不能默默形成無上限 queue。
- 必須提供可觀測狀態與取消語義，避免使用者以為 threadBridge 已支援完整任務隊列。
- queued input 應綁定當時的 `current_codex_thread_id`，若 session binding 在等待期間改變，必須取消或要求使用者確認。

不應做的事：

- 不應把 `queue` 表達成 canonical `run_status=queued`。
- 不應在沒有 cancel/replace 規則時把多則訊息堆成隱性 FIFO。

### `steer`

語義：

- running 時普通文字送入 app-server `turn/steer`。
- 使用目前 backend active `turn_id` 作為 `expectedTurnId`。
- 不開新 turn。
- 不期待新的 `turn/started` notification。
- 使用者輸入應在 transcript / delivery 中歸屬到既有 active turn。

前置條件：

- backend busy truth 必須能提供 `thread_id`、active `turn_id`、以及該 turn 是否可 steer。
- Codex 版本必須支援 app-server `turn/steer`；最低正式版本是 `rust-v0.99.0`，alpha 最低是 `rust-v0.99.0-alpha.4`。
- 如果 active turn 是 review / manual compaction / user shell 等 non-steerable kind，應回退為 reject 或給出明確不可 steer 提示，而不是自動 `turn/start`。

適用場景：

- 使用者補充或修正目前任務，例如「剛剛那個先別做，先看 failing tests」。
- 使用者想改變當前回合方向，而不是排下一個任務。

不適用場景：

- 使用者要在目前任務完成後再問下一題。
- 使用者要保留目前 turn 不變，另外排一個新 turn。
- 圖片分析 v1 不應直接混入 `steer`；圖片仍走 pending image batch / explicit analyze control。

### 優先級與例外

running input policy 不應攔截所有 Telegram 輸入。

優先級建議：

1. 已存在的 interactive prompt / `request_user_input` 回覆優先。
2. `/stop`、狀態查詢、policy 設定、session repair 等控制命令優先。
3. 圖片與文件走各自的 media gate / pending batch policy。
4. 只有普通文字才套用 `running_input_policy`。

建議 command surface：

- `/get_running_input_policy`
- `/set_running_input_policy reject`
- `/set_running_input_policy queue`
- `/set_running_input_policy steer`

如果也要支援 bot-wide default，建議命名保持 Telegram adapter scope，例如：

```text
TELEGRAM_RUNNING_INPUT_POLICY=reject
```

這個環境設定只作為新 thread 的預設值；每個 workspace thread 仍可覆蓋。

### `STOP ai 回應`

`STOP ai 回應` 應被視為 busy gate 的第一個正式 control action，而不是單純的字串命令別名。

這意味著之後應明確定義：

- `stop` 作用在目前哪個 active turn
- 何時允許 stop
- stop 成功、失敗、已結束時 Telegram 應顯示什麼狀態
- stop 後 preview / final / busy state 如何收斂

### `STOP 並插入發言`

這條應被視為 `STOP ai 回應` 的進一步產品語義，而不是單純兩個分離按鈕碰巧一起出現。

建議語義：

- 使用者在 `running` 期間送出一則新輸入
- Telegram control surface 允許使用者選擇：
  - 先停止目前回應
  - 再把這則新輸入插入成新的下一個 turn
- 這裡的「插入」不是保留舊 turn 與新 turn 同時執行
- 它代表：
  - 目前 active turn 被顯式中止
  - 新輸入成為新的 active turn 起點

這條能力的好處是：

- 比單純 reject 更符合聊天直覺
- 比模糊地讓使用者重打一遍更明確
- 也比直接暗中覆蓋目前 turn 更安全，因為 stop 是顯式的

需要明確定義的點：

- 什麼時機仍允許 stop-and-insert
- 若 stop 已來不及，是否退回普通 busy reject
- 新輸入是保留原文直接重送，還是要顯式確認
- preview / final / busy keyboard 如何在 stop-and-insert 後收斂

### `序列發言`

這條應被理解成 busy gate 下的有限 queue 語義，而不是一般自由輸入重新放開。

建議語義：

- 使用者在 `running` 期間輸入一則新訊息
- Telegram control surface 允許使用者選擇把它記成「下一個待送 turn」
- 目前 active turn 不被中止
- 新輸入在目前 turn 結束後，才進入新的 turn

這裡要明確承認：

- 這已經比單純 reject 更接近 queue
- 因此若要做，應先限制成非常明確的 v1 形態

例如：

- 同一時間最多只保留一則 sequenced utterance
- 新的 sequenced utterance 會覆蓋舊的，或明確拒絕第二則
- Telegram UI 必須顯示目前已有一則待序列發言
- 使用者應可取消這則 sequenced utterance

如果這些不先固定，`序列發言` 很容易重新回到：

- 使用者以為整個 thread 已經有完整 queue
- 但 runtime 實際只有半套排隊語義

### AI 回應中的提示

這裡的「AI 回應中的提示」建議先理解成：

- 在執行中或結束後，給使用者的 follow-up affordance
- 而不是隱含 queue 語義

例如：

- `等待完成後再問`
- `顯示目前狀態`
- `停止回應`

v1 應避免讓這些提示看起來像 Telegram 端已經支援「排隊下一個請求」；它們比較像被允許的有限輸入，而不是 queue。

但目前新增記錄的一個產品方向是：

- Telegram busy gate 之後可評估兩種明確 follow-up 策略：
  - `STOP 並插入發言`
  - `序列發言`

其中較保守的落地順序應是：

1. 先做 `STOP ai 回應`
2. 再做 `STOP 並插入發言`
3. 最後才評估是否引入 `序列發言`

原因是：

- `STOP 並插入發言` 仍然建立在單 active turn 模型上
- `序列發言` 則已開始引入顯式 queue 語義
- 兩者的實作風險與對使用者心智的影響不同，不應在文案上混成同一件事

### Pending Image Batch 的控制面

pending image batch 不是 `running` control 的特例，而是另一種需要顯式收斂的 Telegram control surface。

目前已存在：

- 圖片上傳後保存為待分析 batch
- 一則 control message 顯示目前 batch 狀態
- `直接分析` inline button

目前缺口：

- 缺少 `取消這批圖片` 或等價 control action
- 使用者若誤傳圖片，只能等待下一次分析時機或想辦法用文字覆蓋意圖，這不是真正的撤銷
- pending batch 若長時間存在，Telegram UI 會看起來像系統已接受了一個未來一定會執行的請求，容易造成 queue 幻覺

建議的 v1 語義：

- pending image batch control message 至少提供兩個 action：
  - `直接分析`
  - `取消這批圖片`
- `取消這批圖片` 的作用只限於尚未開始分析的 pending batch
- 取消後應刪除或失效化 `pending-image-batch.json`，並讓 control message 明確收斂成已取消狀態
- 取消不應影響目前 thread 的 lifecycle / binding / run status
- 取消後若使用者重新上傳圖片，應建立新的 batch，而不是沿用已取消 batch 的 identity

這個 control action 的重點不是提供 queue 管理，而是讓「圖片已保存但尚未分析」這個中間態可逆。

## 狀態模型

後續可以把 thread 狀態明確分成：

- `idle`
- `running`
- `broken`
- `archived`

其中 `running` 應該是短期執行態，不一定要持久化到本地檔案，但要能被 Telegram 層與 Web App 觀測層讀到。

## 實作注意點

- 這個 busy gate 應該是 Telegram-thread scoped，而不是 process-level
- 文字訊息與圖片分析必須共用同一套 gate，避免其中一路繞過
- `/workspace_info` 等診斷面也應共用同一套 gate resolver，不能再混用 `current_codex_thread_id` 視角與 title 視角
- 如果 Codex 執行失敗、超時或程序中斷，busy 狀態必須可靠釋放
- 對 bot-owned current-Codex-thread gate，需要額外定義 crash 後的 stale snapshot 回收機制
  - 不能只依賴 `bot_turn_completed`
  - 應明確規定由 heartbeat timeout、startup reconciliation、或其他 lease 模型負責解除卡死 busy gate
- 預覽訊息更新期間不能讓第二個 turn 把第一個 turn 的 draft 狀態污染掉
- log 需要能看出一次輸入是被拒絕、延後，還是成功進入執行
- 如果引入 Telegram 控制面，log 也應區分：
  - busy reject
  - reply keyboard shown
  - reply keyboard removed
  - control text accepted
  - stop accepted / stop ignored / stop failed
- pending image batch 的 control action 也應記 log，例如：
  - image batch analyze accepted
  - image batch cancel accepted
  - image batch cancel ignored
  - image batch control message updated
- Telegram 目前是靠 background turn + shared status 達成 reject，不是靠 thread-aware dispatcher 原生保證
- 若未來要把這個語義做成更乾淨的 transport/runtime 邊界，仍應考慮 ingress 層或 dispatcher 分發策略
- forum topic 場景下，仍需明確驗證 `ReplyKeyboardMarkup` 的 user/thread 作用範圍，避免同 chat 其他 topic 受到錯誤影響

## 與其他計劃的關係

- 和 [telegram-webapp-observability.md](../telegram-adapter/telegram-webapp-observability.md) 直接相關
  - Web App 若要顯示 thread 即時狀態，需要先有明確的 busy 語義
- 和 [topic-title-status.md](../telegram-adapter/topic-title-status.md) 相關
  - 若之後想把 topic title 當狀態欄，`running` 會是重要訊號
- 和 [message-queue-and-status-delivery.md](../telegram-adapter/message-queue-and-status-delivery.md) 直接相關
  - running 階段的 reply keyboard、移除鍵盤時機、以及相關 status message 屬於 Telegram delivery / control surface，不應直接混進 core gate 內部實作
  - pending image batch 的 `直接分析 / 取消這批圖片` control message 也應由 delivery 規格承接

## 暫定結論

這項應列為後續功能，不在目前版本立即實作。

短期推薦方案是：

- 先加入 Telegram-thread scoped busy gate
- 先採用「拒絕新輸入，不做排隊」
- 等 Web App 觀測面成形後，再決定是否升級成顯式 queue 模型

目前狀態可更新為：

- `v1` 已經達到使用者可感知的 reject 行為
- 但底層實作仍是「shared status + background turn」方案，不是最終的 ingress / state-machine 主規格
- bot process crash 後的 stale busy 回收仍未定義完整，current-Codex-thread gate 目前仍可能卡死
- 下一步應先把 Telegram running input policy 固定為 `reject | queue | steer`，預設 `reject`
- `steer` 可在不引入 canonical queue state 的前提下改善 running follow-up UX，但必須依賴 backend active turn truth 與 Codex `turn/steer` capability
- `queue` 應晚於 `steer` 或至少限制成 one-slot pending input，因為它需要 artifact、取消/覆蓋、binding drift 與完成後自動啟動 turn 的完整語義
- 下一步可以把 Telegram 互動控制面加進這份 plan：
  - `ReplyKeyboardMarkup` 作為主要 control surface
  - `ReplyKeyboardRemove` 作為退出 `running` 的必要收尾
  - `STOP ai 回應` 作為第一個正式 control action
  - 再評估 `STOP 並插入發言`
  - 最後才決定是否引入有限的 `序列發言`
