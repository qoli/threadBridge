# Telegram HTML Rendering

這份文檔描述 `threadBridge` 目前已實作的 `Codex output markdown -> Telegram HTML` 渲染流程。

對應程式碼主要在：

- [rust/src/telegram_runtime/final_reply.rs](/Volumes/Data/Github/threadBridge/rust/src/telegram_runtime/final_reply.rs)
- [rust/src/telegram_runtime/thread_flow.rs](/Volumes/Data/Github/threadBridge/rust/src/telegram_runtime/thread_flow.rs)
- [rust/src/telegram_runtime/media.rs](/Volumes/Data/Github/threadBridge/rust/src/telegram_runtime/media.rs)

## 套用範圍

目前只有「final assistant reply」會走這套 renderer。

包含兩條出口：

- 一般 thread 文字對話的最終回覆
- image analysis 的最終分析回覆

目前不包含：

- preview draft
- restore / reconnect / reset 等系統訊息
- workspace runtime outbox 的 text / caption

## 整體流程

最終回覆的流程是：

1. 取得 assistant 的最終原始文字。
2. 呼叫 `plan_final_assistant_reply(raw_text, 4096)`。
3. 先把原始文字當成 Markdown 解析。
4. 將可支援的結構轉成 Telegram `HTML parse mode` 可接受的字串。
5. 依結果選擇三種送法之一：
   - inline HTML
   - inline plain text
   - markdown attachment

實際送出時由 `send_final_assistant_reply(...)` 負責。

## Markdown 解析方式

目前使用 `pulldown-cmark`，且開啟 `Options::all()`：

- 輸入被當成完整 Markdown 解析
- renderer 不是靠 regex patch 字串
- 會先取得 Markdown event stream，再逐項轉成 Telegram HTML

這代表目前的行為是：

- 上游可以輸出普通文字，也可以輸出 Markdown
- renderer 會盡量保留一部分常見 Markdown 結構
- 但最終能力以 Telegram HTML 可穩定送出的範圍為準

## 支援的渲染規則

### 段落

- 一般文字會原樣保留
- 段落之間會插入空行

### 標題

- Markdown heading 會降級成 `<b>...</b>`
- 不保留 heading level，例如 `#`、`##` 不會有不同樣式

例子：

```md
# Title
```

會變成：

```html
<b>Title</b>
```

### 強調樣式

- `*italic*` / `_italic_` -> `<i>...</i>`
- `**bold**` / `__bold__` -> `<b>...</b>`
- `~~strike~~` -> `<s>...</s>`

### 行內 code

- `` `code` `` -> `<code>...</code>`

### code block

- fenced code block 與 indented code block 都會轉成 `<pre><code>...</code></pre>`
- 若 fenced code block 有 language label，例如 ````` ```rust `````，目前會把語言名稱直接插進 code block 第一行

例子：

```md
```rust
fn main() {}
```
```

會變成近似：

```html
<pre><code>rust
fn main() {}</code></pre>
```

注意：目前這不是 Telegram 的語法高亮設定，只是把 `rust` 文字放進第一行。

### 列表

- unordered list 會輸出成 `- item`
- ordered list 會輸出成 `1. item`
- nested list 目前用前置空白縮排

例子：

```md
- one
- two
```

會變成近似：

```html
- one
- two
```

這裡沒有用 `<ul>` / `<li>`，而是轉成可讀的純文字列表，再放在 Telegram HTML 訊息裡。

### 連結

- Markdown link 會轉成 `<a href="...">label</a>`

例子：

```md
[OpenAI](https://openai.com)
```

會變成：

```html
<a href="https://openai.com">OpenAI</a>
```

### 圖片

- Markdown image 不會送成 Telegram 圖片
- 目前只會輸出成純文字：`Image: <url>`

## HTML escape 規則

所有普通文字內容都會先做 HTML escape，再插入 Telegram HTML。

目前會 escape：

- `&` -> `&amp;`
- `<` -> `&lt;`
- `>` -> `&gt;`
- `"` -> `&quot;`

所以 assistant 原文中的 raw HTML 不會被直接信任或直接透傳。

例子：

```md
<script>alert(1)</script>
```

會變成：

```html
&lt;script&gt;alert(1)&lt;/script&gt;
```

## 不支援結構的處理方式

以下結構目前被視為 unsupported：

- blockquote
- footnote definition
- table
- table head / row / cell

一旦進入 unsupported block：

1. renderer 會暫停正常 HTML 輸出。
2. 收集該 subtree 內的文字內容。
3. 結束時把整塊包成 `<pre><code>...</code></pre>`。

例子：

```md
> nested
> quote
```

會變成近似：

```html
<pre><code>nested
quote</code></pre>
```

注意幾點：

- 目前 blockquote 的 `>` 標記本身不會保留
- unsupported subtree 主要保留內容，不保留原結構外觀
- 這是一種保守 fallback，目標是內容可送、可讀

## Task list / Rule / Footnote reference

這幾種 event 目前有最低限度處理：

- task list marker:
  - checked -> `[x] `
  - unchecked -> `[ ] `
- horizontal rule -> `----`
- footnote reference -> `[label]`

但它們的整體視覺表達仍然很接近純文字，不是完整 Telegram rich rendering。

## 三種最終送信模式

`plan_final_assistant_reply(...)` 目前會產生三種結果。

### 1. `InlineHtml`

條件：

- 原始文字非空
- render 後 HTML 非空
- render 後 HTML 長度不超過 `4096` 字元

行為：

- 用 `bot.send_message(...).parse_mode(ParseMode::Html)` 發送

### 2. `InlinePlainText`

條件：

- 原始文字為空白，或
- render 後 HTML 為空

行為：

- 直接發純文字，不帶 `parse_mode`

目前內部 reason 有：

- `empty_reply`
- `html_render_empty`

### 3. `MarkdownAttachment`

條件：

- render 後 HTML 超過 `4096` 字元

行為：

1. 先送一則純文字通知：
   - `Reply too long for inline Telegram delivery. Full response attached.`
2. 內文附一段 preview snippet
3. 將原始 assistant 輸出寫成 `.md` 檔
4. 以 Telegram document 方式送出

目前附件行為：

- 寫入位置：`data/<thread-key>/state/telegram/overflow-reply-<timestamp>.md`
- Telegram 顯示檔名固定為：`reply.md`
- 附件送成功後會嘗試刪掉暫存檔

## 送信失敗 fallback

如果原本走 `InlineHtml`，但 Telegram 拒收該 HTML 訊息：

1. 會記一筆 warning log
2. 立刻改用原始 assistant 文字，以 plain text 重送一次

這代表目前的優先順序是：

- 先嘗試保留格式
- 一旦 HTML 不穩，就優先保證能送達

## 目前與 preview 的差異

這套 renderer 不會用在 preview。

preview 目前仍然是：

- 根據 event stream 生成純文字草稿
- 經由 `sendMessageDraft` 發送
- 不帶 Telegram HTML parse mode

所以目前「preview 長相」和「最終訊息長相」可能不一致，這是現行設計的一部分。

## 目前已知限制

這份文檔只描述現況，不代表理想狀態。就目前實作來看，主要限制有：

- blockquote、table 等結構只會 fallback 成 code block
- heading level 不保留
- list 是文字型列表，不是真正 HTML list
- fenced code block 的語言名稱只是普通第一行文字
- preview 不會反映 final HTML renderer 的實際效果
- 長訊息目前不做多則分段，而是直接改走附件

## 相關測試

目前已有幾個基本測試覆蓋：

- 支援結構會轉成 HTML
- raw HTML 會被 escape
- unsupported block 會 fallback 成 code block
- 過長內容會改走 markdown attachment
- preview snippet 會取第一個非空段落
- 空回覆會退成 plain text

對應測試在：

- [rust/src/telegram_runtime/final_reply.rs](/Volumes/Data/Github/threadBridge/rust/src/telegram_runtime/final_reply.rs)
