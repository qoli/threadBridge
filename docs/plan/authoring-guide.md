# Plan 編寫指南

這份文檔用來把 `docs/plan/` 目前已經形成的整理方式，明確寫成可延續的規則。

目標不是把所有 plan 變成僵硬模板，而是讓未來新增想法時，能快速判斷：

- 應該更新既有 plan，還是新增一份新 plan
- 新 plan 應該描述哪一層
- 每份 plan 至少要交代哪些事情
- 要怎麼把它接回 `README.md`、既有主規格與實際代碼

## 這個目錄的角色

`docs/plan/` 目前同時承載三種東西：

- 已落地方案
- 部分落地方案
- 尚未實作的設計草稿

所以這裡不是單純的 backlog，也不是單純的 architecture spec。

它比較接近：

- runtime / product / adapter 的設計筆記
- 已落地行為的補充說明
- 尚未收斂的重構方向

因此新文檔不應只寫「想法」，而應該明確說明：

- 它在解決什麼問題
- 它屬於哪一層
- 它現在成熟到什麼程度
- 它和其他 plan / 代碼的關係是什麼

## 先判斷：更新既有 plan，還是新增新 plan

新增想法前，先做一次去重判斷。

優先更新既有 plan 的情況：

- 只是補充既有 plan 的成熟度、目前進度、已知限制
- 只是把某份 plan 的狀態語義、資料來源、命名補得更完整
- 只是為既有 plan 增加下一步、風險、開放問題
- 新想法本質上仍屬於既有文檔的同一責任邊界

適合新增新 plan 的情況：

- 新想法已經跨出既有文檔的責任範圍
- 它需要自己的問題陳述、術語、資料模型或 API 形狀
- 如果硬塞進舊文檔，會讓該文檔同時描述兩個不同層級的問題
- 它將來很可能成為獨立主規格，或至少是某個子系統的主草稿

簡單判斷法：

- 如果它在回答「這個既有方案還缺什麼」：通常更新原文檔
- 如果它在回答「我們還缺一份新的規格來定義另一層」：通常開新文檔

## Plan 的責任邊界

每份 plan 應盡量只處理一個清楚的責任面，避免把多層語義混在一起。

目前可以沿用的切法：

- `runtime-architecture`
  - current architecture 的 canonical role boundary 與 temporary exception
- `session-lifecycle`
  - thread / workspace / Codex thread 的生命週期與控制操作
- `runtime-state-machine`
  - canonical state axes 與狀態轉移
- `message-queue-and-status-delivery`
  - Telegram outbound delivery lane 與送信語義
- `runtime-protocol`
  - transport-neutral 的 request / event / state view
- `topic-title-status`
  - Telegram topic title 這個 UI surface 承載什麼狀態
- `codex-busy-input-gate`
  - inbound gate 與執行互斥

新增文檔前，先問自己：

- 是在定義 canonical role boundary，還是在補某個子系統細節？
- 這份 plan 是在定義 thread state，還是在定義 UI 呈現？
- 是在定義 lifecycle，還是在定義 delivery？
- 是在定義 runtime core，還是在定義 Telegram adapter？

如果一份文檔同時想回答以上多個問題，通常切分還不夠清楚。

## 檔名規範

沿用目前目錄慣例：

- 檔名使用英文、小寫、kebab-case
- 名稱描述問題域，而不是會議筆記式標題

建議：

- `session-lifecycle.md`
- `runtime-state-machine.md`
- `message-queue-and-status-delivery.md`

避免：

- `new-idea.md`
- `misc-notes.md`
- `telegram-stuff.md`

## 每份 Plan 的標準結構

不要求每份文檔機械式一模一樣，但建議至少包含下面幾段。

### 1. 標題

用一句話描述這份 plan 處理的主題。

例如：

- `# Runtime State Machine 草稿`
- `# Session 生命週期草稿`

### 2. `目前進度`

這是目前最重要的固定段落，建議每份 plan 都保留，而且放在前面。

至少回答：

- 這份文檔是已落地、部分落地，還是純草稿
- 目前代碼裡已經有什麼
- 還沒完成的是什麼

建議格式：

- `這份文檔已部分落地，但仍不是完整主規格。`
- `目前已實作：`
- `目前尚未完成：`

### 3. `問題` 或 `背景`

描述為什麼需要這份文檔。

這一段應避免只有抽象願景，最好明確指出目前的混亂點，例如：

- 哪些語義分散在多份文檔
- 哪些責任邊界不清
- 哪些行為目前只能從代碼猜
- 為什麼現有模型不再適用

### 4. `定位`、`方向` 或 `目標`

這一段負責定義這份文檔要處理什麼，不處理什麼。

常見寫法：

- `這份文件是 ... 的唯一主規格`
- `這份文件只規範 ... v1`
- `明確不處理：...`

如果一份文檔沒有先講清楚自己不解什麼問題，後面通常會開始偷帶其他層的語義。

### 5. 主體規格

這是文檔核心，可以依題目選擇最適合的章節名稱，例如：

- `核心原則`
- `Canonical 狀態軸`
- `建議的協議物件`
- `現有 Telegram Surfaces`
- `Ordering 規則`
- `資料模型`
- `狀態轉移`
- `對實作的影響`

這一段至少要把新概念落到可實作的層級。

如果引入新狀態、事件、artifact 或欄位，盡量一併說清楚：

- 合法值是什麼
- source of truth 是什麼
- 是 persistent 還是 ephemeral
- 哪一層擁有它
- 會在什麼時機轉移或更新

### 6. `與其他計劃的關係`

`docs/plan/` 已經不是單文件世界，所以新 plan 應明確掛回其他文檔。

至少回答：

- 它依賴哪份文檔
- 哪些既有文檔之後應引用它
- 哪些語義不能在這份文件裡重複定義

如果它是主規格，要寫清楚「其他文件應引用它」。

如果它不是主規格，要寫清楚「它引用哪份主規格」。

### 7. `開放問題`

把還沒決定的分歧列出來，不要把模糊決策藏在正文裡。

好的開放問題通常是：

- 預設模式應該是哪一個
- 欄位要不要對外暴露
- 失敗時要採哪種 UX
- 是否需要持久化某個衍生狀態

### 8. `建議的下一步` 或 `暫定結論`

文檔收尾要能回答：讀完之後，接下來做什麼。

適合放：

- 最先落地的 v1 範圍
- 哪個 helper / API / command 應先補
- 哪份文檔接下來要同步更新

## 成熟度標記規則

目前 `README.md` 已經在用這三種成熟度，新增或更新文檔時應繼續沿用：

- `已落地`
- `部分落地`
- `純草稿`

判斷原則：

- `已落地`
  - 核心行為已進入代碼，文檔主要是在記錄已採用方案與邊界
- `部分落地`
  - 已有 v1 或部分能力進入代碼，但文檔中的完整目標尚未全部完成
- `純草稿`
  - 主要仍是設計方向，沒有成為實際行為的穩定 source of truth

若某份文檔是跨多個版本演進的，`目前進度` 段應明說：

- 哪些已落地
- 哪些仍未落地

不要只寫「已完成」或「待實作」而不交代範圍。

## 主規格與從屬文檔

不是每份 plan 都是主規格。

如果一份文檔在定義 canonical naming、唯一 state axes、唯一 delivery semantics，應直接寫明它是主規格，並要求其他文檔引用它。

如果一份文檔只是某個 surface 或子問題，則應避免重複定義主規格已經決定的術語。

例如：

- `runtime-architecture` 應定義 canonical role boundary 與 temporary exception
- `runtime-state-machine` 應定義 canonical state axes
- `message-queue-and-status-delivery` 應定義 outbound lane
- `topic-title-status` 不應再自創另一套 thread 主狀態 enum

若新文檔涉及：

- 哪個模組是 owner
- 哪個 surface 只是 adapter
- 哪些跨層依賴是 temporary exception

應先引用 `runtime-architecture.md`，不要在新文檔內重複發明另一套角色名稱。

## Temporary Exception 規則

若 current code 仍暫時違反主文檔，不應只把理由留在 commit 訊息、PR 描述或零散 plan。

應優先採用：

- 在主文檔列出 `暫時例外`
- 說明現況
- 說明為什麼它不是 canonical architecture
- 說明退出方向

這樣後續遇到回歸時，維護者才不會把 temporary exception 誤當成新的正常模式。

## 術語與命名要求

若某個術語已在既有文檔中形成主線，後續新增 plan 應優先沿用。

目前已經形成的關鍵語言包括：

- `Telegram thread`
- `Workspace binding`
- `Codex thread`
- `binding_status`
- `lifecycle_status`
- `run_status`
- `content / draft / status / edit`
- `source of truth`
- `persistent` / `ephemeral`

不要在新文檔裡平行創造語義接近、但名稱不同的詞，除非這份文檔就是要正式改名，而且有說清楚遷移原因。

### 建議固定詞彙表

下面這組詞應視為 `docs/plan/` 目前的優先術語。

- `Telegram thread`
  - Telegram topic / 討論串 / 使用者正在互動的對話容器
- `Workspace binding`
  - Telegram thread 綁定到的真實 workspace 與其持久化關聯
- `Codex thread`
  - app-server 內的 Codex `thread.id` continuity
- `current_codex_thread_id`
  - 目前這個 Telegram thread 正式採用的 Codex 對話
- `tui_active_codex_thread_id`
  - 受管本地 TUI 最近一次使用或目前活著的 Codex 對話
- `active turn`
  - 某個 Codex thread 當前正在執行的那一輪
- `lifecycle_status`
  - 只描述 Telegram thread 是否 `active` / `archived`
- `binding_status`
  - 只描述 workspace binding 是否 `unbound` / `healthy` / `broken`
- `run_status`
  - 只描述目前是否有 active Codex turn 在跑，`idle` / `running`

如果要描述 scope，建議直接寫：

- `Telegram-scoped`
- `workspace-scoped`
- `Codex-turn scoped`
- `session_id` 視角

避免只寫：

- `thread-level`
- `session-level`

因為這兩個詞在這個 repo 裡很容易混淆 `Telegram thread`、`Codex thread`、`session_id`、以及 `active turn`。

### 舊詞與過渡詞

下列詞不是完全不能出現，但應只在下列情況使用：

- `codex_thread_id`
  - 只在泛指某個 Codex thread id，或描述 legacy / wire 兼容欄位時使用
  - 若要描述目前 Telegram continuity，優先寫 `current_codex_thread_id`
- `selected_session_id`
  - 視為 legacy 欄位名，除非在講兼容讀取，不應再當新術語
- `/bind_workspace`
  - 視為舊命令名或內部/歷史語境
  - 描述目前正式產品流時，優先寫 `/add_workspace` 或 create-bind flow
- `/reconnect_codex`
  - 視為舊 management / 歷史命名
  - 描述目前正式 continuity repair flow 時，優先寫 `/repair_session` 或 `repair_session_binding`
- `/new`
  - 視為舊命令名
  - 描述目前 Telegram 指令時，優先寫 `/new_session`
- `handoff`
  - 若描述現行模型，優先拆成 `adoption`、`mirror`、`readiness`
  - `handoff` 主要保留給歷史文檔或舊模型參照
- `viewer` / `attach` / `.attach`
  - 視為舊 viewer/attach 模型詞，除非文檔明確標示為歷史語境，否則不應再作為現行主術語

### 文檔維護規則

若舊文檔因歷史背景仍保留舊詞，至少應做到其中一種：

- 在 `目前進度` 明確標示它是歷史方案 / retired model / archive 參考
- 在首次出現舊詞時補一句目前對應的新詞
- 避免讓舊命令名、舊欄位名、舊模型詞出現在主規格段落而沒有註解

## 與代碼的關係

`docs/plan/` 不是脫離代碼存在的白板。

新增或更新文檔時，應清楚處理下列問題：

- 目前代碼已經實作了哪些部分
- 文檔裡哪些部分仍然只是願景
- 如果文檔與代碼衝突，以代碼為準，之後再回來修文檔

如果引入新概念，盡量說明它未來會落在哪一層：

- Telegram orchestration
- Codex thread control
- repository / persistent state
- workspace bootstrap
- tool execution
- Telegram adapter

## 交叉引用規則

沿用目前目錄的做法：

- 在 `README.md` 登記這份文檔
- 說明它的成熟度與一句話摘要
- 如有依賴關係，補進 `README.md` 的依賴關係段落
- 文檔內引用其他 plan 時，使用可點擊的 Markdown link

如果新文檔會改變既有主線，至少要同步檢查：

- 是否需要更新 `README.md`
- 是否需要更新對應的主規格文檔
- 是否有其他 plan 應改成「引用它」而不是各自重複定義

## 新想法落檔流程

建議流程：

1. 先搜尋 `docs/plan/`，確認沒有現成文檔已經在處理同一件事。
2. 判斷這個想法屬於哪一層，避免混進不相干的責任面。
3. 決定是更新既有 plan，還是新增一份新 plan。
4. 先寫 `目前進度`，明確區分已落地與未落地部分。
5. 寫清楚 `問題`、`定位`、主體規格、`與其他計劃的關係`、`開放問題`。
6. 更新 [README.md](/Volumes/Data/Github/threadBridge/docs/plan/README.md)，把它放進正確成熟度區段。
7. 如果它是主規格，明確寫出哪些文檔之後應引用它。

## 建議模板

下面是一份建議骨架，可依主題刪減，但不建議省略 `目前進度`。

```md
# <Plan 標題>

## 目前進度

這份文檔目前是純草稿 / 已部分落地 / 已落地。

目前已實作：

- ...

目前尚未完成：

- ...

## 問題

...

## 定位

...

## 核心原則

- ...

## 主體規格

...

## 與其他計劃的關係

- ...

## 開放問題

- ...

## 建議的下一步

1. ...
2. ...
3. ...
```

## 暫定結論

未來新增想法時，目標不是把 `docs/plan/` 變成更多零散筆記，而是維持這個目錄作為：

- 有成熟度標記的設計索引
- 有責任邊界的規格草稿集合
- 能和實際代碼互相校正的工作文檔

新 plan 若做不到這三點，通常代表它還需要先整理，再落進這個目錄。
