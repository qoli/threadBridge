# Telegram Markdown 適配草稿

## 目前進度

這份 Plan 已經不是純草稿，`v1` 的核心路徑已部分落地到程式碼。

目前已實作：

- 最終 assistant 回覆已集中到 Telegram 專用入口
  - [`rust/src/telegram_runtime/final_reply.rs`](../../../rust/src/telegram_runtime/final_reply.rs)
- 內部使用 `pulldown-cmark` 解析 Markdown，並輸出 Telegram `HTML parse mode`
- HTML 發送失敗時，會自動 fallback 到純文字
- 超過單則訊息限制時，會改走 notice + `reply.md` attachment
- `reply.md` attachment 已開始在送出前做 Telegram 文件大小 preflight
- 若 `reply.md` attachment 超過 Telegram 文件上限，目前已會回到明確 warning notice，而不是只留下底層 upload failure
- Markdown link 目前統一改寫成 `code` 樣式 label，而不是保留 Telegram link
- 所有主要 bot 文字送信路徑目前都明確關閉 link preview
- preview draft 已接入 `sendMessageDraft + HTML parse mode`
- preview draft 與 final reply 現在共用同一套 Markdown -> Telegram HTML renderer
- Telegram 文字顯示已開始收斂成 line1 符號、line2+ 內容的格式
- 目前會將最後一組 final reply 的 raw markdown 與 intermediate html dump 到：
  - `/Volumes/Data/Github/threadBridge/tmp/final-reply-last.md`
  - `/Volumes/Data/Github/threadBridge/tmp/final-reply-last.html`

目前已明確不做或已回退的部分：

- 不再做自動的 file-bullet 兩行重排與全角空格 continuation indent
  - 先前嘗試過，但對一般清單與 inline code 命中太寬，不夠安全
- 非 final reply surface 仍未完全統一到同一個 rich-text 中間表示

目前尚未完成的項目：

- 是否要建立更明確的中間表示，而不是目前 renderer 內部直接輸出 HTML
- block quote / 更複雜 nested list / 更多 Telegram-specific layout 重建策略
- 是否要把目前保留的 debug dump 收斂成正式診斷工具鏈
- diff artifact 是否應例外地以 URL 形式呈現，而不是和普通 Markdown link 一起被降級
- 若 attachment fallback 遇到 Telegram 文件大小上限，是否應進一步走 artifact URL 或其他更穩定的內容載體

## 問題

`threadBridge` 的最終輸出，目前大多直接把文字送回 Telegram。

但 Telegram 並不是一般的 Markdown 顯示器，它有自己的格式限制與解析規則。這會帶來幾種常見問題：

- 本來在普通 Markdown 裡可讀的內容，到 Telegram 裡格式錯亂
- 程式碼區塊、列表、引用、連結顯示不穩定
- 特殊符號沒有正確 escape，導致訊息發送失敗
- 同一份回覆，在不同 Telegram client 上呈現不一致

所以這個問題本質上不是「要不要支援 Markdown」，而是：

- threadBridge 要不要有一層專門面向 Telegram 的表示適配

## 方向

新增一個 Telegram 表示層，把 assistant 內容從「原始文字」轉成「適合 Telegram 的訊息格式」。

這一層應該負責：

- 控制哪些 Markdown 能保留
- 哪些結構需要降級
- 哪些字元要 escape
- 什麼時候改用純文字而不是格式化訊息

## 目標

### 主要目標

- 降低 Telegram 發送失敗率
- 提高程式碼、列表、引用、連結的穩定顯示品質
- 讓同一份 assistant 回覆在 Telegram 裡更可預測

### 次要目標

- 讓 Codex 不需要知道 Telegram 太多細節
- 把平台差異集中在 bot 端處理
- 為未來 Web App / 多平台輸出保留不同 renderer 的空間

## 建議的心智模型

建議把輸出拆成三層：

- `assistant 原始內容`
  - Codex 最終產生的內容
- `中間表示`
  - 結構化的段落、列表、程式碼區塊、引用、連結
- `Telegram renderer`
  - 轉成 Telegram 可接受的文字與 parse mode

這樣可以避免：

- 讓 Codex 直接為 Telegram Markdown 細節負責
- 在整個程式裡到處手工 escape 字串

## 要處理的顯示元素

### 基本文字

- 普通段落
- 粗體
- 斜體
- 行內 code
- pre/code block

### 結構化內容

- 有序列表
- 無序列表
- block quote
- 小節標題
- 鍵值資訊

### 連結與路徑

- URL
- 本地路徑
- 檔名
- 指令

這裡可以進一步區分：

- 普通內文 link
  - 仍偏向保守處理，不強求保留 Telegram link 呈現
- artifact link
  - 例如 diff viewer URL
  - 這類 link 不應和普通敘述性連結完全混成同一條降級規則

## 適配策略

### 策略 1：保守 Markdown

只保留 Telegram 最穩定的格式：

- 粗體
- 行內 code
- code block
- 簡單列表

其餘內容降級為純文字。

優點：

- 最穩
- 最容易避免 parse error

缺點：

- 表現力有限

### 策略 2：Telegram MarkdownV2 Renderer

以 Telegram MarkdownV2 為標準做完整 escape 與渲染。

優點：

- 格式能力比較強
- 可以保留較多結構

缺點：

- escape 規則很麻煩
- 一旦有漏掉字元，整段訊息就可能送不出去

### 策略 3：HTML Renderer

使用 Telegram 支援的 HTML parse mode，而不是 Markdown。

優點：

- 某些結構比 MarkdownV2 更直觀
- escape 規則在某些情況下更容易控制

缺點：

- 不是所有結構都好表達
- 一樣需要做平台特化處理

## 建議的初版

初版建議採用：

- 內部先建立簡單的中間表示
- Telegram 端先實作一個保守 renderer
- 預設偏向：
  - 純文字段落
  - 行內 code
  - code block
  - 簡單列表
- 遇到不穩定結構時，寧可降級成純文字

換句話說：

- 初期追求穩定
- 不追求 Telegram 裡的完整 Markdown 表現力

以目前程式碼來看，已經做出的實際決策是：

- parse mode 採用 `HTML`，不是 `MarkdownV2`
- final reply 優先穩定送出，再談完整表現力
- fallback 與 attachment 路徑都已經存在
- oversized `reply.md` attachment 已不再直接撞到底層 upload failure
- path / local file references 與普通 URL 不再嘗試保留 Telegram link 呈現

但目前仍缺一個明確規格：

- 若使用者真正需要的是查看 diff，Telegram 是否應優先送出 diff URL，而不是正文或 attachment
- 若 attachment 已回退成 warning notice，下一層 artifact / URL fallback 是否應由 delivery 層而不是 renderer 決定

## Diff URL 想法

目前普通 Markdown link 會被改寫成 `code` 樣式 label。這對一般內容是合理的，因為它能降低 Telegram link 呈現不穩定的問題。

但 diff 比較像一種 artifact，而不是一般段落中的輔助連結。

對 diff 來說，更合理的方向可能是：

- Telegram 端不承擔完整 diff 閱讀面
- 若已有穩定 diff viewer / artifact URL，優先保留 URL 形式
- 讓使用者跳到更適合 diff 的 surface 閱讀

這表示未來 renderer 可能要區分：

- 一般內文 link
- 一般 artifact URL
- diff URL

而不是單純把所有 link 都套用同一條 rewrite 規則。

## 與現有功能的關係

這個適配層不只影響普通 assistant 回覆，也會影響：

- preview draft
- tool 產生的文字說明
- 錯誤訊息
- restore / reconnect / reset 等系統提示
- 未來 Web App 中可能重放的消息內容

所以它不應該只是某一個 helper，而應該是一個比較明確的 renderer 邏輯。

目前已實際影響的範圍只有：

- final assistant reply
- image analysis 的 final assistant reply 路徑

目前尚未納入同一套 renderer 的範圍：

- preview draft
- 一般系統提示
- restore / reconnect / reset 等文案本身的 rich-text 適配
- workspace outbox 產出的文字內容

目前新增確認的一個方向是：

- preview draft 已正式接入 `parse_mode=HTML`
- draft / final 已開始共用同一條 HTML render 路線
- draft-specific 差異只保留在 heartbeat、節流、截斷與 update lifecycle

## Draft 與 Final 共用 HTML 的想法

`sendMessageDraft` 和 final reply 的需求不同。

draft 的重點比較像：

- 快速讓使用者看見 Codex 正在形成中的文本
- 讓 preview 與 final message 的格式語義不要差太遠
- 減少「draft 很粗糙、final 才突然變整齊」的觀感落差

既然 `sendMessageDraft` 也支持 `parse_mode`，目前已採用的新方向不是：

- 另外做一套 draft 專用的 plain-text markdown 清洗

而是：

- 讓 draft 盡量和 final message 共用同一條 HTML render 路線
- 只在 preview surface 另補必要的截斷、節流、heartbeat prefix 等 draft-specific 處理

這表示 draft / final 比較合理的分工可能是：

- 共用：
  - Markdown 解析
  - HTML render
  - link / path / artifact 的表現策略
- draft 專有：
  - heartbeat / animated update
  - 長度截斷
  - preview update 節流
  - 必要時對未完成文本做較保守降級

這樣的好處是：

- preview 與 final message 的格式語義更一致
- renderer 規則不用在 draft / final 之間分叉太多
- 後續若要處理 link、diff URL、artifact URL，也比較不需要維護兩套規則

## 可能的實作位置

比較合理的位置是在 Telegram runtime 層，而不是 Codex runtime 層。

理由：

- Codex 應該產生平台無關內容
- Telegram renderer 是 UI surface 的責任
- 未來如果要支援 Web App、CLI viewer、其他表現形式，也可以共用中間表示

## 風險

- 如果一開始直接追求完整 MarkdownV2，複雜度會很高
- 如果 renderer 太激進，可能改壞原本文字語意
- 如果沒有中間表示，後續會變成大量字串 escape 與 patch
- preview 與 final message 如果使用不同邏輯，會造成觀感不一致
- 如果 draft 直接共用 final HTML renderer，還要確認 `sendMessageDraft` 在動畫更新時的穩定性與限制
- 如果 attachment fallback 沒有再經過 Telegram 文件上限檢查，最終仍可能在 delivery 端失敗
- 如果 diff URL 沒有穩定 host / viewer，只把它當成一般 URL 發出去，實際體驗可能比 attachment 更差

## 開放問題

- 初版應該用 MarkdownV2 還是 HTML？
- preview draft 要不要也使用同一套 renderer？
- `sendMessageDraft` 是否應直接共享 final reply 的 HTML renderer，還是仍保留一層 draft-specific 降級？
- 本地路徑、命令、檔名是否應一律用 monospace？
- 是否要保留 assistant 原始輸出，以便 debug renderer 問題？
- 失敗時是否自動 fallback 到純文字模式？
- attachment fallback 遇到 Telegram 文件大小上限時，應回到純文字、較短 notice，還是其他 artifact 路徑？
- diff URL 應來自哪個正式 surface：management UI、diff viewer，還是其他 artifact host？

其中幾個問題目前已經有程式碼答案：

- `MarkdownV2` 還是 `HTML`
  - 目前已選 `HTML`
- 是否保留 assistant 原始輸出做 debug
  - 目前已保留最後一組 dump 到 `tmp/`
- 是否自動 fallback 到純文字
  - 目前已實作

但文件上限這一題目前還沒有明確答案，應由 Telegram delivery 規格來接：

- [message-queue-and-status-delivery.md](message-queue-and-status-delivery.md)

## 建議的下一步

1. 繼續把更多非 final reply surface 收斂到同一套 renderer / formatter。
2. 重新評估是否真的需要一個顯式中間表示，而不是持續在 renderer 內部直接輸出 HTML。
3. 針對真實 `data/` 樣本持續收斂 Telegram-specific 排版問題，特別是 nested list、長 bullet、block quote。
4. 視需要把仍保留的 final dump 收斂成更正式的診斷面。
5. 把 `reply.md` attachment 與 Telegram 文件大小上限的關係，明確掛回 delivery 規格。
