# Codex 執行中阻止新輸入

## 目前進度

這份 Plan 的 `v1` 已經部分落地，不再是純草稿。

目前已實作：

- Telegram 文字訊息 busy gate
- 圖片保存後延後分析的 busy gate
- `/new`、`/reconnect_codex`、已綁定 thread 的 `/bind_workspace` 受 busy 狀態保護
- busy 狀態已經不只看 bot 本身，也會讀 workspace shared status

目前尚未實作：

- 顯式 queue 模型
- 更完整的 `runtime-state-machine` 對齊
- Web App 觀測面上的正式狀態展示

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
- 讓後續觀測面可以正確顯示 thread 的 busy 狀態

## 建議方向

第一階段不做排隊，先做硬性阻止。

建議語義：

- 同一 thread 同一時間只允許一個 active Codex turn
- 如果 thread 正在執行中，新進文字訊息直接回覆 busy 提示，不寫入 Codex turn
- 如果 thread 正在執行中，新進圖片只允許保存為待處理素材，不能立即啟動分析
- `/new`、`/reconnect_codex`、`/bind_workspace` 這類命令也應該定義是否受 busy 狀態保護

## 提示文案方向

建議先使用明確、低歧義文案：

- `Codex 仍在處理上一個請求，請等待目前回合完成後再發送新訊息。`
- 如果是圖片分析按鈕或圖片輸入，可以補充：
  - `圖片已保存，但目前不會立即分析。`

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
- 預覽訊息更新期間不能讓第二個 turn 把第一個 turn 的 draft 狀態污染掉
- log 需要能看出一次輸入是被拒絕、延後，還是成功進入執行

## 與其他計劃的關係

- 和 [telegram-webapp-observability.md](/Volumes/Data/Github/threadBridge/docs/plan/telegram-webapp-observability.md) 直接相關
  - Web App 若要顯示 thread 即時狀態，需要先有明確的 busy 語義
- 和 [topic-title-status.md](/Volumes/Data/Github/threadBridge/docs/plan/topic-title-status.md) 相關
  - 若之後想把 topic title 當狀態欄，`running` 會是重要訊號

## 暫定結論

這項應列為後續功能，不在目前版本立即實作。

短期推薦方案是：

- 先加入 thread-level busy gate
- 先採用「拒絕新輸入，不做排隊」
- 等 Web App 觀測面成形後，再決定是否升級成顯式 queue 模型
