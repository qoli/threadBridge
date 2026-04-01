# Runtime Data Root 草稿

## 目前進度

這份文檔目前是「部分落地」。

目前代碼裡已經有的部分：

- `RuntimeConfig` 已長期攜帶 `data_root_path` / `debug_log_path`
- repository、runtime owner、workspace helper、app-server worker 都已經走 shared data-root plumbing
- `DATA_ROOT` / `DEBUG_LOG_PATH` override 與 `BOT_DATA_PATH` compatibility 仍可使用
- 預設 data root 已不再一律寫死 `./data`

目前尚未完成：

- 更完整的 public release / packaging 文件收斂仍在其他計劃中
- 目前只固定 build-profile 雙模式，不額外提供新的 runtime mode enum 或 UI control
- historical / exploratory 文檔中仍可能保留舊 `data/` wording，應視上下文理解

## 問題

過去 `threadBridge` 雖然已把 bot-local state 的讀寫路徑抽成 `data_root_path`，但預設值仍落在 repo-local `./data`。

這會帶來兩個問題：

- public / bundled desktop runtime 沒有正式的 app-data 落點
- maintainer 與使用者文件會把「debug 開發預設」誤寫成「所有模式的正式契約」

同時，若在切換到 app-data 時又自動搬移、複製、或回頭探測 repo-local `data/`，反而會讓行為不透明，破壞開發模式與 release 模式的隔離。

## 定位

這份文件定義 bot-local runtime state 與 installed runtime assets 的本地路徑契約。

它處理：

- 預設 data root 的 build-profile 規則
- installed runtime assets root 的 build-profile / bundle 規則
- 平台 local app-data dir 的映射
- override precedence
- no-migration / no-copy / no-fallback 邊界
- helper script 與 maintainer docs 應如何表達這個契約

它不處理：

- workspace `.threadbridge/` surface 本身的檔案結構
- public release / notarization / DMG 流程
- Telegram 或 management API 的功能語義

## 主體規格

### 1. 資料根目錄模式

- `debug` build 預設使用 repo-local `./data`
- `debug` build 的 source assets 預設使用 repo-local `./runtime_assets`
- bundled `release` build 預設使用平台 local app-data dir 下的 `threadBridge/data`
- bundled `release` build 的 installed runtime assets 預設使用平台 local app-data dir 下的 `threadBridge/runtime_assets`
- 這是 build-profile 規則，不額外引入新的 runtime mode enum

### 2. 平台映射

- macOS:
  - data root: `~/Library/Application Support/threadBridge/data`
  - installed runtime assets root: `~/Library/Application Support/threadBridge/runtime_assets`
- Linux: `$XDG_DATA_HOME/threadBridge`，否則 `~/.local/share/threadBridge`
- Windows: `%LOCALAPPDATA%\\threadBridge`

### 3. Override Precedence

- `DATA_ROOT` 是正式的顯式 override
- `BOT_DATA_PATH` 只保留 compatibility；若使用，data root 取其 parent
- `DEBUG_LOG_PATH` 可顯式覆蓋 event log 路徑
- 若未指定 `DEBUG_LOG_PATH`，則預設派生為 `<data_root>/debug/events.jsonl`

### 4. Migration Policy

- 不自動搬移既有 repo-local `data/`
- 不自動搬移既有 repo-local `runtime_assets/`
- bundled release 只在 installed runtime assets 缺檔時，從 app bundle seed assets 補建
- 不建立 symlink
- 不在 app-data 為空時回頭探測 repo-local `data/`
- 若 `release` 預設模式無法取得平台 local app-data dir，應直接 fail fast，而不是退回 `./data`

### 5. 文件與工具表述

- README、AGENTS、workspace appendix source、local helper script 都應採用 mode-aware wording
- `data/` 與 `runtime_assets/` 都要維持 repo/source 與 bundled/install layout 同構

## 驗收標準

- `cargo run --bin threadbridge_desktop` 仍預設寫入 repo-local `data/`
- `cargo run --release --bin threadbridge_desktop` 預設寫入平台 local app-data dir 下的 `threadBridge/data`
- `DATA_ROOT` 能覆蓋兩種模式
- release 模式不會自動讀取、複製或搬移 repo-local `data/`
- bundled release 首次啟動可在 repo 外自動補齊缺失的 `runtime_assets/`
- workspace runtime / `hcodex` / session continuity 在 repo 外的 bot-local data root 下仍可正常工作

## 與其他計劃的關係

- [macos-public-release-track.md](../desktop-runtime-owner/macos-public-release-track.md)
  - 該文檔把 app-data 落點視為 release gate；本文定義更細的 runtime path contract
- [runtime-architecture.md](runtime-architecture.md)
  - 本文不重定義 owner boundary，只補 runtime path policy
- [workspace-runtime-surface.md](workspace-runtime-surface.md)
  - 本文不改 workspace-local `.threadbridge/` surface，只處理 bot-local state root
