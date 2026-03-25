# Hcodex 重構前歷史模型草稿

## 目前進度

這份文檔屬於已固定的歷史參考。

它不是新的待實作方案，也不是要把舊模型搬回來，而是用來說清楚：

- `hcodex` 重構前到底怎麼管理本地 `codex` 生命週期
- 為什麼那套做法雖然髒，卻比後來某些過渡版本更不容易留下 stale lifecycle state
- 今天的 clean refactor 應該保留哪個本質，而不是照抄哪些表面實作

## 問題

近期討論 `hcodex` 重構時，最容易出現的誤判是：

- 把舊模型只看成「shell script + Python glue，很亂」
- 卻忽略它其實有一條相對閉合的本地 child lifecycle 鏈

這份文檔要固定的核心結論是：

- 舊 `hcodex` 最大的優點，不是架構乾淨
- 而是它真的把「誰啟動本地 `codex`，誰就一路管到收尾」這件事做成了一條閉環

## 定位

這份文檔處理的是：

- 重構前 `hcodex` 的歷史形狀
- CLI 架構與 app-server ws 架構各自如何承擔本地 lifecycle
- 每個主要時期由誰負責本地 `codex` child lifecycle
- 舊模型的強項與代價

這份文檔不處理：

- 現行 `launch_ticket` / `hcodex-ws-bridge` 的完整 websocket 契約
- today 的 owner/runtime authority 劃分
- clean refactor 的最終模組切法

那些內容分別在其他文檔處理。

## 兩條架構線

在 `hcodex` 的歷史裡，至少要先分清楚兩條不同架構線，否則很容易把「時間先後」和「責任模型」混成一件事。

### 1. CLI 架構

這條線的核心是：

- 本地 shell wrapper
- `codex_sync.py`
- Codex hooks / notify
- `codex-sync/*` 狀態面

在這個模型裡：

- shell 是 launcher
- shell 也是 local process owner
- shell / `codex_sync.py` 一起完成 child tracking 與 cleanup

也就是說，這條線的強項是：

- 本地 `codex` child lifecycle 的責任集中而閉合

### 2. app-server ws 架構

這條線的核心是：

- shared app-server runtime
- `resolve-hcodex-launch`
- `codex --remote`
- local websocket bridge
- 後來的 ingress / observer / adoption 語義

在這個模型裡：

- session transport 不再是本地 CLI hooks 驅動
- `hcodex` 開始面對 remote websocket contract
- lifecycle write-path 逐步從 shell / Python 移到 Rust runtime

這條線的強項是：

- runtime boundary、transport contract、session semantics 更容易被正式化

它的風險則是：

- 如果把 transport 收斂做對了，但把 local process ownership 做薄了，就會比舊 CLI 模型更容易留下 stale lifecycle state

## 歷史分期

下面的時間分期要放在上面這個架構對照下閱讀。

### 1. CLI 架構的 shell + `codex_sync.py` 閉環時期

代表提交：

- `d12a85d` `feat(workspace): 實作本地 Codex CLI 與 Telegram 狀態同步機制`
- `d93d9f0` `feat(workspace): 新增 codex 啟動包裝腳本並重構子程序管理邏輯`
- `501763d` `feat(telegram_runtime): 新增子行程追蹤與終止邏輯`

這個時期的核心形狀是：

- workspace appendix 直接注入一個 shell `hcodex()` function
- shell `hcodex()` 先跑 `prepare-launch`
- shell 自己持有 `THREADBRIDGE_CODEX_SHELL_PID`
- shell 用 `codex_launch` 包一層真正的 `codex` 啟動
- `codex_launch` 在 `exec` 前把 child pid / child pgid / child command 記下來
- `codex` 結束後，shell 再發 `shell_process_exited`

這代表本地 lifecycle 的 write-side 主體其實很明確：

- shell wrapper 就是 launcher
- shell wrapper 也是 process owner
- shell wrapper 同時也是 cleanup 的最後收口點

### 2. CLI 架構下的 child tracking 補強期

`501763d` 之後，舊模型雖然依舊髒，但 lifecycle 閉環更完整了。

這時已經存在：

- `prepare-launch`
  - 先建立 owner claim
  - 先拒絕 workspace 內的並行 live CLI session
- `record-child-process`
  - 把 child pid / pgid / command 寫回 owner claim 與 session status
- `shell_process_started`
  - 將 shell pid 標成 active
- `shell_process_exited`
  - 將對應 shell pid 的 CLI session 收成 `idle`
  - 清掉 owner claim
- `record-exit-diagnostic`
  - 在 137 / 143 這種 signal exit 形狀下補記診斷資料

這一層的關鍵不是資料格式，而是責任順序：

1. 啟動前先 claim ownership
2. 啟動時立即抓 child process identity
3. 結束時由同一條 shell call stack 發出 exit event
4. owner claim 與 session status 由同一套管理腳本收尾

所以當年的強項不是「狀態很乾淨」，而是：

- 啟動與收尾責任沒有被拆散

### 3. CLI 架構延伸到 managed `hcodex` / handoff 時期

代表提交：

- `ca9ea28` `feat(threadbridge): add managed hcodex mirror handoff`
- `1e0a7b0` `feat(threadbridge): add exclusive cli handoff attach`
- `9f71c2e` `feat(threadbridge): add tui proxy adoption flow`

這個時期開始把 `hcodex` 從單純 CLI sync，往 managed local TUI 入口推進。

但即使加入：

- handoff
- attach intent
- 後來的 adoption / local mirror 語義

本地 child lifecycle 依然主要被 shell 鏈拿著。

也就是說，舊模型雖然開始混入更多 session/handoff 語義，但它沒有失去一個關鍵特性：

- 真正啟動 `codex` 的本地 launcher，仍然是那個會在最後發 `shell_process_exited` 的 shell

### 4. app-server ws 架構進場，但 shell 仍是本地 process owner

代表提交：

- `9f60e40` `feat(threadbridge): add shared app-server runtime foundation`
- `996fe0e` `refactor(threadbridge): replace python hcodex bridge`

這個時期 transport 與 runtime 形狀已經大變：

- `hcodex` 會先 resolve 共享 runtime / daemon 入口
- 必要時會起本地 websocket bridge
- 最後再把 `--remote` 丟給 `codex`

這裡最重要的區分是：

- 架構上，系統已經從 CLI hooks / notify 主模型，轉向 app-server ws 主模型
- 但本地 process lifecycle 的最後責任者，暫時還沒有跟著一起切走

但即使那時 launch contract 已經開始變複雜，shell 仍維持一條很硬的事實：

- 它最後是直接 `exec "$codex_bin" --remote "$remote_ws_url"`

這表示在 `88d4bb1` 之前，哪怕 websocket 路徑已經變了，本地 `codex` child lifecycle 的最後責任者仍是 shell 本身。

這也是舊模型穩的地方：

- transport 可以變
- handoff 語義可以變
- 但本地 process owner 沒有變薄

### 5. app-server ws 架構下，Rust launcher 取代直接 `exec`

代表提交：

- `88d4bb1` `fix(threadbridge): stabilize hcodex mirror lifecycle`

這次是歷史上的真正轉折點。

從這次開始：

- shell wrapper 不再直接 `exec codex --remote ...`
- shell 改成呼叫 `run-hcodex-session`
- Rust `run-hcodex-session` 改用 `spawn + wait`
- `workspace_status` 開始依賴：
  - `record_hcodex_launcher_started`
  - `record_hcodex_launcher_ended`

這一步的收益很明確：

- lifecycle write-path 從 shell / Python / event file，開始往 Rust runtime 收斂
- `hcodex` 的責任邊界更容易被正式化

但風險也在這一步出現：

- 原本由 shell call stack 天然持有的 local process ownership，開始被拆成多段
- 若 `run-hcodex-session` 只覆蓋 happy path cleanup，就會比舊模型更容易留下 stale state

換句話說：

- 舊 CLI 模型的問題是髒
- 新 app-server ws 模型在某些版本的問題則是 lifecycle contract 變薄

## 這份歷史真正要比較的不是「新舊」，而是「哪條架構線拿著本地 lifecycle」

如果只按時間看，很容易誤會成：

- CLI 時期 = 舊
- app-server ws 時期 = 新
- 所以只要往新架構走，責任自然就會更好

但這段歷史真正說明的是：

- CLI 架構強在 local lifecycle ownership
- app-server ws 架構強在 transport / session contract
- clean refactor 應該是把後者的邊界清晰度，和前者的強生命周期閉環合在一起

而不是二選一。

## 舊模型為什麼雖髒但穩

把舊代碼抽象後，它真正提供了四個 today 仍然需要的特性。

### 1. 單一的本地 process owner

- 誰啟動 `codex`
- 誰持有 shell pid
- 誰知道 child pid / pgid
- 誰在最後發 exit event

基本上是同一條本地 launcher 鏈。

### 2. 啟動與收尾在同一條 call path

- `prepare-launch`
- `record-child-process`
- `shell_process_exited`

這三段雖然分散在 shell 與 Python，但仍然由同一條本地啟動路徑串起來，而不是交給不同 runtime 邊界「盡量補上」。

### 3. 並行 session 會在啟動前被拒絕

舊 `prepare-launch` 很早就會拒絕：

- 已有 live CLI session
- 既有 owner claim 不屬於目前 shell pid

這讓很多 lifecycle 衝突在 spawn 前就被擋下。

### 4. signal exit 也有最低限度的診斷/收尾路徑

它未必優雅，但至少不是完全失語：

- exit 137 / 143 會記錄 `shell_exit_diagnostic`
- child pid / pgid / command 已經在前面先落盤

所以就算最後仍出錯，舊模型通常還留得下足夠的狀態線索。

## 舊模型的代價

這套做法今天不能原樣搬回來，原因也很清楚。

- shell、Python、workspace state、attach intent、viewer handoff 混在一起
- owner、CLI、handoff、mirror、adoption 這些語義長期糾纏
- 很多契約其實靠隱性順序維持，不是正式型別或模組邊界
- 可測性與可維護性都不好

所以 today 的方向依然應該是：

- 不回到舊實作
- 但保留它的本質優點

## 對今天重構的約束

從這段歷史可以提煉出一條真正該保留的約束：

- `hcodex` 可以不再是 shell script
- 可以不再用 Python `codex_sync.py`
- 也可以不再沿用 CLI handoff vocabulary
- 但它不能失去「本地 `codex --remote` child 的強生命週期管理者」這個角色

也就是說，今天要保留的是 CLI 架構的本地 lifecycle 閉環能力，而不是 CLI hooks / shell / `codex_sync.py` 這些舊技術面。

換成 today 的語言就是：

- `hcodex` 不必是 runtime owner
- 但它必須是 local Codex process owner

如果未來 clean refactor 把 `hcodex` 做成只剩 launch adapter，而把 spawn / signal forwarding / teardown / final reconciliation 分散回不同層，那就等於把舊模型唯一真正值得保留的東西刪掉了。

## 與其他計劃的關係

- [hcodex-lifecycle-supervision.md](/Volumes/Data/Github/threadBridge/docs/plan/hcodex-lifecycle-supervision.md)
  - 定義 today 的正式 supervision 約束
- [hcodex-launch-contract.md](/Volumes/Data/Github/threadBridge/docs/plan/hcodex-launch-contract.md)
  - 定義 today 的 launch / bridge websocket 契約
- [post-cli-runtime-cleanup.md](/Volumes/Data/Github/threadBridge/docs/plan/post-cli-runtime-cleanup.md)
  - 處理 CLI 時代命名、狀態面與 compatibility 收尾
- [owner-runtime-contract.md](/Volumes/Data/Github/threadBridge/docs/plan/owner-runtime-contract.md)
  - 固定 `desktop runtime owner` 與 `hcodex` 的不同 ownership 層次

## 建議的下一步

1. 後續討論 `hcodex` clean refactor 時，先把這份歷史文檔當成約束背景，而不是只看當前代碼外觀。
2. 每個新 patch 都要回答同一個問題：
   - 這一步是否仍保留「誰啟動本地 Codex，誰就負責一路收尾」？
3. 若答案是否定的，應視為高風險重構，而不是普通模組整理。
