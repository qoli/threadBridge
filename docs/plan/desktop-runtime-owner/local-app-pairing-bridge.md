# Local Session Discovery and Pairing Bridge 草稿

## 目前進度

這份文檔目前是純草稿，尚未開始實作。

目前代碼裡已經有的前置條件：

- `threadbridge_desktop` 已是 machine-local runtime owner
- local management API 已有 workspace / thread / runtime 查詢與 control action
- workspace app-server ws backend 已由 desktop owner 管理
- Telegram adapter 已經和 shared `runtime_control`、observer、delivery bus 分層
- Codex home-dir session mirror 已固定成 Telegram read-side projection，不回放給 app-server

目前尚未完成：

- threadBridge 啟動的 Codex 對話，尚未有能讓 Codex Desktop 或其他本機 Codex surface 快速發現的 owner-published session index
- threadBridge 管理的 thread / turn / busy / interrupt / final 狀態，尚未有對外的同步語義；其他 app 只能慢速或間接觀察，不能跟同一個 runtime truth 對齊
- threadBridge 尚未有外部本機 app 的 pairing registration model
- 尚未有由 desktop owner 管理的 Unix socket / local socket capability bridge
- 尚未定義外部 app 能查詢或修改 threadBridge runtime 的最小 capability set
- 尚未定義 pairing lifecycle、權限、信任、審計與 replay 禁止規則

這份草稿來自一次 VSCode Codex VSIX 的架構觀察：

- VSCode Codex extension 自己啟動 bundled `codex app-server`，用 stdin / stdout JSONL 管 thread / turn；這表示 extension 是一個正式 Codex app-server host，而不是只寫 home-dir transcript 的旁路工具
- macOS ChatGPT app 的 `Work with VS Code` 路徑不是 app-server replay，而是 extension 在本機寫 pairing registration，再開 Unix socket 暴露少量 IDE capability
- VSCode extension 同時有獨立的 MCP / LSP bridge，讓 Codex 透過明確工具查詢 IDE diagnostics / references / workspace symbols；這是輔助模式，不是這份草稿的主線

這些觀察對 threadBridge 的啟發是：

- threadBridge 不缺第二套 `codex app-server` stdin / stdout client；它缺的是把自己已經擁有的 app-server host 狀態發佈成可被本機 app 快速發現的 owner surface
- 對話同步不應理解成 replay 兩邊的 user input，而應理解成 discovery、liveness、active thread、busy truth、turn event、controlled action 的同步
- 外部 app integration 應該走獨立 pairing / capability bridge
- 不應把外部 app 的對話或輸入直接 replay 到 threadBridge 管理的 app-server
- capability bridge 應該暴露少量 typed operation，而不是共享 raw transcript 或任意 shell

## 問題

threadBridge 目前已有兩種主要互動面：

- Telegram adapter
- local management / desktop surface

另外還有兩種 Codex 相關路徑：

- threadBridge 自己管理的 workspace app-server / observer / `hcodex`
- Codex Desktop 或 Codex TUI 自己寫到 user home 的 session log

這些路徑今天已經能部分「交叉可見」，例如外部 Codex session 可以透過 home-dir mirror 投影到 Telegram。但這種 mirror 是 read-side observation，不是正式 integration protocol。

更具體地說，目前 threadBridge 啟動的對話有兩個體感問題：

- Codex Desktop 不能很快發現 threadBridge 這邊新建或切換的 active Codex thread
- 即使之後透過 home-dir session 或其他觀察面看到了，也不代表 interrupt、busy、turn event、active session、final delivery 等行為已同步

如果之後希望 threadBridge 和其他本機 app 更直接協作，例如：

- ChatGPT macOS app
- VSCode / Cursor / Windsurf extension
- 其他本機 IDE 或 agent shell
- threadBridge 自己未來的 desktop helper app

就需要一條正式的 local pairing bridge。

這條 bridge 要解決的不是「怎樣把外部 app 的訊息塞進 Codex thread」，而是：

- threadBridge 如何把自己管理中的 workspace / session / active thread 發佈給本機其他 Codex surface
- 外部 app 如何發現 threadBridge desktop owner
- threadBridge 如何知道對方是誰、有哪些 capability、是否還活著
- 雙方如何交換少量 runtime / IDE / app context
- 哪些 operation 可以安全執行
- 哪些 operation 必須被明確禁止

## 定位

這份文檔定義的是：

- `desktop runtime owner` 底下的 local session discovery / runtime sync / app pairing 草稿

它處理：

- threadBridge-managed workspace / current thread / runtime endpoint 如何被本機 Codex surface 發現
- 本機 app 如何註冊自己可被 threadBridge 發現
- threadBridge 如何暴露受限 local socket endpoint
- pairing payload 應包含哪些身份、workspace、capability、socket、timestamp 資訊
- owner-side session index 應包含哪些 workspace、thread、run status、runtime status 資訊
- 外部 app 能呼叫哪些 typed capability
- threadBridge 能否反向呼叫對方 capability
- pairing bridge 和 app-server、Telegram mirror、desktop tool bridge 的邊界

它不處理：

- 直接取代 app-server ws backend
- 直接新增一條 Codex thread replay path
- 讓 Codex Desktop 自動接管 threadBridge 的 app-server lifecycle
- Telegram renderer / delivery 細節
- workspace-local `.threadbridge/bin/*` tool wrapper
- general-purpose desktop automation
- 任意跨 sandbox shell escape

## 修正後的核心焦點

這份草稿的主線應該是「threadBridge 作為 Codex app-server host，如何被其他本機 Codex surface 發現並同步狀態」。

VSIX 值得參考的不是它有 MCP，而是它把 extension 做成了一個完整 host：

- extension 管理 bundled `codex app-server` 的 process lifecycle
- extension 透過 JSONL stdin / stdout 管 thread / turn
- extension 對外提供 desktop pairing registration
- extension 暴露少量受控 host capability，讓另一個 desktop app 能查詢或操作 IDE context

threadBridge 已經有前兩項的同類能力：

- [codex.rs](../../../rust/src/codex.rs) 已有 stdio / websocket app-server client
- [app-server-ws-backend.md](app-server-ws-backend.md) 已把 workspace app-server 收斂成 desktop owner 管理的 backend plane

所以 threadBridge 不應重做 VSIX 的 app-server client。它應補的是後兩項：

- owner-published discovery surface：讓 Codex Desktop / 本機 app 很快知道 threadBridge 正在管理哪些 workspace、thread、active session、socket/API endpoint
- owner-backed sync surface：讓外部 app 能用 typed query / event / action 對齊 threadBridge 的 runtime truth，而不是等 home-dir mirror 或各自推測

這裡的「同步」不是雙寫 transcript，也不是讓兩個 app-server 同時執行同一輪。

同步應固定成：

- `discover`: 發現 threadBridge owner、workspace、current thread、runtime endpoint
- `observe`: 讀取 thread status、busy truth、turn phase、last final、interaction request
- `subscribe`: 接收 thread / turn / runtime health 的增量事件
- `act`: 經 shared runtime control 執行 interrupt、launch、repair、policy 這類受控 action

不應包含：

- external app 直接呼叫 `turn/start`
- external app 直接呼叫 `turn/steer`
- external app 自行修改 `session-binding.json`
- external app 用 raw Codex JSONL transcript 當同步來源

## VSIX 觀察到的架構模式

這裡只記錄對 threadBridge 有用的模式，不把 VSIX 實作視為可直接照搬的 contract。

### 1. Codex app-server process 仍是 extension 自己管理的 backend

VSCode Codex extension 內部會啟動 bundled `codex app-server`，再透過 JSON-RPC shape 的 stdin / stdout message 管理：

- `initialize`
- `thread/start`
- `thread/resume`
- `thread/read`
- `turn/start`
- `turn/interrupt`
- internal notification handler

threadBridge 目前在 [codex.rs](../../../rust/src/codex.rs) 已有類似 client 能力，而且已支援 stdio 與 websocket。

因此這條線對 threadBridge 的結論不是「重做 app-server client」，而是：

- app-server control 仍應留在 `app-server-ws-backend` / `CodexRunner`
- local app pairing bridge 不應成為第二套 app-server client

### 2. macOS app pairing 使用 registration file + Unix socket

VSIX 的 macOS desktop bridge 會：

- 在 user home 的 app support 目錄寫 registration payload
- payload 包含 app name、bundle id、extension id / version、workspace name、capabilities、socket path、timestamp
- 在 `/tmp/<id>.sock` 建立 Unix socket
- socket message 使用 length-prefixed JSON frame
- extension deregister 時移除 registration file 與 socket

這個模式值得 threadBridge 借鑑，因為它把 discovery 和 transport 分開：

- registration file 是 discovery / liveness / metadata surface
- Unix socket 是 command transport
- capability list 是可協作範圍

### 3. desktop bridge 暴露的是少量 host capability

VSIX 的 desktop bridge 不是讓 ChatGPT app 直接控制 VSCode extension 的全部內部狀態，而是暴露少量 IDE capability，例如：

- `ping`
- active editor content
- selection
- highlight
- set content
- replace selection
- reload / mark for reload

threadBridge 應採用同樣方向：

- pairing bridge 只暴露明確 capability
- 每個 capability 都有 request / response schema
- 沒有 schema 的操作不能穿過 bridge

### 4. MCP / LSP bridge 是可選的第二層，不是主線

VSIX 另有 LSP MCP bridge，讓 Codex 用 tool 查詢 IDE diagnostics、references、workspace symbols。

對 threadBridge 的啟發是：

- 如果未來要讓 Codex 主動查 IDE diagnostics 或 workspace symbols，可以另開 typed MCP / tool capability
- 不應把 runtime 狀態長篇注入 `AGENTS.md` 或每輪 prompt
- 但目前要解的問題是 threadBridge-started session 的 discovery / sync，不是先新增 MCP

## Proposed Model

### 1. Discovery

threadBridge desktop owner 可以維護一個本機 owner / pairing registry，例如：

```text
~/Library/Application Support/threadBridge/app_pairing_extensions/<pairing_id>.json
```

或在 release app-data root 下提供同等 surface。

registration payload 應至少包含：

```json
{
  "schema_version": 1,
  "pairing_id": "uuid",
  "app_name": "VS Code",
  "bundle_id": "com.microsoft.VSCode",
  "adapter_name": "openai.chatgpt",
  "adapter_version": "26.506.31322",
  "workspace_name": "threadBridge",
  "workspace_cwd": "/Volumes/Data/Github/threadBridge",
  "capabilities": ["ping", "active_editor_context"],
  "socket_path": "/tmp/threadbridge-pairing-<pairing_id>.sock",
  "registered_at": "2026-05-09T00:00:00Z",
  "expires_at": null
}
```

不應把 secrets、raw transcript、ChatGPT token、Telegram token、或 provider payload 寫進 registration。

threadBridge 自己還應發布 owner-side session index，讓其他本機 Codex surface 不必等 home-dir mirror 慢慢掃到：

```json
{
  "schema_version": 1,
  "owner": "threadbridge_desktop",
  "workspaces": [
    {
      "workspace_cwd": "/Volumes/Data/Github/threadBridge",
      "workspace_name": "threadBridge",
      "current_thread_id": "codex-thread-id",
      "thread_key": "telegram-thread-key-or-local-key",
      "runtime_status": "healthy",
      "run_status": "idle",
      "socket_path": "/tmp/threadbridge-owner.sock",
      "management_url": "http://127.0.0.1:<port>"
    }
  ],
  "updated_at": "2026-05-09T00:00:00Z"
}
```

這份 index 是 discovery / liveness surface，不是 authority 本體。真正 authority 仍在 desktop owner、app-server backend、shared runtime control。

### 2. Transport

v1 可以採用 Unix socket，frame 使用簡單 length-prefixed JSON：

```text
uint32_le byte_length
utf8_json_payload
```

基本 message shape：

```json
{
  "id": "request-id",
  "method": "ping",
  "params": {}
}
```

response shape：

```json
{
  "id": "request-id",
  "status": "success",
  "result": {}
}
```

錯誤 response：

```json
{
  "id": "request-id",
  "status": "error",
  "error": {
    "code": "not_trusted",
    "message": "Workspace is not trusted for this capability."
  }
}
```

### 3. Capability Categories

初版應只考慮四類 capability。

#### Query

Query capability 只能讀取狀態，不改變 runtime：

- `ping`
- `workspace_status`
- `thread_status`
- `current_session`
- `recent_sessions`
- `runtime_health`
- `current_active_thread`
- `turn_status`

這些能力大多可由 existing management / runtime protocol view 派生。

#### Subscription

Subscription capability 用來解決「Codex Desktop 發現慢、狀態不同步」的問題，但仍只推送 threadBridge owner 已經知道的 runtime truth：

- `subscribe_runtime_events`
- `subscribe_thread_events`
- `subscribe_workspace_events`

event 應使用 typed payload，來源可對齊 existing local management SSE / runtime protocol event vocabulary。

#### Controlled Mutation

Controlled mutation capability 會改變 threadBridge 狀態，必須走 shared runtime control：

- `interrupt_running_turn`
- `set_running_input_policy`
- `launch_local_session`
- `repair_session_binding`

這些 capability 不應直接操作 Telegram adapter 或 repository internals。

它們應翻譯成 existing `RuntimeControlActionRequest` 或後續 transport-neutral action。

#### Host Context

Host context capability 是外部 app 提供給 threadBridge 的 context：

- active editor file
- selected text
- diagnostics summary
- open workspace roots

threadBridge 可以消費這些 context，但 v1 不應把它們自動送進 Codex turn。

使用方式應是：

- 顯示給 user / management surface
- 作為 explicit context attachment
- 或經過明確 action 後才進入 Codex input

## Hard Boundaries

### 1. 不做 app-server replay

pairing bridge 不得把外部 app message 自動轉成：

- `turn/start`
- `turn/steer`
- `thread/resume`
- `thread/start`

如果未來要支援「外部 app 對 threadBridge thread 發言」，也必須先成為 shared runtime control action，並有明確 UI / audit / owner 語義。

同樣地，threadBridge 也不應為了讓 Codex Desktop「看起來同步」而把 Telegram input 重新寫入 Codex Desktop 自己的 app-server。那會製造兩條 execution truth。

### 2. 不取代 Codex home-dir mirror

home-dir mirror 仍是 Telegram read-side projection。

pairing bridge 不應讀 raw Codex JSONL，也不應替代 existing cursor / delivery bus 去重。

如果 pairing app 能提供更即時的外部 app event，也應作為新的 observation source，不能繞過 transcript mirror / delivery ownership。

### 3. 不把外部 app 當 runtime owner

外部 app 可以提供 capability 或 context，但不能成為：

- workspace runtime authority
- session continuity authority
- owner-canonical runtime health source

machine-level authority 仍屬於 `desktop runtime owner`。

### 4. 不繞過 shared runtime control

任何會改變 thread / workspace / session state 的 operation，都應先落到 shared runtime control vocabulary。

pairing bridge 只是 transport / discovery / capability boundary，不是新的 control core。

### 5. 不把 pairing registration 當安全憑證

registration file 只能作 discovery metadata。

真正執行 capability 前仍需要：

- socket ownership / file mode 檢查
- pairing id 驗證
- workspace trust 檢查
- capability allowlist
- 必要時 user confirmation

## 與既有計劃關係

- [runtime-architecture.md](../runtime-control/runtime-architecture.md)
  - 固定 owner / runtime_control / observer / adapter 邊界；pairing bridge 必須歸 `desktop runtime owner`
- [app-server-ws-backend.md](app-server-ws-backend.md)
  - app-server backend 仍是 Codex execution substrate；pairing bridge 不替代 backend
- [runtime-protocol.md](../runtime-control/runtime-protocol.md)
  - controlled mutation capability 應回到 shared action / event vocabulary
- [codex-home-dir-session-mirror.md](../telegram-adapter/codex-home-dir-session-mirror.md)
  - 外部 Codex session 仍只透過 read-side mirror 投影，不經 pairing bridge replay
- [desktop-runtime-tool-bridge.md](desktop-runtime-tool-bridge.md)
  - tool bridge 是 workspace / Codex 請求 desktop capability；pairing bridge 是外部 local app 和 desktop owner 交換 capability
- [message-queue-and-status-delivery.md](../telegram-adapter/message-queue-and-status-delivery.md)
  - pairing bridge 不應暗示 inbound user input queue；正式輸出仍需 delivery ownership

## 建議最短實作路徑

### Phase 0: docs / protocol sketch

- 固定這份文檔的主線是 session discovery / runtime sync，而不是 MCP/LSP
- 固定 registration payload
- 固定 owner-side session index payload
- 固定 socket frame
- 固定 v1 capability names
- 固定禁止 app-server replay 的硬邊界

### Phase 1: owner-side read-only session registry

- `threadbridge_desktop` 建立 pairing registry dir
- 支援寫入 threadBridge 自己的 owner registration / session index
- index 至少包含 workspace、current thread、run status、runtime status、socket/API endpoint
- management UI 顯示 active pairings 與 owner-published sessions
- 不接受任何 mutation

### Phase 2: Unix socket query server

- owner 開 Unix socket
- 支援 `ping`
- 支援 `runtime_health`
- 支援 `thread_status` / `workspace_status`
- 支援 `current_active_thread` / `turn_status`
- 所有 request / response 寫入 audit event

### Phase 3: event subscription

- 對齊 local management SSE / runtime protocol event vocabulary
- 支援訂閱 workspace / thread / runtime health 增量事件
- Codex Desktop 或其他本機 surface 不需要輪詢 home-dir JSONL 才能知道狀態變化
- event 只同步 threadBridge owner truth，不同步 raw transcript

### Phase 4: controlled mutations

- 只透過 `RuntimeControlActionRequest`
- 初版只接：
  - `interrupt_running_turn`
  - `set_running_input_policy`
- 需要明確 origin / actor / pairing id
- mutation result 回寫 audit log

### Phase 5: host context intake

- 支援外部 app 主動提供 active editor / selection / diagnostics
- 只存成 explicit context artifact 或 management-visible snapshot
- 不自動開始 Codex turn

## Open Questions

- pairing registry 應放在 ChatGPT app 類似的 user home app support，還是 threadBridge release data root？
- owner-side session index 是否應與 pairing registry 同目錄，還是獨立為 `threadbridge_owner/current.json`？
- pairing id 是否應每次 desktop owner 啟動重建，還是 per app / per workspace 穩定？
- 是否需要雙向 pairing，也就是 threadBridge 同時讀外部 app registration，外部 app 也讀 threadBridge registration？
- query capability 是否應直接走 local management API，而 socket 只保留給 app-to-app pairing？
- event subscription 應復用 local management SSE，還是另開 UDS framed JSON event stream？
- controlled mutation 是否全部需要 user confirmation，還是可按 capability / paired app trust level 分級？
- host context artifact 應存進 bot-local runtime data root，還是 workspace-local `.threadbridge/state/`？
- 如果同一 workspace 同時被 VSCode、Cursor、ChatGPT macOS app pairing，threadBridge 如何呈現 priority / active focus？

## 測試要求

- registration payload 不含 secrets
- owner-side session index 不含 raw transcript / provider payload / Telegram token
- registration missing / stale / invalid JSON 不會阻塞 desktop owner
- socket frame parser 能處理 partial read / oversized frame / invalid JSON
- unknown capability 回傳 typed error
- event subscription 不會重送 raw app-server JSONL
- untrusted workspace 拒絕非 `ping` capability
- controlled mutation 必須經 shared runtime control，不可直接改 repository
- `turn/start` / `turn/steer` 不存在於 pairing bridge v1 capability allowlist
- socket cleanup 不會刪除非 socket 檔案
