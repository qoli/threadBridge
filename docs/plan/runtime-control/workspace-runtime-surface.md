# Workspace Runtime Surface 草稿

## 目前進度

這份文檔已進入「部分落地」。

目前代碼裡已經存在的能力：

- threadBridge 會把 managed runtime appendix 寫入真實 workspace 的 `AGENTS.md`
- threadBridge 會在真實 workspace 下建立 `./.threadbridge/`
- 目前已固定安裝的 wrapper / runtime surface 包括：
  - `./.threadbridge/bin/build_prompt_config`
  - `./.threadbridge/bin/generate_image`
  - `./.threadbridge/bin/hcodex`
  - `./.threadbridge/bin/send_telegram_media`
  - `./.threadbridge/state/workspace-config.json`
  - `./.threadbridge/state/app-server/*`
  - `./.threadbridge/state/runtime-observer/*`
  - `./.threadbridge/tool_requests/*`
  - `./.threadbridge/tool_results/*`
- workspace bootstrap 與 surface materialization 已由 `rust/src/workspace.rs` 負責
- appendix wording 與 surface 使用方式已由 `runtime_assets/templates/AGENTS.md` 描述
- Phase 1 已新增一個明確定位：
  - `./.threadbridge/state/runtime-observer/*` 是 workspace-local observation / activity surface
  - desktop owner heartbeat 才是 managed runtime health 的 canonical authority

目前尚未完成：

- `workspace runtime surface` 的正式主規格文檔
- 哪些 tool 應該對所有 workspace 固定可用，哪些應按 profile / project type 選擇啟用
- workspace surface capability 的顯式資料模型
- surface profile 與 management API / runtime protocol 的對齊
- 非必要 tool 不再預設灑進所有 workspace 的收斂策略

## 問題

目前 `.threadbridge/` 已經不只是幾個 shell script。

它其實已經形成一個真實的 `workspace runtime surface`：

- Codex local entrypoint
- workspace-local state
- request / result lane
- bot / desktop / local TUI 可共同消費的 capability surface

但現在還缺一份專門的文檔把這件事說清楚。結果是：

- `.threadbridge/` 容易被理解成一堆實作細節，而不是正式 runtime contract
- 目前固定安裝的 tool set 對所有 workspace 一視同仁，沒有區分 project type
- image / Telegram / prompt-build 類能力會和一般 coding workspace 混在同一套 surface
- 後續若想加更多 tool，容易直接變成 wrapper 不斷增生，而不是能力面被規劃

## 定位

這份文檔定義的是 `workspace runtime surface` 主草稿。

它處理的是：

- 真實 workspace 內 `./.threadbridge/` 的正式角色
- 哪些 artifact / wrapper / state file 應被視為 workspace runtime surface
- 這個 surface 如何成為 Telegram、desktop runtime、`hcodex`、tool wrappers 的共同工作面
- 未來如何按 project type / workspace profile 選擇啟用 tools

它明確不處理：

- Telegram outbound delivery 主規格
- thread / binding / state machine 主規格
- desktop runtime capability host 本身的跨沙盒 API
- secondary LLM / image generation 的產品策略細節

## 核心想法

### 1. `workspace runtime surface` 是正式產品面，不只是 bootstrap 副產物

threadBridge 的一個重要特徵是：

- 工作目錄是實際 workspace
- runtime surface 也直接存在於實際 workspace 裡

這使得 `./.threadbridge/` 不應只被當成安裝細節，而應被理解成：

- workspace-local runtime contract
- bot / desktop / local TUI 的共享工作面

### 2. surface 的價值在於把責任落回 workspace

這個 surface 目前已經承接：

- local `hcodex`
- tool request / result lane
- workspace execution mode
- workspace-local runtime observation
- Telegram outbox

這類能力如果都退回 bot-local `data/` 或 Telegram handler，會讓 runtime 邊界重新變模糊。

因此較合理的方向是：

- 優先思考哪些能力能先落在 workspace runtime surface
- 再由 Telegram / desktop / management UI 去消費它

### 3. future surface 不應永遠是固定 wrapper 大全

目前所有 workspace 預設拿到同一組 wrapper，這對早期很務實。

但長期上更合理的模型應該是：

- workspace runtime surface 有穩定骨架
- tool capability 則依 project type / workspace profile 決定

也就是說：

- 不是每個 workspace 都必須預設帶 image / Telegram / prompt-build 類 tool
- 也不是每次新增 capability 都要無條件灑到所有 workspace

## 目前已存在的 surface

目前最核心的 surface 可分成四類。

### 1. Wrapper commands

- `./.threadbridge/bin/hcodex`
- `./.threadbridge/bin/build_prompt_config`
- `./.threadbridge/bin/generate_image`
- `./.threadbridge/bin/send_telegram_media`

### 2. Workspace-local runtime state

- `./.threadbridge/state/workspace-config.json`
- `./.threadbridge/state/app-server/*`
- `./.threadbridge/state/runtime-observer/*`

這裡需要固定一個 Phase 1 已確認的語義：

- `app-server/*`
  - workspace 受管 daemon / proxy endpoint 的當前連線資訊
- `runtime-observer/*`
  - local TUI / managed runtime 活動與 mirror 相關的 observation / activity surface
  - 不是 machine-level runtime health 的 canonical authority
  - 不應再被文檔描述成「誰擁有 runtime」的 ownership surface

### 3. Tool request / result lane

- `./.threadbridge/tool_requests/*`
- `./.threadbridge/tool_results/*`

### 4. Managed appendix / shell compatibility

- workspace `AGENTS.md` 內的 managed appendix
- `./.threadbridge/shell/*`

## 長期方向：按 project type 選擇啟用 tools

這裡先記錄一個未來方向：

- workspace runtime surface 應能依 project type / workspace profile，決定啟用哪些 tools

這個想法的目標不是做複雜 plugin marketplace，而是先解決一個比較實際的問題：

- 不同 workspace 需要的 runtime surface 並不相同

例如：

- 一般 coding workspace
  - 需要 `hcodex`
  - 可能需要 workspace execution config
  - 不一定需要 image / prompt-build / Telegram media wrapper
- image / concept workspace
  - 可能需要 `build_prompt_config`
  - 可能需要 `generate_image`
  - 可能需要 `send_telegram_media`
- docs / audit workspace
  - 可能只需要 `hcodex`、observability、少量 export capability

因此較合理的長期模型應該是：

- `workspace runtime surface core`
  - 所有 workspace 都有
- `workspace tool capability set`
  - 依 profile 選擇啟用

## 建議的資料模型

初版可考慮補一個顯式的 workspace surface profile 檔案，例如：

- `./.threadbridge/state/runtime-surface.json`

至少包含：

- `schema_version`
- `profile`
  - 例如 `coding`
  - `image_workflow`
  - `docs`
- `enabled_tools`
  - `hcodex`
  - `build_prompt_config`
  - `generate_image`
  - `send_telegram_media`
- `required_state_surfaces`
  - `workspace_config`
  - `app_server`
  - `shared_runtime`
  - `tool_io`
- `materialized_at`

這裡先不強行要求檔名一定如此。

但主張應先固定的是：

- workspace surface profile 需要有顯式 source of truth
- 不應只靠「看到哪些檔案存在」來倒推 capability

## 穩定骨架與可選 capability

比較合理的切法是：

### 穩定骨架

所有 workspace 都應保留：

- `./.threadbridge/bin/hcodex`
- `./.threadbridge/state/workspace-config.json`
- `./.threadbridge/state/app-server/*`
- `./.threadbridge/state/runtime-observer/*`

原因是：

- 這些已經接近 workspace runtime surface 的基礎 contract
- 其中 `runtime-observer/*` 的角色應固定為 observability/activity contract，而不是 owner authority

### 可選 capability

下面這些更適合走 profile / capability 啟用：

- `build_prompt_config`
- `generate_image`
- `send_telegram_media`
- 未來其他 workspace-local wrappers

原因是：

- 這些更接近 workflow-specific tool surface
- 不一定適合對所有 project type 預設開啟

## 與其他計劃的關係

- [session-level-mirror-and-readiness.md](session-level-mirror-and-readiness.md)
  - 描述 shared runtime、`hcodex`、mirror、adoption 的現行模型
- [codex-execution-modes.md](codex-execution-modes.md)
  - `workspace-config.json` 已是 surface 的正式一部分
- [desktop-runtime-tool-bridge.md](../desktop-runtime-owner/desktop-runtime-tool-bridge.md)
  - 描述如何從 workspace tool surface 呼叫 desktop capability host
- [optional-agents-injection.md](optional-agents-injection.md)
  - 描述 appendix 是否必須 inline 注入，但不取代 surface 本身
- [runtime-protocol.md](runtime-protocol.md)
  - workspace runtime health 的 public surface 已改成 `runtime_readiness`
  - 後續若 surface profile 要對外可見，應掛進正式 view / action 命名
- [post-cli-runtime-cleanup.md](post-cli-runtime-cleanup.md)
  - `runtime-observer/*` 已是主寫入 surface，`shared-runtime/*` 只保留 legacy read compatibility
  - `local-tui-session.json` 已是主寫入 surface，legacy `local-session.json` 仍保留 read compatibility

## 開放問題

- workspace surface profile 應該放進既有 `workspace-config.json`，還是獨立成新的 `runtime-surface.json`？
- `hcodex` 是否屬於永遠不可關閉的 core capability，還是極少數 profile 也可省略？
- `send_telegram_media` 這種 adapter-aware wrapper，是否適合做成某些 profile 才啟用？
- project type 應由使用者手選、由 repo 偵測、還是由 workspace template 決定？
- 之後若新增更多 tool，應該先掛進 capability set，還是先證明它屬於穩定骨架？

## 建議的下一步

1. 先把 `workspace runtime surface` 這個概念固定為正式術語，而不是只在 appendix / code 裡零散出現。
2. 決定穩定骨架與可選 capability 的邊界，先不要再把所有 wrapper 都默認成全 workspace 安裝。
3. 定義一個最小 workspace surface profile 資料模型。
4. 再決定 profile 是手動選擇、workspace template 派生，還是兩者並存。
5. 等 profile 模型清楚後，再回頭整理 `runtime_assets/templates/AGENTS.md`、workspace bootstrap 與 management surface。
