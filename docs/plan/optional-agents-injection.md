# 可選 AGENTS 注入草稿

## 目前進度

這份文檔目前仍是草稿，尚未開始實作。

目前實際行為仍然是：

- `/add_workspace` / 等價 create-bind flow 會安裝 `.threadbridge/`
- `/add_workspace` / 等價 create-bind flow 會同步更新 workspace `AGENTS.md` 的 managed appendix

也就是說，目前還沒有 `tools only` 或 `no injection` 模式。

## 問題

目前 `threadBridge` 在綁定 workspace 時，會把 managed appendix 寫進目標 workspace 的 `AGENTS.md`。

這個做法有明顯優點：

- Codex 在 workspace 內工作時，能自然讀到 threadBridge runtime appendix
- `.threadbridge/bin/*`、tool request/result 路徑可以被明確說明
- bot 可以比較穩定地把 workspace 視為可執行 runtime

但它也有明顯代價：

- 會修改使用者自己的 repo
- 對某些專案來說，`AGENTS.md` 是使用者自己管理的，不應被自動追加內容
- 有些 workspace 可能根本不想讓 threadBridge 接管 prompt/runtime 行為
- 同一個 workspace 可能只想要工具面，不想要 instruction appendix

所以未來更合理的方向應該是：

- `AGENTS.md` appendix 注入不是強制行為
- 而是可選能力
- 而且不應只有「直接把整段 appendix 寫進 root `AGENTS.md`」這一種形式

## 方向

把 workspace runtime 拆成兩個可以分開控制的部分：

- `工具面`
  - `.threadbridge/bin/`
  - `.threadbridge/tool_requests/`
  - `.threadbridge/tool_results/`
- `指令面`
  - workspace `AGENTS.md` 裡的 threadBridge runtime 指令
  - 或一份由 `AGENTS.md` 轉介的附加子文檔

也就是說，未來應該允許：

- 安裝工具面，但不注入 `AGENTS.md`
- 注入 `AGENTS.md`，但由使用者明確同意
- 以 root `AGENTS.md` 內嵌 appendix 的形式注入
- 以附加子文檔的形式注入
- 完整模式不再只是一種實作，而是一組可選注入策略

## 建議的模式

### 模式 A：Full Runtime

- 安裝 `.threadbridge/`
- 在 root `AGENTS.md` 直接注入 managed appendix

適合：

- 專門拿來給 threadBridge 使用的 workspace
- 希望 Codex 在 workspace 內自然讀到 runtime 規則的情境

### 模式 B：Tools Only

- 安裝 `.threadbridge/`
- 不改 `AGENTS.md`

適合：

- 使用者想保留自己的 `AGENTS.md`
- 只想要 wrapper / tool surface
- 不希望 bot 自動修改 repo 內文件

### 模式 C：No Injection / External Instructions

- workspace 不注入 appendix
- threadBridge 在每次 turn 透過 prompt 或其他顯式方式告訴 Codex 如何使用 `.threadbridge/`

適合：

- 極度保守、不允許 bot 改 repo 文件的情境
- 之後如果要支援更細緻的 per-thread runtime 指令，也比較容易擴展

### 模式 D：Child Document Injection

- 安裝 `.threadbridge/`
- 建立一份 threadBridge 管理的附加子文檔，例如 `AGENTS.threadbridge.md`
- root `AGENTS.md` 只注入一小段穩定轉介，明確要求同時遵循該子文檔

適合：

- 使用者接受極小幅修改 root `AGENTS.md`，但不想把整段 appendix 直接混進主文檔
- 希望把 threadBridge runtime 規則與專案自己的 `AGENTS.md` 分開維護
- 想保留「Codex 能自然讀到 runtime 指令」這個優點，但降低 merge noise 與 ownership 混淆

## Child Document 模式的語意

這個模式的關鍵不是「完全不碰 `AGENTS.md`」，而是：

- root `AGENTS.md` 只負責宣告有一份附加子文檔需要一併遵循
- threadBridge runtime 的具體內容放在獨立文件裡
- threadBridge 只全權管理那份子文檔與極小的轉介段落，不把大量內容混進使用者主文檔

一個可能的形態是：

- `AGENTS.md`
  - 保留使用者原始內容
  - 追加一段很短的 managed 轉介段落
- `AGENTS.threadbridge.md`
  - 放完整 runtime appendix 內容

這樣做的好處是：

- root `AGENTS.md` 的 diff 更小
- appendix 更新只會落在 threadBridge 自己的子文檔
- 使用者比較容易理解哪些內容是專案自有規則，哪些內容是 runtime 附加規則

## 可能的產品語意

目前 `/add_workspace` / create-bind flow 的語意比較接近：

- 綁定 workspace
- 安裝 runtime
- 注入 appendix

未來可以改成更細：

- `/add_workspace <path>`
  - 只做基礎 binding
- `/enable_threadbridge_appendix`
  - 明確啟用 inline appendix
- `/enable_threadbridge_child_agents`
  - 建立子文檔並在 root `AGENTS.md` 注入轉介
- 或 `/add_workspace <path> --tools-only`
  - 綁定但不注入 appendix
- 或 `/add_workspace <path> --agents-mode=inline|child|external|tools-only`
  - 明確選擇指令面策略

如果不想把命令做太複雜，也可以先從 config 開關開始。

## 對現有架構的影響

### Workspace Bootstrap

現在的 `ensure_workspace_runtime()` 同時做了：

- 建立 `.threadbridge/`
- 寫入或更新 `AGENTS.md` managed block

未來應該拆成兩個 API：

- `ensure_workspace_tools_runtime()`
- `ensure_workspace_agents_instructions(mode)`

其中 `mode` 至少應涵蓋：

- `inline_appendix`
- `child_document`
- `external_instructions`
- `tools_only`

如果採用 `child_document`，bootstrap 還需要再拆出兩個穩定責任：

- `ensure_workspace_agents_referral_block()`
  - 在 root `AGENTS.md` 建立或更新短轉介段落
- `ensure_workspace_agents_child_document()`
  - 寫入或更新 `AGENTS.threadbridge.md`

### Codex Prompt 策略

如果 appendix 注入變成可選，Codex 就不能永遠假設：

- workspace `AGENTS.md` 一定直接內嵌 threadBridge appendix

所以 runtime 需要有 fallback：

- 若是 `inline_appendix`，直接依 workspace `AGENTS.md`
- 若是 `child_document`，也依 workspace `AGENTS.md`，但它需要穩定轉介到附加子文檔
- 若是 `external_instructions`，則在每次 turn 的 prompt preamble 裡明確說明 `.threadbridge/` 的使用方式
- 若是 `tools_only`，則只有工具面，沒有額外指令面保證

### Session Binding

`session-binding.json` 不應只保存布林值，而應該保存顯式模式，例如：

- `agents_injection_mode: "inline_appendix" | "child_document" | "external_instructions" | "tools_only"`

這樣 threadBridge 才知道目前該走哪一套 prompt/runtime 行為，也能在 UI 或 `/status` 裡正確展示目前 workspace 的接入方式。

## 優點

- 減少對使用者 repo 的侵入性
- 更容易接受綁定現有專案
- 更符合「workspace 是使用者自己的」這個邊界
- 讓工具面和指令面解耦
- `child_document` 模式還能降低 root `AGENTS.md` 的維護噪音

## 代價

- runtime 模型會變得多一個分支
- prompt preamble、inline appendix、child document 模式要同時維護
- 如果沒有 appendix，Codex 使用 `.threadbridge/` 的穩定性可能稍弱
- 需要決定 child document 的固定命名、相對路徑與轉介文案格式

## 開放問題

- 預設模式應該是 Full Runtime，還是 Tools Only？
- appendix 是否應該改成首次綁定時詢問，而不是默認注入？
- 如果使用者手動修改了 `AGENTS.md` managed block，要如何處理？
- 未來是否要支援「先 tools-only，之後再手動開啟 appendix」？
- `child_document` 的檔名應該是 `AGENTS.threadbridge.md`、`.threadbridge/AGENTS.md`，還是別的固定位置？
- root `AGENTS.md` 的轉介段落要做到多短，才既可靠又不惹人嫌？
- 如果專案本身已經有多份子 AGENTS 文件，threadBridge 要不要遵循既有命名慣例？

## 建議的下一步

1. 先把 runtime bootstrap API 拆成 tools 與 appendix 兩部分。
2. 把 appendix enablement 改成明確的 `agents_injection_mode`，不要只用布林值。
3. 為 `child_document` 模式定義固定檔名、轉介段落格式與更新策略。
4. 決定預設策略是 `inline_appendix`、`child_document`、還是 `tools_only`。
5. 為「非 inline 模式」設計 prompt fallback，確保 `.threadbridge/` 仍然可用。
