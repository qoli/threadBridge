# Codex 執行中阻止新輸入

## 目前進度

這份 Plan 的 `v1` 已經部分落地，不再是純草稿。

目前已實作：

- Telegram 文字訊息 busy gate
- 圖片保存後延後分析的 busy gate
- `/new`、`/reconnect_codex`、已綁定 thread 的 `/bind_workspace` 受 busy 狀態保護
- busy 狀態已經不只看 bot 本身，也會讀 workspace shared status
- Telegram 文字 turn 與圖片分析改成 background 執行；handler 會先寫入 busy，再快速返回
- 因此同一 Telegram chat / topic 的後續輸入，現在會在 busy gate 被明確拒絕，而不再主要表現成長 handler 造成的隱性串行化

目前尚未實作：

- 顯式 queue 模型
- 更完整的 `runtime-state-machine` 對齊
- Web App 觀測面上的正式狀態展示
- Telegram 互動式 busy control surface
  - `STOP ai 回應`
  - 執行中提示內的 follow-up affordance

目前已知邊界：

- `teloxide` 預設仍按 `ChatId` 分發 update；在 forum topic 場景下，同一個 supergroup 內的 topic 共享同一個 chat id
- 現在靠「先寫 busy、再把長 turn 丟到 background」已經能讓後續輸入命中 reject，但底層 dispatcher 仍不是 thread-aware 的併發模型
- 也就是說，目前已經解決了「同 chat 連發看起來像排隊」這個主要 UX 問題，但 Telegram ingress 語義仍未被正式抽象成獨立層
- bot-owned selected-session gate 目前仍信任 workspace 內的 per-session snapshot
  - `bot_turn_started` 會把 session 狀態寫成 `turn_running`
  - gate 判斷目前只看 snapshot 是否仍是 busy，沒有額外驗證 bot process / task 是否仍活著
  - 如果 bot 進程在 turn 結束前被意外終止，這個 session 可能停留在 stale `running`，導致後續 Telegram 文字、圖片分析與 session state 變更命令持續被 busy gate 拒絕

## 背景

現在同一個 Telegram thread 在 Codex 尚未完成當前 turn 時，仍然可以繼續收到新的文字或圖片輸入。

這會帶來幾個問題：

- 使用者以為訊息會排隊，但實際上目前沒有清楚的 thread-level 執行閘控語義
- 同一 thread 可能在前一個 turn 尚未結束時，再次觸發新的 Codex 執行
- 預覽訊息、圖片分析、工具輸出與 conversation log 的時序會變得不穩定
- 之後如果要做 Web App 觀測，也很難定義 thread 當前究竟是 `idle`、`running`、還是 `queued`

## 目標

為每個 Telegram thread 建立明確的「執行中」狀態。

當 thread 已有一個 Codex turn 正在執行時：

- 阻止同一 thread 的新文字訊息直接送入 Codex
- 阻止同一 thread 的新圖片分析直接啟動
- 給使用者一個清楚且一致的 Telegram 提示
- 讓使用者在 Telegram UI 上能直接採取下一步動作，而不只是被動看到 reject 訊息
- 讓後續觀測面可以正確顯示 thread 的 busy 狀態

## 建議方向

第一階段不做排隊，先做硬性阻止。

建議語義：

- 同一 thread 同一時間只允許一個 active Codex turn
- 如果 thread 正在執行中，新進文字訊息直接回覆 busy 提示，不寫入 Codex turn
- 如果 thread 正在執行中，新進圖片只允許保存為待處理素材，不能立即啟動分析
- `/new`、`/reconnect_codex`、`/bind_workspace` 這類命令也應該定義是否受 busy 狀態保護
- busy gate 應明確區分：
  - `reject` 新輸入
  - `control` 目前正在執行的 turn
  - `prompt` 使用者下一步可能想做的事

同時，busy gate 不能只考慮「正常完成時如何釋放」，也要定義 crash recovery。

建議補上 bot-owned busy snapshot 的失效語義：

- 如果 `owner = bot` 且 phase 仍是 `turn_running` / `turn_finalizing`，不能無限期把 snapshot 視為真實執行中
- threadBridge 重啟後，應能辨識「這是上一個 bot 進程留下的 stale busy」
- selected-session gate 應有明確的自我修復路徑，而不是要求使用者等待一個永遠不會完成的 turn

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

## 提示文案方向

建議先使用明確、低歧義文案：

- `Codex 仍在處理上一個請求，請等待目前回合完成後再發送新訊息。`
- 如果是圖片分析按鈕或圖片輸入，可以補充：
  - `圖片已保存，但目前不會立即分析。`

## Telegram 互動控制面

busy gate 的 Telegram UX 不應只有純文字拒絕。

當 thread 進入 `running` 時，Telegram 端應考慮提供一個輕量但明確的控制面，至少支援：

- 停止目前 AI 回應
- 顯示目前狀態
- 提示使用者可在完成後再送出的 follow-up 類型

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

- busy gate 是 thread-level runtime 語義
- 這條方案的目標不是多一個按鈕，而是用 UX 引導減少自由輸入
- `ReplyKeyboardMarkup` 比較接近「在執行中暫時替換輸入面」
- 但它仍只是 UX 層限制，不應取代真正的 server-side busy gate

### `STOP ai 回應`

`STOP ai 回應` 應被視為 busy gate 的第一個正式 control action，而不是單純的字串命令別名。

這意味著之後應明確定義：

- `stop` 作用在目前哪個 active turn
- 何時允許 stop
- stop 成功、失敗、已結束時 Telegram 應顯示什麼狀態
- stop 後 preview / final / busy state 如何收斂

### AI 回應中的提示

這裡的「AI 回應中的提示」建議先理解成：

- 在執行中或結束後，給使用者的 follow-up affordance
- 而不是隱含 queue 語義

例如：

- `等待完成後再問`
- `顯示目前狀態`
- `停止回應`

v1 應避免讓這些提示看起來像 Telegram 端已經支援「排隊下一個請求」；它們比較像被允許的有限輸入，而不是 queue。

## 狀態模型

後續可以把 thread 狀態明確分成：

- `idle`
- `running`
- `broken`
- `archived`

其中 `running` 應該是短期執行態，不一定要持久化到本地檔案，但要能被 Telegram 層與 Web App 觀測層讀到。

## 實作注意點

- 這個 busy gate 應該是 thread-level，而不是 process-level
- 文字訊息與圖片分析必須共用同一套 gate，避免其中一路繞過
- 如果 Codex 執行失敗、超時或程序中斷，busy 狀態必須可靠釋放
- 對 bot-owned selected-session gate，需要額外定義 crash 後的 stale snapshot 回收機制
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
- Telegram 目前是靠 background turn + shared status 達成 reject，不是靠 thread-aware dispatcher 原生保證
- 若未來要把這個語義做成更乾淨的 transport/runtime 邊界，仍應考慮 ingress 層或 dispatcher 分發策略
- forum topic 場景下，仍需明確驗證 `ReplyKeyboardMarkup` 的 user/thread 作用範圍，避免同 chat 其他 topic 受到錯誤影響

## 與其他計劃的關係

- 和 [telegram-webapp-observability.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-webapp-observability.md) 直接相關
  - Web App 若要顯示 thread 即時狀態，需要先有明確的 busy 語義
- 和 [topic-title-status.md](/Volumes/Data/Github/threadBridge/docs/plan/topic-title-status.md) 相關
  - 若之後想把 topic title 當狀態欄，`running` 會是重要訊號
- 和 [message-queue-and-status-delivery.md](/Volumes/Data/Github/threadBridge/docs/plan/message-queue-and-status-delivery.md) 直接相關
  - running 階段的 reply keyboard、移除鍵盤時機、以及相關 status message 屬於 Telegram delivery / control surface，不應直接混進 core gate 內部實作

## 暫定結論

這項應列為後續功能，不在目前版本立即實作。

短期推薦方案是：

- 先加入 thread-level busy gate
- 先採用「拒絕新輸入，不做排隊」
- 等 Web App 觀測面成形後，再決定是否升級成顯式 queue 模型

目前狀態可更新為：

- `v1` 已經達到使用者可感知的 reject 行為
- 但底層實作仍是「shared status + background turn」方案，不是最終的 ingress / state-machine 主規格
- bot process crash 後的 stale busy 回收仍未定義完整，selected-session gate 目前仍可能卡死
- 下一步可以把 Telegram 互動控制面加進這份 plan：
  - `ReplyKeyboardMarkup` 作為主要 control surface
  - `ReplyKeyboardRemove` 作為退出 `running` 的必要收尾
  - `STOP ai 回應` 作為第一個正式 control action
