# App-Server WS Backend Worker 進度報告（2026-03-27）

## 目前進度

這份報告覆蓋 `app-server-ws-backend` 的 backend worker 主線落地狀態，時間窗為 `2026-03-27` 當日提交。

基於 `app-server-ws-backend.md` 的目標語義，backend worker 主線目前可判定為「高比例部分落地」：

- workspace-scoped backend child worker：已落地
- worker-first run authority / busy state 查詢：已落地
- owner-managed `hcodex` launch endpoint 下沉 worker：已落地
- observer attach contract 仍是 `thread/resume` 語義：未完成
- backend plane 長期獨立 contract 收斂：未完成

本報告只覆蓋 backend worker 主線，不覆蓋 Telegram/management surface 的完整產品層收尾。

## 當日提交錨點（2026-03-27）

- `63fbe5b` `📝 docs(plan): 新增 backend worker 進度報告並更新本地執行腳本`
- `a71c48b` `feat(runtime): enforce worker-first run authority`
- `3ea86f7` `feat(runtime): surface run phase in session events`
- `bc76384` `test(runtime): cover unavailable worker views`
- `8a939ef` `feat(runtime): route tui interaction replies via worker`
- `edc590a` `refactor(runtime): drop telegram ingress adapter handle`
- `668ffbc` `refactor(runtime): keep owner-managed ingress in owner`
- `b7169d4` `refactor(runtime): split runtime workspace resolution helpers`
- `ce43533` `refactor(runtime): narrow ingress ownership in control`
- `45bad2f` `refactor(runtime): reuse live owner ingress during reconcile`
- `114c357` `refactor(hcodex): prefer worker runtime endpoint`
- `4bfcd27` `refactor(runtime): remove owner ingress from control construction`
- `e1c3fa5` `refactor(runtime): move owner ingress interaction wiring`
- `0e403e6` `refactor(runtime): remove owner ingress from app state construction`
- `6e6c1d6` `refactor(runtime): tighten owner ingress launch ownership`
- `32d2cc5` `feat(runtime): move owner-managed hcodex launch into worker`

## 增量進度（63fbe5b）

- `docs/plan` 已新增 backend worker 主線進度快照，並掛入 plan registry。
- 本地 runtime helper `scripts/local_threadbridge.sh` 已切到 `cargo build --bins`，避免只編 `threadbridge_desktop`。
- 本地 runtime helper 已固定檢查 `threadbridge_desktop` / `app_server_ws_worker` 兩個 binary，缺失時 fail-fast。
- 未使用的 `scripts/build_and_deploy_threadbridge.sh` 已移除，減少舊部署腳本對 today runtime 形狀的干擾。

## 已落地能力（backend worker 主線）

### 1. workspace-scoped backend worker 成為 runtime substrate

- `WorkspaceRuntimeManager` 目前以 `app_server_ws_worker` 作為 workspace runtime child，並在 state file 中固定寫入 `worker_ws_url` / `worker_pid` / `hcodex_ws_url`。
- worker 啟動時擁有並監督其上游 `codex app-server`，符合「worker 擁有 backend process substrate」的方向。

### 2. worker-first run authority 已進入 shared runtime 判斷路徑

- `CodexRunner` 新增 `threadbridge/getThreadRunState` 調用能力。
- `thread_state` 讀取 worker busy truth 來派生 `run_status` / `run_phase`，並把 `turn_interrupt_requested` 映射到 `turn_finalizing`。
- `runtime_protocol` 在 worker 不可用場景已回報 `run_status=unavailable` / `run_phase=unavailable`，避免偽裝成 idle。

### 3. interaction response 與本地 launch ownership 進一步下沉到 worker

- worker 目前可處理 `threadbridge/respondRequestUserInput`，把互動回覆注入對應 thread channel。
- worker 提供 `threadbridge/ensureHcodexIngress`，由 owner/runtime manager 透過 worker 取得並確保 `hcodex` launch endpoint。
- `hcodex` launch 路徑已優先依賴 worker runtime endpoint，不再由多處直接持有 owner ingress 細節。

### 4. control / adapter 對 owner ingress 的直接耦合持續收斂

- `runtime_control` 保留 owner-managed 與 self-managed 的分流，但 owner ingress 的 construction / wiring 已從多處抽離。
- Telegram runtime 端已移除舊的 ingress adapter handle，改走 worker/owner 收斂後的路徑。

## Worker 模式驗證 Runbook

以下三個訊號同時成立，可判定 workspace 已在 worker mode：

1. workspace runtime state file 具備 worker 欄位：`worker_ws_url`、`worker_pid`、`hcodex_ws_url`
   - 例：`cat .threadbridge/state/app-server/current.json`
2. `worker_pid` 對應實際 `app_server_ws_worker` 進程
   - 例：`ps -p <worker_pid> -o pid=,comm=,args=`
3. owner/management 視角可見 worker-ready runtime
   - owner heartbeat event `runtime_owner.workspace.app_server_ready` 包含 `worker_ws_url`
   - `/api/workspaces` 回傳 `app_server_status=running`、`hcodex_ingress_status=running`、`runtime_readiness=ready`

## 驗證證據（本次更新執行）

以下測試在本次報告更新時重新執行且通過：

- `cargo test app_server_ws_worker::tests::`
- `cargo test runtime_protocol::tests::build_thread_views_marks_missing_worker_as_unavailable`
- `cargo test runtime_protocol::tests::build_workspace_views_marks_missing_worker_as_unavailable`
- `cargo test runtime_protocol::tests::working_session_summaries_fall_back_to_worker_busy_state`
- `cargo test thread_state::tests::effective_busy_snapshot_falls_back_to_worker_state`
- `cargo test thread_state::tests::worker_interrupt_requested_maps_to_turn_finalizing`

## 與原計劃對齊度（backend worker 主線）

以 [app-server-ws-backend.md](app-server-ws-backend.md) 的 backend worker 主線目標估算，當前對齊度約為 **80%**。

已完成：

- workspace-scoped backend child worker 已可運作
- worker-first run authority 已進共享狀態判斷
- owner-managed `hcodex` launch endpoint 已下沉 worker
- 本地 runtime helper 已把 worker binary 納入 build 與 preflight 檢查

未完成：

- observer attach 仍依賴 `thread/resume` attach 語義，而非正式 subscribe contract
- backend plane 與 shared runtime semantics 的長期 API 邊界尚未收斂為獨立 contract
- busy authority 雖已 worker-first，但仍有部分衍生/兼容路徑待完全退出

## 下一步（只列 backend worker 主線）

1. 在 upstream 支持範圍內，將 observer attach 從 `thread/resume` 過渡到正式 subscribe 模型。
2. 將 worker-local API 與 shared runtime protocol 接縫顯式化，避免雙重 authority 長期並存。
3. 把 busy gate 剩餘衍生路徑收斂到 backend native truth + 明確錯誤語義。
