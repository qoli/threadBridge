# Topic Title 狀態欄草稿

## 目前進度

這份 Plan 已部分落地。

目前已實作：

- title 基底優先使用 thread title
- 若 thread title 缺失，回退到 workspace basename
- suffix 目前支持 `· cli`、`· bot`、`· broken`
- background watcher 會在共享 workspace status 變化時更新 title

目前尚未實作：

- context ratio / ctx%
- 更細緻的 title 節流與觀測規格
- 更完整地對齊 `runtime-state-machine`

## 問題

現在的 Telegram topic title 比較像單純的人類可讀名稱，沒有承載太多 runtime 狀態。

但對 `threadBridge` 來說，topic title 其實是 Telegram 裡少數一直可見的 UI 面。這表示它可以拿來表達一些非常精簡的狀態，而不必每次都依賴額外的 status message 或 slash command。

目前最值得放進 title 的資訊有兩種：

- 綁定的 workspace path，或它的簡短表示
- 目前 context window 的使用比例

## 方向

把 Telegram topic title 當成一個很輕量的 runtime 狀態欄。

它應該仍然以人類可讀為優先，但可以承載少量、穩定、容易掃描的執行狀態。

## 可以表達的資訊

### Workspace

可能的表示方式：

- 只顯示 repo 名稱
  - 例：`threadBridge`
- 顯示短路徑尾巴
  - 例：`Github/threadBridge`
- 帶前綴
  - 例：`ws:threadBridge`

建議：

- 預設只用最後一段路徑
- 只有名稱衝突時，才顯示更多 path context

### Context Window 比例

可能的表示方式：

- 百分比
  - 例：`42%`
- 簡短比例
  - 例：`ctx 4/10`
- badge 風格
  - 例：`[ctx42]`

建議：

- 用簡短數字，不用視覺條
- 儘量固定寬度，方便掃描

## 可能的 title 格式

### 格式 A

`threadBridge · 42%`

優點：

- 簡單
- 可讀性高
- 干擾低

缺點：

- 百分比的語意不夠明確

### 格式 B

`threadBridge · ctx42%`

優點：

- 清楚指出這是 context ratio

缺點：

- 比較吵

### 格式 C

`threadBridge · 42% · active`

優點：

- 可以順便表達 binding 健康狀態

缺點：

- 很容易變太長

### 格式 D

`tb · ctx42`

優點：

- 很短

缺點：

- 太像內部縮寫，不適合作為預設

## 建議的初版

建議先從下面這種格式開始：

`<workspace-label> · ctx<percent>%`

例子：

- `threadBridge · ctx18%`
- `eisonAI · ctx63%`
- `artBot · ctx91%`

如果目前 thread 尚未綁定：

- `Unbound · ctx0%`

如果目前 binding 已損壞：

- `threadBridge · broken`
- 或 `threadBridge · ctx63% · broken`

## 什麼時候更新 title

title 不應該在每一個小事件都更新，而應該只在有意義的狀態變化時更新：

- `/bind_workspace` 之後
- `/new` 之後
- `/reconnect_codex` 之後
- 一次完整 Codex turn 完成後，而且 context ratio 有明顯變化時
- archive / restore 狀態切換時

## 資料來源需求

如果要把 context ratio 放進 title，threadBridge 需要一個穩定的估算來源。

可能來源：

- Codex app-server 是否有提供 token usage 或 compaction 狀態
- 由本地依 turn 歷史長度估算
- 未來在 `session-binding.json` 或 metadata 裡保存一個衍生指標

這一點可以後面再定，但 title 格式和更新時機可以先確立。

## 風險

- repo 名稱太長時，title 可能不好讀
- 更新太頻繁會讓使用者覺得吵
- 如果 context ratio 只是近似值，使用者可能會過度相信它
- title 不應該被塞進太多 runtime flag

## 開放問題

- workspace label 應該完全自動生成，還是允許一部分使用者自定？
- broken / unbound 狀態要不要完全取代 context ratio 顯示？
- archived thread 要不要保留最後一個 runtime title，還是切成 restore 導向的 title？
- title 的標籤應該中文化、英文化，還是盡量保持符號化與語言中立？

## 建議的下一步

1. 先決定唯一的 title 格式。
2. 定義 context ratio 的資料來源。
3. 在 Rust 裡加入 title rendering helper，讓 title 更新可預測。
4. 只在 binding 狀態變更與 turn 完成時更新，不要逐事件更新。
