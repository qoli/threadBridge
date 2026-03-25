# Hcodex Launch Contract 草稿

## 目前進度

這份文檔目前已進入「部分落地」。

目前已確認：

- `resolve-hcodex-launch` 會回傳 ingress launch URL，而不是可直接交給 upstream Codex `--remote` 的 canonical endpoint
- launch URL 可能帶有 one-shot `launch_ticket` 等 sideband handshake state
- `run-hcodex-session` 目前必須先啟動本地 `hcodex-ws-bridge`，再把本地 `ws://127.0.0.1:<port>/` 交給 Codex
- `hcodex-ws-bridge` 必須在本地短暫 reconnect 視窗內保留同一條 upstream websocket session，並對啟動階段的重複 request 做本地 replay
- 代碼中已補上對應註釋與回歸測試，避免後續重構再次把這兩個 bug 帶回來

目前尚未完成：

- 這個 launch contract 仍屬於 ingress / compatibility boundary，而不是長期最終形態
- 若未來 upstream Codex 原生支持更寬鬆的 remote attach / reconnect contract，這層 bridge 仍可再簡化

## 問題

`hcodex` 這條路徑同時面對兩個不同的 websocket contract：

1. threadBridge ingress launch URL
   - 可能包含 `launch_ticket` 等一次性 sideband state
   - 目的是讓 ingress 知道這次 `hcodex` launch 要接到哪個 thread
2. upstream Codex `--remote`
   - 目前只接受 bare `ws://host:port/` 或 `wss://host:port/`
   - 不能帶 query、fragment，也不能帶非 root path

這兩個 contract 不能混為一談。只要未來維護者把「launch URL」誤當成「Codex remote URL」，或把本地 reconnect 誤當成「可再次撥打同一個 launch URL」，就會把已修過的 bug 再次引回來。

## 兩個已知回歸

### 1. `launch_ticket` 直接傳給 Codex

錯誤做法：

- 把 `ws://127.0.0.1:61399/?launch_ticket=...` 直接交給 `codex --remote`

實際結果：

- upstream Codex 在 remote address normalize 階段直接拒絕
- 終端報錯為 `invalid remote address ...`

目前防線：

- [`hcodex_runtime.rs`](/Volumes/Data/Github/threadBridge/rust/src/hcodex_runtime.rs#L258) 會在 spawn Codex 前先啟動 `hcodex-ws-bridge`
- [`hcodex_ws_bridge.rs`](/Volumes/Data/Github/threadBridge/rust/src/hcodex_ws_bridge.rs#L90) 的 `is_codex_safe_remote_ws_url` 明確鏡像 upstream Codex 的限制
- 只有 bare `ws://host:port/` 才能略過 bridge

### 2. 第二條本地 websocket 觸發 `failed to connect to remote app server`

錯誤做法：

- 第一條本地 Codex websocket 已經成功完成 `initialize`
- Codex 啟動過程中又打了第二條本地 websocket
- bridge 把第二條連線上的 `initialize` 再次轉發到同一條 upstream ingress session

實際結果：

- upstream app-server 對第二次 `initialize` 回 `Already initialized`
- Codex 在本地只顯示成 `Error: failed to connect to remote app server`

目前防線：

- [`hcodex_ws_bridge.rs`](/Volumes/Data/Github/threadBridge/rust/src/hcodex_ws_bridge.rs#L128) 會保留首條 upstream session，而不是重撥 launch URL
- [`hcodex_ws_bridge.rs`](/Volumes/Data/Github/threadBridge/rust/src/hcodex_ws_bridge.rs#L210) 會對 reconnect startup request replay 已快取的回應
- [`hcodex_ws_bridge.rs`](/Volumes/Data/Github/threadBridge/rust/src/hcodex_ws_bridge.rs#L198) 會吞掉重複的 `initialized` notification

## 當前契約

### 1. `resolve-hcodex-launch`

只負責：

- 選定 thread
- 產生 ingress launch URL
- 在 query string 內附帶 `launch_ticket`

不負責：

- 保證該 URL 符合 upstream Codex `--remote` 的限制
- 處理本地 reconnect

### 2. `run-hcodex-session`

只要收到的 launch URL 不是 bare `ws://host:port/`，就必須：

1. 啟動 `hcodex-ws-bridge`
2. 等 bridge ready file
3. 把 bridge 回傳的本地 `ws://127.0.0.1:<port>/` 交給 Codex

這一步是唯一允許做 launch URL -> Codex remote URL 適配的 compatibility boundary。

但這個 boundary 只解決 transport contract，不取代 lifecycle supervision：

- upstream Codex 只要求 `--remote` 拿到合法 websocket endpoint
- `hcodex` 仍必須自己承擔本地 `codex --remote` child 的 spawn / signal forwarding / teardown / final reconciliation
- 不能因為 upstream Codex 已處理 websocket initialize 與 `thread/resume`，就把 `hcodex` 做薄成只剩 launch adapter

### 3. `hcodex-ws-bridge`

bridge 的責任有兩個，而且兩個都不能少：

1. 保留完整 launch URL
   - upstream 握手時必須保留原始 path / query，不能掉 `launch_ticket`
2. 保留單次 upstream session
   - 本地短暫 reconnect 時不能再對同一個 launch URL 做第二次 `connect_async`

如果只保留其中一個，就會重新觸發上面兩個回歸之一。

## 維護不變式

任何重構只要碰到 `resolve-hcodex-launch`、`run-hcodex-session`、`hcodex-ws-bridge`，都必須保住下面幾條：

- `launch_ticket` 是 one-shot。它只能消耗一次，不能拿來支撐後續 reconnect。
- launch URL 不等於 Codex remote URL。除非 URL 滿足 bare `ws://host:port/` 的嚴格條件，否則必須經過 bridge。
- 本地第二條 websocket 不等於新的 upstream session。它只是同一個本地 Codex 啟動流程中的短暫 reconnect。
- 若未來想移除 bridge，前提不是「看起來多餘」，而是 upstream Codex contract 已真的放寬，且有新的集成測試覆蓋等價場景。

## 測試要求

凡是修改這條鏈路，至少要保住下面兩類測試：

- query/path launch URL 不能直接當 `--remote`
  - 目前由 [`hcodex_ws_bridge.rs`](/Volumes/Data/Github/threadBridge/rust/src/hcodex_ws_bridge.rs#L573) 附近的 URL contract 測試覆蓋
- 第二條本地 websocket 不得導致第二次 upstream `initialize`
  - 目前由 [`hcodex_ws_bridge.rs`](/Volumes/Data/Github/threadBridge/rust/src/hcodex_ws_bridge.rs#L678) 的 reconnect replay 測試覆蓋

如果未來有重構把這兩類測試刪掉，應視為高風險變更。

## 與其他計劃的關係

- [owner-runtime-contract.md](/Volumes/Data/Github/threadBridge/docs/plan/owner-runtime-contract.md)
  - 定義 `hcodex` / ingress 在 owner-managed runtime 中的責任邊界
- [post-cli-runtime-cleanup.md](/Volumes/Data/Github/threadBridge/docs/plan/post-cli-runtime-cleanup.md)
  - 記錄 CLI 時代 shim 收尾時，哪些 `hcodex` launch shim 仍應保留，哪些才是可清理的歷史殘留
- [hcodex-lifecycle-supervision.md](/Volumes/Data/Github/threadBridge/docs/plan/hcodex-lifecycle-supervision.md)
  - 補充 launch 完成後，`hcodex` 對 local Codex child lifecycle supervision 與 teardown 的責任

## 建議的下一步

- 若未來想再次簡化 `hcodex` launch 鏈，先從上游 Codex 的 remote contract 出發，而不是先刪 threadBridge 這側的 bridge
- 若未來再出現 `invalid remote address ...` 或 `failed to connect to remote app server`，優先回來檢查這份文檔列出的不變式
