const appState = {
  setup: null,
  health: null,
  workspaces: [],
  archived: [],
  transcripts: {},
  sessions: {},
  workspacePanels: {},
  executionModeDrafts: {},
};

const WORKSPACE_PANEL_KEYS = ['advanced', 'launch', 'sessions', 'transcript'];

function escapeHtml(value) {
  return String(value ?? '')
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;');
}

function renderJson(id, data) {
  document.getElementById(id).textContent = JSON.stringify(data, null, 2);
}

function transcriptCache(threadKey) {
  if (!appState.transcripts[threadKey]) {
    appState.transcripts[threadKey] = {
      loaded: false,
      loading: false,
      error: null,
      entries: [],
    };
  }
  return appState.transcripts[threadKey];
}

function sessionCache(threadKey) {
  if (!appState.sessions[threadKey]) {
    appState.sessions[threadKey] = {
      loaded: false,
      loading: false,
      error: null,
      summaries: [],
      selectedSessionId: null,
      recordsBySessionId: {},
      recordLoadingBySessionId: {},
      recordErrorBySessionId: {},
    };
  }
  return appState.sessions[threadKey];
}

function workspacePanelState(threadKey) {
  if (!appState.workspacePanels[threadKey]) {
    appState.workspacePanels[threadKey] = {
      advanced: false,
      launch: false,
      sessions: false,
      transcript: false,
    };
  }
  return appState.workspacePanels[threadKey];
}

function panelOpenAttr(threadKey, panelKey) {
  return workspacePanelState(threadKey)[panelKey] ? 'open' : '';
}

function rememberWorkspacePanelStatesFromDom() {
  const root = document.getElementById('workspaces');
  if (!root) {
    return;
  }
  const panels = root.querySelectorAll('details[data-thread-key][data-panel-key]');
  for (const panel of panels) {
    const threadKey = panel.dataset.threadKey;
    const panelKey = panel.dataset.panelKey;
    if (!threadKey || !panelKey || !WORKSPACE_PANEL_KEYS.includes(panelKey)) {
      continue;
    }
    workspacePanelState(threadKey)[panelKey] = panel.open;
  }
}

function toneForStatus(value) {
  switch (value) {
    case 'running':
    case 'ready':
    case 'healthy':
    case 'active':
    case 'yes':
    case 'configured':
      return 'good';
    case 'degraded':
    case 'pending_adoption':
    case 'idle':
    case 'missing':
      return 'warn';
    case 'broken':
    case 'conflict':
    case 'unavailable':
    case 'stale':
    case 'invalid':
    case 'error':
      return 'bad';
    default:
      return 'neutral';
  }
}

function badge(label, value) {
  const tone = toneForStatus(value);
  return `<span class="badge badge-${tone}">${escapeHtml(label)}: ${escapeHtml(value)}</span>`;
}

function setBadge(id, label, value) {
  document.getElementById(id).className = `badge badge-${toneForStatus(value)}`;
  document.getElementById(id).textContent = label;
}

function metaItem(label, value) {
  return `
    <div class="meta-item">
      <span class="meta-label">${escapeHtml(label)}</span>
      <code>${escapeHtml(value ?? 'none')}</code>
    </div>
  `;
}

function matchesQuery(values, query) {
  if (!query) {
    return true;
  }
  const lowered = query.toLowerCase();
  return values.some(value => String(value ?? '').toLowerCase().includes(lowered));
}

function workspaceFilterQuery() {
  return document.getElementById('workspace-filter').value.trim().toLowerCase();
}

function workspaceByThreadKey(threadKey) {
  return (appState.workspaces || []).find(item => item.thread_key === threadKey) || null;
}

function workspaceIndexByCwd(workspaceCwd) {
  return (appState.workspaces || []).findIndex(item => item.workspace_cwd === workspaceCwd);
}

function archivedThreadIndexByKey(threadKey) {
  return (appState.archived || []).findIndex(item => item.thread_key === threadKey);
}

function upsertArrayItem(items, index, value) {
  if (index >= 0) {
    items[index] = value;
    return items;
  }
  items.push(value);
  return items;
}

function removeArrayItem(items, index) {
  if (index < 0) {
    return items;
  }
  items.splice(index, 1);
  return items;
}

function reconcileExecutionModeDrafts() {
  for (const [threadKey, draftValue] of Object.entries(appState.executionModeDrafts || {})) {
    const workspace = workspaceByThreadKey(threadKey);
    if (!workspace || workspace.workspace_execution_mode === draftValue) {
      delete appState.executionModeDrafts[threadKey];
    }
  }
}

function effectiveExecutionModeValue(item) {
  return appState.executionModeDrafts[item.thread_key] || item.workspace_execution_mode || 'full_auto';
}

function setExecutionModeDraft(threadKey, value) {
  const workspace = workspaceByThreadKey(threadKey);
  if (!workspace || !value || value === workspace.workspace_execution_mode) {
    delete appState.executionModeDrafts[threadKey];
    return;
  }
  appState.executionModeDrafts[threadKey] = value;
}

function shouldDeferWorkspaceCardsRender() {
  const root = document.getElementById('workspaces');
  if (!root || !root.children.length) {
    return false;
  }
  const active = document.activeElement;
  if (active?.id?.startsWith('mode-')) {
    return true;
  }
  return Object.keys(appState.executionModeDrafts || {}).length > 0;
}

const pendingObservabilityRefreshThreadKeys = new Set();
let renderScheduled = false;
let observabilityRefreshScheduled = false;
let fullRefreshScheduled = false;
let initialSnapshotLoaded = false;

function markObservabilityRefresh(threadKey) {
  if (!threadKey) {
    return;
  }
  if (transcriptCache(threadKey).loaded || sessionCache(threadKey).loaded) {
    pendingObservabilityRefreshThreadKeys.add(threadKey);
  }
}

async function refreshPendingObservability() {
  const threadKeys = [...pendingObservabilityRefreshThreadKeys];
  pendingObservabilityRefreshThreadKeys.clear();
  await Promise.all(threadKeys.flatMap(threadKey => {
    const tasks = [];
    if (transcriptCache(threadKey).loaded) {
      tasks.push(loadTranscript(threadKey, false));
    }
    if (sessionCache(threadKey).loaded) {
      tasks.push(loadWorkingSessions(threadKey, false));
    }
    return tasks;
  }));
}

function scheduleRender() {
  if (renderScheduled) {
    return;
  }
  renderScheduled = true;
  window.setTimeout(() => {
    renderScheduled = false;
    reconcileExecutionModeDrafts();
    renderAll();
  }, 50);
}

function scheduleObservabilityRefresh() {
  if (observabilityRefreshScheduled || !pendingObservabilityRefreshThreadKeys.size) {
    return;
  }
  observabilityRefreshScheduled = true;
  window.setTimeout(async () => {
    observabilityRefreshScheduled = false;
    await refreshPendingObservability();
  }, 50);
}

function scheduleFullRefresh() {
  if (fullRefreshScheduled) {
    return;
  }
  fullRefreshScheduled = true;
  window.setTimeout(async () => {
    fullRefreshScheduled = false;
    await refresh();
  }, 150);
}

function applyRuntimeEvent(payload) {
  if (!initialSnapshotLoaded) {
    scheduleFullRefresh();
    return;
  }
  if (!payload?.kind) {
    scheduleFullRefresh();
    return;
  }

  let shouldRender = false;
  switch (payload.kind) {
    case 'setup_changed':
      if (payload.op === 'upsert' && payload.current) {
        appState.setup = payload.current;
        shouldRender = true;
      }
      break;
    case 'runtime_health_changed':
      if (payload.op === 'upsert' && payload.current) {
        appState.health = payload.current;
        shouldRender = true;
      }
      break;
    case 'managed_codex_changed':
      if (payload.op === 'remove') {
        appState.health = { ...(appState.health || {}), managed_codex: null };
        shouldRender = true;
        break;
      }
      if (payload.op === 'upsert' && payload.current) {
        appState.health = { ...(appState.health || {}), managed_codex: payload.current };
        shouldRender = true;
      }
      break;
    case 'workspace_state_changed': {
      const key = payload.key;
      const existingIndex = typeof key === 'string' ? workspaceIndexByCwd(key) : -1;
      const previousThreadKey = existingIndex >= 0 ? appState.workspaces[existingIndex]?.thread_key : null;
      if (payload.op === 'remove') {
        removeArrayItem(appState.workspaces, existingIndex);
        markObservabilityRefresh(previousThreadKey);
        shouldRender = true;
        break;
      }
      if (payload.op === 'upsert' && payload.current) {
        upsertArrayItem(appState.workspaces, existingIndex, payload.current);
        markObservabilityRefresh(payload.current.thread_key || previousThreadKey);
        shouldRender = true;
      }
      break;
    }
    case 'archived_thread_changed': {
      const key = payload.key;
      const existingIndex = typeof key === 'string' ? archivedThreadIndexByKey(key) : -1;
      if (payload.op === 'remove') {
        removeArrayItem(appState.archived, existingIndex);
        shouldRender = true;
        break;
      }
      if (payload.op === 'upsert' && payload.current) {
        upsertArrayItem(appState.archived, existingIndex, payload.current);
        shouldRender = true;
      }
      break;
    }
    case 'thread_state_changed':
      markObservabilityRefresh(payload.key || payload.current?.thread_key || null);
      break;
    case 'error':
      scheduleFullRefresh();
      return;
    default:
      scheduleFullRefresh();
      return;
  }

  if (shouldRender) {
    scheduleRender();
  }
  scheduleObservabilityRefresh();
}

function renderSetupCard(setup) {
  const setupLabel = setup.telegram_token_configured ? 'configured' : 'missing';
  setBadge('setup-pill', `Setup ${setupLabel}`, setupLabel);
  document.getElementById('authorized-user-ids').value = (setup.authorized_user_ids || []).join(',');
  document.getElementById('setup-runtime-note').textContent = setup.control_chat_ready
    ? `Control chat is ready: ${setup.control_chat_id}`
    : 'Control chat is not ready. Send /start to the bot from the target Telegram chat first.';
  renderJson('setup', setup);
}

function renderHealthSummary(health) {
  const root = document.getElementById('health-summary');
  const owner = health.runtime_owner || {};
  const managedCodex = health.managed_codex || {};
  const metrics = [
    ['Global App Server', health.app_server_status || 'unknown'],
    ['Global hcodex Ingress', health.hcodex_ingress_status || 'unknown'],
    ['Global Runtime', health.runtime_readiness || 'unknown'],
    ['Owner State', owner.state || 'inactive'],
    ['Owner Last Success', owner.last_successful_reconcile_at || 'never'],
    ['Running Workspaces', String(health.running_workspaces ?? 0)],
    ['Ready Workspaces', String(health.ready_workspaces ?? 0)],
    ['Degraded Workspaces', String(health.degraded_workspaces ?? 0)],
    ['Unavailable Workspaces', String(health.unavailable_workspaces ?? 0)],
    ['Broken Threads', String(health.broken_threads ?? 0)],
    ['Conflicted Workspaces', String(health.conflicted_workspaces ?? 0)],
    ['Managed Codex Source', managedCodex.source || 'unknown'],
    ['Managed Codex Version', managedCodex.version || 'unknown'],
    ['Managed Codex Ready', managedCodex.binary_ready ? 'yes' : 'no'],
  ];
  root.innerHTML = metrics.map(([label, value]) => `
    <div class="metric">
      <span class="metric-label">${escapeHtml(label)}</span>
      <span class="metric-value"><code>${escapeHtml(value)}</code></span>
    </div>
  `).join('');

  setBadge('owner-pill', `Owner ${owner.state || 'inactive'}`, owner.state || 'inactive');
  setBadge(
    'managed-codex-pill',
    `Codex ${managedCodex.binary_ready ? 'ready' : 'unavailable'}`,
    managedCodex.binary_ready ? 'ready' : 'unavailable',
  );

  const hint = document.getElementById('runtime-recovery-hint');
  if (health.recovery_hint) {
    hint.style.display = 'block';
    hint.textContent = health.recovery_hint;
  } else {
    hint.style.display = 'none';
    hint.textContent = '';
  }

  document.getElementById('runtime-summary-note').textContent =
    `Global summary is aggregated from managed workspaces. Owner report: scanned ${owner.last_report?.scanned_workspaces ?? 0}, ensured ${owner.last_report?.ensured_workspaces ?? 0} workspaces, ensured ${owner.last_report?.ensured_proxies ?? 0} proxies.`;

  document.getElementById('managed-codex-source').value = managedCodex.source || 'brew';
  document.getElementById('managed-codex-source-repo').value =
    managedCodex.build_defaults?.source_repo || '';
  document.getElementById('managed-codex-source-rs-dir').value =
    managedCodex.build_defaults?.source_rs_dir || '';
  document.getElementById('managed-codex-build-profile').value =
    managedCodex.build_defaults?.build_profile || 'dev';

  renderJson('health', health);
}

function workspacePrimaryLabel(item) {
  const workspace = String(item.workspace_cwd || '').trim();
  if (!workspace) {
    return item.title || item.thread_key || 'Workspace';
  }
  const segments = workspace.split('/').filter(Boolean);
  return segments[segments.length - 1] || workspace;
}

function workspaceSecondaryLabel(item) {
  if (item.title && item.title !== workspacePrimaryLabel(item)) {
    return item.title;
  }
  return null;
}

function transcriptEntriesForDelivery(entries, delivery) {
  return (entries || []).filter(entry => delivery === 'all' || entry.delivery === delivery);
}

function formatTranscriptEntry(entry) {
  const phase = entry.phase ? ` · ${entry.phase}` : '';
  const origin = entry.origin ? ` · ${entry.origin}` : '';
  return `<div class="transcript-entry">
    <div class="transcript-meta">${escapeHtml(entry.timestamp || 'unknown')}${escapeHtml(phase)}${escapeHtml(origin)}</div>
    <div>${escapeHtml(entry.text || '')}</div>
  </div>`;
}

function renderTranscriptSection(entries, delivery, emptyLabel) {
  const filtered = transcriptEntriesForDelivery(entries, delivery);
  if (!filtered.length) {
    return `<p class="muted">${escapeHtml(emptyLabel)}</p>`;
  }
  return `<div class="transcript-list">${filtered.map(formatTranscriptEntry).join('')}</div>`;
}

function sessionRunStatus(summary) {
  return summary?.run_status || 'idle';
}

function sessionLabel(summary) {
  return summary?.session_id || 'unknown-session';
}

function compactOriginList(summary) {
  const items = summary?.origins_seen || [];
  return items.length ? items.join(', ') : 'none';
}

function formatSessionSummary(summary, threadKey) {
  const selected = sessionCache(threadKey).selectedSessionId === summary.session_id;
  return `<div class="transcript-entry ${selected ? 'session-entry-selected' : ''}">
    <div class="transcript-meta">
      <strong>${escapeHtml(sessionLabel(summary))}</strong>
      · ${escapeHtml(summary.updated_at || 'unknown')}
      · ${escapeHtml(sessionRunStatus(summary))}
    </div>
    <div class="session-inline-meta">
      <span>origins: <code>${escapeHtml(compactOriginList(summary))}</code></span>
      <span>records: <code>${escapeHtml(summary.record_count ?? 0)}</code></span>
      <span>tools: <code>${escapeHtml(summary.tool_use_count ?? 0)}</code></span>
      <span>final: <code>${escapeHtml(summary.has_final_reply ? 'yes' : 'no')}</code></span>
    </div>
    ${summary.last_error ? `<div class="hint">Last error: ${escapeHtml(summary.last_error)}</div>` : ''}
    <div class="toolbar">
      <button class="secondary" onclick="selectWorkingSession('${threadKey}', '${summary.session_id}')">View Records</button>
    </div>
  </div>`;
}

function formatSessionRecord(record) {
  const kind = record.kind || 'unknown';
  const meta = [
    record.timestamp || 'unknown',
    kind,
    record.origin || 'n/a',
    record.delivery || 'n/a',
    record.phase || 'n/a',
    record.source_ref || 'n/a',
  ];
  return `<div class="transcript-entry">
    <div class="transcript-meta">${meta.map(value => escapeHtml(value)).join(' · ')}</div>
    <div>${escapeHtml(record.text || '')}</div>
  </div>`;
}

function renderWorkingSessionRecords(threadKey, sessionId) {
  const cache = sessionCache(threadKey);
  if (!sessionId) {
    return '<p class="muted">Select a session to inspect its timeline.</p>';
  }
  if (cache.recordLoadingBySessionId[sessionId] && !cache.recordsBySessionId[sessionId]) {
    return '<p class="muted">Loading session records…</p>';
  }
  const error = cache.recordErrorBySessionId[sessionId];
  if (error) {
    return `<p class="hint">${escapeHtml(error)}</p>`;
  }
  const records = cache.recordsBySessionId[sessionId] || [];
  if (!records.length) {
    return '<p class="muted">No session records yet.</p>';
  }
  return `<div class="transcript-list">${records.map(formatSessionRecord).join('')}</div>`;
}

function renderWorkingSessions(threadKey) {
  const cache = sessionCache(threadKey);
  if (cache.loading && !cache.loaded) {
    return '<p class="muted">Loading sessions…</p>';
  }
  if (cache.error) {
    return `<p class="hint">${escapeHtml(cache.error)}</p>`;
  }
  if (!cache.loaded) {
    return `<div class="toolbar"><button class="secondary" onclick="loadWorkingSessions('${threadKey}', true)">Load Sessions</button></div>`;
  }
  if (!cache.summaries.length) {
    return '<p class="muted">No sessions available for this workspace yet.</p>';
  }
  return `
    <div class="toolbar">
      <button class="secondary" onclick="loadWorkingSessions('${threadKey}', true)">Refresh Sessions</button>
      <span class="muted">Session summaries and records are derived from transcript mirror and runtime session status.</span>
    </div>
    <div class="subsection">
      <div class="section-heading compact">
        <h3>Sessions</h3>
      </div>
      <div class="transcript-list">${cache.summaries.map(summary => formatSessionSummary(summary, threadKey)).join('')}</div>
    </div>
    <div class="subsection">
      <div class="section-heading compact">
        <h3>Session Records</h3>
      </div>
      ${renderWorkingSessionRecords(threadKey, cache.selectedSessionId)}
    </div>
  `;
}

function configureAddWorkspaceCard(setup) {
  const button = document.getElementById('add-workspace-button');
  const status = document.getElementById('add-workspace-status');
  if (setup.native_workspace_picker_available) {
    if (setup.telegram_polling_state !== 'active') {
      button.disabled = true;
      status.textContent = 'Telegram bot is not active yet. Save setup or wait for desktop runtime to reconnect polling.';
      return;
    }
    if (!setup.control_chat_ready) {
      button.disabled = true;
      status.textContent = 'Send /start to the bot from the target Telegram chat first. Add Workspace creates a Telegram topic for that workspace.';
      return;
    }
    button.disabled = false;
    status.textContent = 'Desktop runtime will open the system folder picker and then create the workspace thread.';
    return;
  }
  button.disabled = true;
  status.textContent = 'Add Workspace requires threadbridge_desktop. Headless runtime does not expose the native folder picker.';
}

function renderWorkspaceCards(items) {
  const root = document.getElementById('workspaces');
  rememberWorkspacePanelStatesFromDom();
  const query = workspaceFilterQuery();
  const filtered = items.filter(item => matchesQuery([
    item.title,
    item.workspace_cwd,
    item.thread_key,
    item.current_codex_thread_id,
    item.tui_active_codex_thread_id,
  ], query));
  document.getElementById('workspaces-count').textContent = `${filtered.length}/${items.length}`;
  if (!filtered.length) {
    root.innerHTML = '<p class="muted">No managed workspaces match this filter.</p>';
    return;
  }
  root.innerHTML = filtered.map(item => {
    const selectedExecutionMode = effectiveExecutionModeValue(item);
    return `
    <article class="entity-card">
      <div class="entity-head">
        <div>
          <div class="entity-title">${escapeHtml(workspacePrimaryLabel(item))}</div>
          ${workspaceSecondaryLabel(item) ? `<div class="muted">${escapeHtml(workspaceSecondaryLabel(item))}</div>` : ''}
          <div class="entity-path"><code>${escapeHtml(item.workspace_cwd)}</code></div>
        </div>
        <div class="badge-row">
          ${badge('mode', item.workspace_execution_mode || 'full_auto')}
          ${badge('session-mode', item.current_execution_mode || 'unknown')}
          ${badge('binding', item.binding_status)}
          ${badge('run', item.run_status)}
          ${item.conflict ? badge('conflict', 'conflict') : ''}
          ${badge('app', item.app_server_status)}
          ${badge('ingress', item.hcodex_ingress_status)}
          ${badge('runtime', item.runtime_readiness)}
        </div>
      </div>

      ${item.recovery_hint ? `<div class="hint">${escapeHtml(item.recovery_hint)}</div>` : ''}

      ${item.conflict ? '<div class="status-note">Workspace binding conflict detected. Tray launch stays disabled until only one active binding remains.</div>' : ''}
      ${item.mode_drift ? `<div class="status-note">Current session mode differs from workspace mode. The next Telegram turn or hcodex resume will re-apply <code>${escapeHtml(item.workspace_execution_mode)}</code>.</div>` : ''}

      <div class="toolbar">
        <label class="muted" for="mode-${item.thread_key}">Execution Mode</label>
        <select id="mode-${item.thread_key}" ${item.conflict ? 'disabled' : ''} onchange="setExecutionModeDraft('${item.thread_key}', this.value)">
          <option value="full_auto" ${selectedExecutionMode === 'full_auto' ? 'selected' : ''}>full_auto</option>
          <option value="yolo" ${selectedExecutionMode === 'yolo' ? 'selected' : ''}>yolo</option>
        </select>
        <button class="secondary" ${item.conflict ? 'disabled' : ''} onclick="updateExecutionMode('${item.thread_key}')">Save Mode</button>
      </div>

      <div class="stack">
        <div class="meta-label">Recent Sessions</div>
        <div class="session-row">
          ${(item.recent_codex_sessions || []).map(session => `
            <span class="session-pill">
              <code>${escapeHtml(session.session_id)}</code>
              <span class="muted">${escapeHtml(session.execution_mode || 'unknown')}</span>
              <button class="secondary" ${item.conflict ? 'disabled' : ''} onclick="launchHcodexResumeWithSession('${item.thread_key}', '${session.session_id}')">Resume</button>
            </span>
          `).join('') || '<span class="muted">No recent sessions to resume.</span>'}
        </div>
      </div>

      <div class="actions-grid">
        <button class="secondary" onclick="openWorkspace('${item.thread_key}')">Open Workspace</button>
        <button ${item.conflict ? 'disabled' : ''} onclick="startFreshSession('${item.thread_key}')">Start Fresh Session</button>
        <button ${item.conflict ? 'disabled' : ''} onclick="launchHcodexNew('${item.thread_key}')">Launch Local Session (new)</button>
        <button ${item.conflict || !item.current_codex_thread_id ? 'disabled' : ''} onclick="launchHcodexContinueCurrent('${item.thread_key}')">Launch Local Session (continue_current)</button>
        <button class="secondary" ${item.conflict ? 'disabled' : ''} onclick="repairContinuity('${item.thread_key}', '${item.binding_status}', ${item.tui_session_adoption_pending ? 'true' : 'false'})">${item.tui_session_adoption_pending ? 'Adopt TUI' : 'Repair Session'}</button>
        <button class="secondary" onclick="repairRuntime('${item.thread_key}')">Repair Runtime</button>
        <button ${item.conflict ? 'disabled' : ''} onclick="showLaunchConfig('${item.thread_key}')">Show Launch Commands</button>
        <button onclick='archiveThread(${JSON.stringify(item.thread_key)}, ${JSON.stringify(workspacePrimaryLabel(item))})'>Archive</button>
      </div>

      <div class="toolbar">
        <input id="resume-${item.thread_key}" type="text" placeholder="session id to resume" />
        <button class="secondary" ${item.conflict ? 'disabled' : ''} onclick="launchHcodexResume('${item.thread_key}')">Launch Local Session (resume)</button>
      </div>

      <details class="raw-panel" data-thread-key="${item.thread_key}" data-panel-key="advanced" ${panelOpenAttr(item.thread_key, 'advanced')}>
        <summary>Advanced Workspace Details</summary>
        <div class="meta-grid">
          ${metaItem('thread_key', item.thread_key || 'none')}
          ${metaItem('workspace_execution_mode', item.workspace_execution_mode || 'full_auto')}
          ${metaItem('current_execution_mode', item.current_execution_mode || 'unknown')}
          ${metaItem('current_approval_policy', item.current_approval_policy || 'unknown')}
          ${metaItem('current_sandbox_policy', item.current_sandbox_policy || 'unknown')}
          ${metaItem('mode_drift', item.mode_drift ? 'yes' : 'no')}
          ${metaItem('runtime_source', item.runtime_health_source || 'unknown')}
          ${metaItem('owner_checked_at', item.heartbeat_last_checked_at || 'n/a')}
          ${metaItem('owner_last_error', item.heartbeat_last_error || 'none')}
          ${metaItem('session_broken_reason', item.session_broken_reason || 'none')}
          ${metaItem('current_codex_thread_id', item.current_codex_thread_id || 'none')}
          ${metaItem('tui_active_codex_thread_id', item.tui_active_codex_thread_id || 'none')}
          ${metaItem('adoption_pending', item.tui_session_adoption_pending ? 'yes' : 'no')}
          ${metaItem('last_used_at', item.last_used_at || 'unknown')}
          ${metaItem('hcodex_path', item.hcodex_path)}
        </div>
        ${item.tui_active_codex_thread_id ? `<div class="toolbar"><button class="secondary" onclick="rejectTuiSession('${item.thread_key}')">Keep Original Binding</button></div>` : ''}
      </details>

      <details id="launch-wrap-${item.thread_key}" class="raw-panel" data-thread-key="${item.thread_key}" data-panel-key="launch" ${panelOpenAttr(item.thread_key, 'launch')}>
        <summary>Launch Output</summary>
        <pre id="launch-${item.thread_key}">No launch output yet.</pre>
      </details>

      <details class="raw-panel transcript-panel" data-thread-key="${item.thread_key}" data-panel-key="sessions" ${panelOpenAttr(item.thread_key, 'sessions')}>
        <summary>Sessions</summary>
        <div id="sessions-${item.thread_key}" class="stack">
          ${renderWorkingSessions(item.thread_key)}
        </div>
      </details>

      <details class="raw-panel transcript-panel" data-thread-key="${item.thread_key}" data-panel-key="transcript" ${panelOpenAttr(item.thread_key, 'transcript')}>
        <summary>Transcript</summary>
        <div class="toolbar">
          <button class="secondary" onclick="loadTranscript('${item.thread_key}', true)">Refresh Transcript</button>
          <span class="muted">Latest 40 transcript mirror entries.</span>
        </div>
        <div id="transcript-${item.thread_key}" class="stack">
          ${renderWorkspaceTranscript(item.thread_key)}
        </div>
      </details>
    </article>
  `;
  }).join('');
}

function renderWorkspaceTranscript(threadKey) {
  const cache = transcriptCache(threadKey);
  if (cache.loading && !cache.loaded) {
    return '<p class="muted">Loading transcript…</p>';
  }
  if (cache.error) {
    return `<p class="hint">${escapeHtml(cache.error)}</p>`;
  }
  if (!cache.loaded) {
    return `<div class="toolbar"><button class="secondary" onclick="loadTranscript('${threadKey}', true)">Load Transcript</button></div>`;
  }
  return `
    <div class="subsection">
      <div class="section-heading compact">
        <h3>Process Transcript</h3>
      </div>
      ${renderTranscriptSection(cache.entries, 'process', 'No process transcript entries yet.')}
    </div>
    <div class="subsection">
      <div class="section-heading compact">
        <h3>Final Transcript</h3>
      </div>
      ${renderTranscriptSection(cache.entries, 'final', 'No final transcript entries yet.')}
    </div>
  `;
}

function renderArchivedThreads(items) {
  const root = document.getElementById('archived');
  document.getElementById('archived-count').textContent = String(items.length);
  if (!items.length) {
    root.innerHTML = '<p class="muted">No archived workspaces.</p>';
    return;
  }
  root.innerHTML = items.map(item => `
    <article class="entity-card">
      <div class="entity-head">
        <div>
          <div class="entity-title">${escapeHtml(workspacePrimaryLabel(item))}</div>
          ${workspaceSecondaryLabel(item) ? `<div class="muted">${escapeHtml(workspaceSecondaryLabel(item))}</div>` : ''}
          <div class="entity-path"><code>${escapeHtml(item.workspace_cwd || item.thread_key)}</code></div>
        </div>
        <div class="badge-row">
          ${badge('archived', item.archived_at ? 'yes' : 'no')}
        </div>
      </div>
      <div class="meta-grid">
        ${metaItem('workspace', item.workspace_cwd || 'unbound')}
        ${metaItem('archived_at', item.archived_at || 'unknown')}
        ${metaItem('previous_topics', (item.previous_message_thread_ids || []).join(', ') || 'none')}
      </div>
      <div class="actions-grid">
        <button onclick='restoreThread(${JSON.stringify(item.thread_key)}, ${JSON.stringify(item.title || item.thread_key)})'>Restore</button>
      </div>
    </article>
  `).join('');
}

function renderAll() {
  renderSetupCard(appState.setup);
  renderHealthSummary(appState.health);
  configureAddWorkspaceCard(appState.setup);
  if (!shouldDeferWorkspaceCardsRender()) {
    renderWorkspaceCards(appState.workspaces);
  }
  renderArchivedThreads(appState.archived);
}

async function refresh() {
  const [setup, health, workspaces, archived] = await Promise.all([
    fetch('/api/setup').then(r => r.json()),
    fetch('/api/runtime-health').then(r => r.json()),
    fetch('/api/workspaces').then(r => r.json()),
    fetch('/api/archived-threads').then(r => r.json()),
  ]);
  appState.setup = setup;
  appState.health = health;
  appState.workspaces = workspaces;
  appState.archived = archived;
  initialSnapshotLoaded = true;
  reconcileExecutionModeDrafts();
  renderAll();
  await Promise.all([refreshLoadedTranscripts(), refreshLoadedSessions()]);
}

function openLaunchOutput(threadKey, data) {
  const details = document.getElementById(`launch-wrap-${threadKey}`);
  const target = document.getElementById(`launch-${threadKey}`);
  workspacePanelState(threadKey).launch = true;
  if (details) {
    details.open = true;
  }
  if (target) {
    target.textContent = JSON.stringify(data, null, 2);
  }
}

async function showLaunchConfig(threadKey) {
  const response = await fetch(`/api/workspaces/${threadKey}/launch-config`);
  const data = await response.json();
  openLaunchOutput(threadKey, data);
}

async function postRuntimeControlAction(threadKey, payload, failureText) {
  const response = await fetch(`/api/threads/${threadKey}/actions`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(payload),
  });
  const data = await response.json();
  if (!response.ok) {
    alert(data.error || failureText);
    return null;
  }
  openLaunchOutput(threadKey, data);
  await refresh();
  return data;
}

async function updateExecutionMode(threadKey) {
  const select = document.getElementById(`mode-${threadKey}`);
  const executionMode = select?.value;
  if (!executionMode) {
    alert('Pick an execution mode first');
    return;
  }
  const data = await postRuntimeControlAction(
    threadKey,
    { action: 'set_workspace_execution_mode', execution_mode: executionMode },
    'Execution mode update failed',
  );
  if (!data) {
    return;
  }
  delete appState.executionModeDrafts[threadKey];
}

async function launchHcodexNew(threadKey) {
  await postRuntimeControlAction(
    threadKey,
    { action: 'launch_local_session', target: 'new' },
    'Launch failed',
  );
}

async function launchHcodexContinueCurrent(threadKey) {
  await postRuntimeControlAction(
    threadKey,
    { action: 'launch_local_session', target: 'continue_current' },
    'Launch failed',
  );
}

async function launchHcodexResume(threadKey) {
  const sessionId = document.getElementById(`resume-${threadKey}`).value.trim();
  if (!sessionId) {
    alert('Enter a session id first');
    return;
  }
  await launchHcodexResumeWithSession(threadKey, sessionId);
}

async function launchHcodexResumeWithSession(threadKey, sessionId) {
  await postRuntimeControlAction(
    threadKey,
    { action: 'launch_local_session', target: 'resume', session_id: sessionId },
    'Launch failed',
  );
}

async function startFreshSession(threadKey) {
  await postRuntimeControlAction(
    threadKey,
    { action: 'start_fresh_session' },
    'Start fresh session failed',
  );
}

async function repairSessionBinding(threadKey) {
  await postRuntimeControlAction(
    threadKey,
    { action: 'repair_session_binding' },
    'Session repair failed',
  );
}

async function repairContinuity(threadKey, bindingStatus, adoptionPending) {
  if (adoptionPending) {
    await adoptTuiSession(threadKey);
    return;
  }
  await repairSessionBinding(threadKey);
}

async function loadTranscript(threadKey, userInitiated = false) {
  const cache = transcriptCache(threadKey);
  if (cache.loading) {
    return;
  }
  cache.loading = true;
  cache.error = null;
  if (userInitiated) {
    renderWorkspaceTranscriptIntoDom(threadKey);
  }
  try {
    const response = await fetch(`/api/threads/${threadKey}/transcript?delivery=all&limit=40`);
    const data = await response.json();
    if (!response.ok) {
      cache.error = data.error || 'Transcript fetch failed';
      cache.entries = [];
      cache.loaded = false;
      return;
    }
    cache.entries = data;
    cache.loaded = true;
  } catch (error) {
    cache.error = error instanceof Error ? error.message : 'Transcript fetch failed';
    cache.entries = [];
    cache.loaded = false;
  } finally {
    cache.loading = false;
    renderWorkspaceTranscriptIntoDom(threadKey);
  }
}

function renderWorkspaceTranscriptIntoDom(threadKey) {
  const target = document.getElementById(`transcript-${threadKey}`);
  if (!target) {
    return;
  }
  target.innerHTML = renderWorkspaceTranscript(threadKey);
}

function renderWorkingSessionsIntoDom(threadKey) {
  const target = document.getElementById(`sessions-${threadKey}`);
  if (!target) {
    return;
  }
  target.innerHTML = renderWorkingSessions(threadKey);
}

async function refreshLoadedTranscripts() {
  const threadKeys = Object.entries(appState.transcripts)
    .filter(([, cache]) => cache.loaded)
    .map(([threadKey]) => threadKey);
  await Promise.all(threadKeys.map(threadKey => loadTranscript(threadKey, false)));
}

async function loadWorkingSessions(threadKey, userInitiated = false) {
  const cache = sessionCache(threadKey);
  if (cache.loading) {
    return;
  }
  cache.loading = true;
  cache.error = null;
  if (userInitiated) {
    renderWorkingSessionsIntoDom(threadKey);
  }
  try {
    const response = await fetch(`/api/threads/${threadKey}/sessions`);
    const data = await response.json();
    if (!response.ok) {
      cache.error = data.error || 'Session fetch failed';
      cache.summaries = [];
      cache.loaded = false;
      return;
    }
    cache.summaries = data;
    cache.loaded = true;
    const sessionIds = new Set(cache.summaries.map(item => item.session_id));
    if (!cache.selectedSessionId || !sessionIds.has(cache.selectedSessionId)) {
      cache.selectedSessionId = cache.summaries[0]?.session_id || null;
    }
    if (cache.selectedSessionId) {
      await loadWorkingSessionRecords(threadKey, cache.selectedSessionId, false);
    }
  } catch (error) {
    cache.error = error instanceof Error ? error.message : 'Session fetch failed';
    cache.summaries = [];
    cache.loaded = false;
  } finally {
    cache.loading = false;
    renderWorkingSessionsIntoDom(threadKey);
  }
}

async function loadWorkingSessionRecords(threadKey, sessionId, userInitiated = false) {
  const cache = sessionCache(threadKey);
  if (!sessionId || cache.recordLoadingBySessionId[sessionId]) {
    return;
  }
  cache.recordLoadingBySessionId[sessionId] = true;
  delete cache.recordErrorBySessionId[sessionId];
  if (userInitiated) {
    renderWorkingSessionsIntoDom(threadKey);
  }
  try {
    const response = await fetch(`/api/threads/${threadKey}/sessions/${encodeURIComponent(sessionId)}/records`);
    const data = await response.json();
    if (!response.ok) {
      cache.recordErrorBySessionId[sessionId] = data.error || 'Session records fetch failed';
      cache.recordsBySessionId[sessionId] = [];
      return;
    }
    cache.recordsBySessionId[sessionId] = data;
  } catch (error) {
    cache.recordErrorBySessionId[sessionId] = error instanceof Error ? error.message : 'Session records fetch failed';
    cache.recordsBySessionId[sessionId] = [];
  } finally {
    cache.recordLoadingBySessionId[sessionId] = false;
    renderWorkingSessionsIntoDom(threadKey);
  }
}

async function selectWorkingSession(threadKey, sessionId) {
  const cache = sessionCache(threadKey);
  cache.selectedSessionId = sessionId;
  renderWorkingSessionsIntoDom(threadKey);
  await loadWorkingSessionRecords(threadKey, sessionId, true);
}

async function refreshLoadedSessions() {
  const threadKeys = Object.entries(appState.sessions)
    .filter(([, cache]) => cache.loaded)
    .map(([threadKey]) => threadKey);
  await Promise.all(threadKeys.map(threadKey => loadWorkingSessions(threadKey, false)));
}

async function openWorkspace(threadKey) {
  const response = await fetch(`/api/workspaces/${threadKey}/open`, { method: 'POST' });
  const data = await response.json();
  if (!response.ok) {
    alert(data.error || 'Open workspace failed');
  }
}

async function repairRuntime(threadKey) {
  const response = await fetch(`/api/workspaces/${threadKey}/repair-runtime`, { method: 'POST' });
  const data = await response.json();
  if (!response.ok) {
    alert(data.error || 'Runtime repair failed');
    return;
  }
  await refresh();
}

async function adoptTuiSession(threadKey) {
  const response = await fetch(`/api/threads/${threadKey}/adopt-tui`, { method: 'POST' });
  const data = await response.json();
  if (!response.ok) {
    alert(data.error || 'Adopt TUI failed');
    return;
  }
  await refresh();
}

async function rejectTuiSession(threadKey) {
  const response = await fetch(`/api/threads/${threadKey}/reject-tui`, { method: 'POST' });
  const data = await response.json();
  if (!response.ok) {
    alert(data.error || 'Reject TUI failed');
    return;
  }
  await refresh();
}

async function archiveThread(threadKey, label) {
  if (!window.confirm(`Archive workspace "${label}"? This only changes local threadBridge state and Telegram topic state.`)) {
    return;
  }
  const response = await fetch(`/api/threads/${threadKey}/archive`, { method: 'POST' });
  const data = await response.json();
  if (!response.ok) {
    alert(data.error || 'Archive failed');
    return;
  }
  await refresh();
}

async function restoreThread(threadKey, label) {
  if (!window.confirm(`Restore archived workspace "${label}"? This restores local metadata and Telegram topic state only.`)) {
    return;
  }
  const response = await fetch(`/api/threads/${threadKey}/restore`, { method: 'POST' });
  const data = await response.json();
  if (!response.ok) {
    alert(data.error || 'Restore failed');
    return;
  }
  await refresh();
}

async function pickAndAddWorkspace() {
  const status = document.getElementById('add-workspace-status');
  status.textContent = 'Waiting for workspace selection...';
  const response = await fetch('/api/workspaces/pick-and-add', {
    method: 'POST',
  });
  const data = await response.json();
  if (!response.ok) {
    status.textContent = data.error || 'Add workspace failed';
    return;
  }
  if (data.cancelled) {
    status.textContent = 'Workspace selection cancelled.';
    return;
  }
  if (data.probe_report) {
    status.textContent = data.probe_report;
    if (data.thread_key) {
      await refresh();
    }
    return;
  }
  const label = workspacePrimaryLabel(data);
  status.textContent = data.created
    ? `Added workspace: ${label}`
    : `Workspace already managed: ${label}`;
  await refresh();
}

async function updateManagedCodexPreference() {
  const status = document.getElementById('managed-codex-status');
  status.textContent = 'Applying...';
  const response = await fetch('/api/managed-codex/preference', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ source: document.getElementById('managed-codex-source').value }),
  });
  const data = await response.json();
  if (!response.ok) {
    status.textContent = data.error || 'Apply failed';
    return;
  }
  status.textContent = `Applied. Synced ${data.synced_workspaces} workspaces.`;
  await refresh();
}

async function refreshManagedCodexCache() {
  const status = document.getElementById('managed-codex-status');
  status.textContent = 'Refreshing cache...';
  const response = await fetch('/api/managed-codex/refresh-cache', {
    method: 'POST',
  });
  const data = await response.json();
  if (!response.ok) {
    status.textContent = data.error || 'Refresh failed';
    return;
  }
  status.textContent = `Cache refreshed: ${data.version || data.binary_path}`;
  await refresh();
}

async function reconcileRuntimeOwner() {
  const status = document.getElementById('managed-codex-status');
  status.textContent = 'Reconciling runtime owner...';
  const response = await fetch('/api/runtime-owner/reconcile', {
    method: 'POST',
  });
  const data = await response.json();
  if (!response.ok) {
    status.textContent = data.error || 'Runtime owner reconcile failed';
    return;
  }
  status.textContent =
    `Reconciled ${data.report?.scanned_workspaces ?? 0} workspaces. ` +
    `Owner state: ${data.status?.state || 'unknown'}.`;
  await refresh();
}

async function buildManagedCodexSource() {
  const status = document.getElementById('managed-codex-status');
  status.textContent = 'Building source Codex...';
  const response = await fetch('/api/managed-codex/build-source', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      source_repo: document.getElementById('managed-codex-source-repo').value.trim() || null,
      source_rs_dir: document.getElementById('managed-codex-source-rs-dir').value.trim() || null,
      build_profile: document.getElementById('managed-codex-build-profile').value,
    }),
  });
  const data = await response.json();
  if (!response.ok) {
    status.textContent = data.error || 'Build failed';
    return;
  }
  status.textContent = `Source build ready: ${data.version || data.binary_path}`;
  await refresh();
}

async function saveManagedCodexBuildDefaults() {
  const status = document.getElementById('managed-codex-status');
  status.textContent = 'Saving build defaults...';
  const response = await fetch('/api/managed-codex/build-defaults', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      source_repo: document.getElementById('managed-codex-source-repo').value.trim(),
      source_rs_dir: document.getElementById('managed-codex-source-rs-dir').value.trim(),
      build_profile: document.getElementById('managed-codex-build-profile').value,
    }),
  });
  const data = await response.json();
  if (!response.ok) {
    status.textContent = data.error || 'Save defaults failed';
    return;
  }
  status.textContent = `Build defaults saved: ${data.build_defaults.build_profile}`;
  await refresh();
}

document.getElementById('setup-form').addEventListener('submit', async event => {
  event.preventDefault();
  const status = document.getElementById('setup-status');
  status.textContent = 'Saving...';
  const payload = {
    telegram_token: document.getElementById('telegram-token').value,
    authorized_user_ids: document.getElementById('authorized-user-ids').value
      .split(',')
      .map(x => x.trim())
      .filter(Boolean)
      .map(x => Number(x)),
  };
  const response = await fetch('/api/setup/telegram', {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(payload),
  });
  const data = await response.json();
  if (!response.ok) {
    status.textContent = data.error || 'Save failed';
    return;
  }
  document.getElementById('telegram-token').value = '';
  status.textContent = data.restart_required
    ? 'Saved. Restart required before polling can start.'
    : 'Saved. Desktop runtime will retry polling automatically.';
  await refresh();
});

document.getElementById('workspace-filter').addEventListener('input', () => renderWorkspaceCards(appState.workspaces));

refresh();
const events = new EventSource('/api/events');

for (const eventName of [
  'setup_changed',
  'runtime_health_changed',
  'managed_codex_changed',
  'thread_state_changed',
  'workspace_state_changed',
  'archived_thread_changed',
]) {
  events.addEventListener(eventName, event => {
    try {
      applyRuntimeEvent(JSON.parse(event.data));
    } catch (error) {
      console.warn(`management SSE ${eventName} parse failed`, error);
      scheduleFullRefresh();
    }
  });
}

events.addEventListener('error', event => {
  console.warn('management SSE error event', event);
  scheduleFullRefresh();
});

events.onerror = error => {
  console.warn('management SSE transport error', error);
  scheduleFullRefresh();
};
