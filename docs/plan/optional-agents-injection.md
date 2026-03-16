# 可選 AGENTS 注入草稿

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

## 方向

把 workspace runtime 拆成兩個可以分開控制的部分：

- `工具面`
  - `.threadbridge/bin/`
  - `.threadbridge/tool_requests/`
  - `.threadbridge/tool_results/`
- `指令面`
  - workspace `AGENTS.md` 裡的 threadBridge managed appendix

也就是說，未來應該允許：

- 安裝工具面，但不注入 `AGENTS.md`
- 注入 `AGENTS.md`，但由使用者明確同意
- 完整模式：工具面 + appendix

## 建議的模式

### 模式 A：Full Runtime

- 安裝 `.threadbridge/`
- 注入 `AGENTS.md` appendix

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

## 可能的產品語意

目前 `/bind_workspace` 的語意比較接近：

- 綁定 workspace
- 安裝 runtime
- 注入 appendix

未來可以改成更細：

- `/bind_workspace <path>`
  - 只做基礎 binding
- `/enable_threadbridge_appendix`
  - 明確注入 appendix
- 或 `/bind_workspace <path> --tools-only`
  - 綁定但不注入 appendix

如果不想把命令做太複雜，也可以先從 config 開關開始。

## 對現有架構的影響

### Workspace Bootstrap

現在的 `ensure_workspace_runtime()` 同時做了：

- 建立 `.threadbridge/`
- 寫入或更新 `AGENTS.md` managed block

未來應該拆成兩個 API：

- `ensure_workspace_tools_runtime()`
- `ensure_workspace_agents_appendix()`

讓呼叫端自己決定是否要做第二步。

### Codex Prompt 策略

如果 appendix 注入變成可選，Codex 就不能永遠假設：

- workspace `AGENTS.md` 一定帶有 threadBridge appendix

所以 runtime 需要有 fallback：

- 若 appendix 已啟用，直接依 workspace `AGENTS.md`
- 若 appendix 未啟用，則在每次 turn 的 prompt preamble 裡明確說明 `.threadbridge/` 的使用方式

### Session Binding

`session-binding.json` 可能需要保存一個顯式欄位，例如：

- `agents_appendix_enabled: true/false`

這樣 threadBridge 才知道目前該走哪一套 prompt/runtime 行為。

## 優點

- 減少對使用者 repo 的侵入性
- 更容易接受綁定現有專案
- 更符合「workspace 是使用者自己的」這個邊界
- 讓工具面和指令面解耦

## 代價

- runtime 模型會變得多一個分支
- prompt preamble 與 appendix 模式要同時維護
- 如果沒有 appendix，Codex 使用 `.threadbridge/` 的穩定性可能稍弱

## 開放問題

- 預設模式應該是 Full Runtime，還是 Tools Only？
- appendix 是否應該改成首次綁定時詢問，而不是默認注入？
- 如果使用者手動修改了 `AGENTS.md` managed block，要如何處理？
- 未來是否要支援「先 tools-only，之後再手動開啟 appendix」？

## 建議的下一步

1. 先把 runtime bootstrap API 拆成 tools 與 appendix 兩部分。
2. 決定預設策略是 Full Runtime 還是 Tools Only。
3. 在 `session-binding.json` 裡加入 appendix enablement 狀態。
4. 為「無 appendix 模式」設計 prompt fallback，確保 `.threadbridge/` 仍然可用。
