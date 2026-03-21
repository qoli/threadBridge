function escapeHtml(value) {
  return String(value ?? '')
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;');
}

async function renderJson(id, path) {
  const response = await fetch(path);
  const data = await response.json();
  document.getElementById(id).textContent = JSON.stringify(data, null, 2);
  return data;
}

function renderWorkspaceCards(items) {
  const root = document.getElementById('workspaces');
  if (!items.length) {
    root.innerHTML = '<p>No managed workspaces.</p>';
    return;
  }
  root.innerHTML = items.map(item => `
    <div class="workspace-card">
      <strong>${item.title || item.workspace_cwd}</strong><br />
      <code>${item.workspace_cwd}</code><br />
      thread_key: <code>${item.thread_key || ''}</code><br />
      binding: <code>${item.binding_status}</code> |
      run: <code>${item.run_status}</code> |
      runtime_source: <code>${item.runtime_health_source || 'unknown'}</code><br />
      app_server: <code>${item.app_server_status}</code> |
      tui_proxy: <code>${item.tui_proxy_status}</code> |
      handoff: <code>${item.handoff_readiness}</code><br />
      owner_checked_at: <code>${item.heartbeat_last_checked_at || 'n/a'}</code><br />
      owner_last_error: <code>${item.heartbeat_last_error || 'none'}</code><br />
      ${item.recovery_hint ? `<div class="hint" style="margin-top:0.75rem;">${escapeHtml(item.recovery_hint)}</div>` : ''}
      ${item.conflict ? '<strong style="color:var(--accent)">Workspace binding conflict detected. Tray launch is disabled until only one active binding remains.</strong><br />' : ''}
      current: <code>${item.current_codex_thread_id || 'none'}</code><br />
      tui: <code>${item.tui_active_codex_thread_id || 'none'}</code><br />
      adoption_pending: <code>${item.tui_session_adoption_pending ? 'yes' : 'no'}</code><br />
      hcodex: <code>${item.hcodex_path}</code><br />
      recent: ${item.recent_codex_sessions.map(x => `<code>${x.session_id}</code>`).join(', ') || 'none'}
      <div style="margin-top:0.75rem;" class="toolbar">
        ${(item.recent_codex_sessions || []).map(session => `
          <button class="secondary" ${item.conflict ? 'disabled' : ''} onclick="launchResumeWithSession('${item.thread_key}', '${session.session_id}')">Resume ${session.session_id}</button>
        `).join('') || '<span class="muted">No recent sessions to resume.</span>'}
      </div>
      <div style="margin-top:0.75rem;" class="toolbar">
        <input id="bind-${item.thread_key}" type="text" value="${escapeHtml(item.workspace_cwd)}" style="min-width:18rem;flex:1" />
        <button class="secondary" onclick="bindWorkspace('${item.thread_key}')">Bind Workspace</button>
      </div>
      <div style="margin-top:0.75rem;">
        <button class="secondary" onclick="openWorkspace('${item.thread_key}')">Open Workspace</button>
        <button ${item.conflict ? 'disabled' : ''} onclick="launchNew('${item.thread_key}')">Launch New</button>
        <button class="secondary" ${item.conflict ? 'disabled' : ''} onclick="reconnectCodex('${item.thread_key}')">Reconnect Codex</button>
        <button class="secondary" onclick="repairRuntime('${item.thread_key}')">Repair Runtime</button>
        <button ${item.conflict ? 'disabled' : ''} onclick="showLaunchConfig('${item.thread_key}')">Show Launch Commands</button>
        <button onclick='archiveThread(${JSON.stringify(item.thread_key)}, ${JSON.stringify(item.title || item.thread_key)})'>Archive</button>
      </div>
      <div style="margin-top:0.75rem;">
        <input id="resume-${item.thread_key}" type="text" placeholder="session id to resume" style="min-width:18rem;flex:1" />
        <button class="secondary" ${item.conflict ? 'disabled' : ''} onclick="launchResume('${item.thread_key}')">Launch Resume</button>
      </div>
      <pre id="launch-${item.thread_key}" style="display:none;margin-top:0.75rem;"></pre>
    </div>
  `).join('');
}

function renderHealthSummary(health) {
  const root = document.getElementById('health-summary');
  const owner = health.runtime_owner || {};
  const managedCodex = health.managed_codex || {};
  const metrics = [
    ['Global App Server', health.app_server_status || 'unknown'],
    ['Global TUI Proxy', health.tui_proxy_status || 'unknown'],
    ['Global Handoff', health.handoff_readiness || 'unknown'],
    ['Owner State', owner.state || 'inactive'],
    ['Owner Last Start', owner.last_reconcile_started_at || 'never'],
    ['Owner Last Finish', owner.last_reconcile_finished_at || 'never'],
    ['Owner Last Success', owner.last_successful_reconcile_at || 'never'],
    ['Owner Last Error', owner.last_error || 'none'],
    ['Running Workspaces', String(health.running_workspaces ?? 0)],
    ['Ready Workspaces', String(health.ready_workspaces ?? 0)],
    ['Degraded Workspaces', String(health.degraded_workspaces ?? 0)],
    ['Unavailable Workspaces', String(health.unavailable_workspaces ?? 0)],
    ['Broken Threads', String(health.broken_threads ?? 0)],
    ['Conflicted Workspaces', String(health.conflicted_workspaces ?? 0)],
    ['Owner Scanned Workspaces', String(owner.last_report?.scanned_workspaces ?? 0)],
    ['Owner Ensured Workspaces', String(owner.last_report?.ensured_workspaces ?? 0)],
    ['Owner Ensured Proxies', String(owner.last_report?.ensured_proxies ?? 0)],
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
  const hint = document.getElementById('runtime-recovery-hint');
  if (health.recovery_hint) {
    hint.style.display = 'block';
    hint.textContent = health.recovery_hint;
  } else {
    hint.style.display = 'none';
    hint.textContent = '';
  }
}

function renderThreads(items) {
  const root = document.getElementById('threads');
  if (!items.length) {
    root.innerHTML = '<p>No active threads.</p>';
    return;
  }
  root.innerHTML = items.map(item => `
    <div style="border:1px solid var(--border);border-radius:12px;padding:1rem;margin-bottom:1rem;background:white;">
      <strong>${item.title || item.thread_key}</strong><br />
      thread_key: <code>${item.thread_key}</code><br />
      workspace: <code>${item.workspace_cwd || 'unbound'}</code><br />
      binding: <code>${item.binding_status}</code> |
      run: <code>${item.run_status}</code><br />
      current: <code>${item.current_codex_thread_id || 'none'}</code><br />
      tui: <code>${item.tui_active_codex_thread_id || 'none'}</code><br />
      adoption_pending: <code>${item.tui_session_adoption_pending ? 'yes' : 'no'}</code><br />
      last_used_at: <code>${item.last_used_at || 'unknown'}</code><br />
      last_error: <code>${item.last_error || 'none'}</code>
      <div style="margin-top:0.75rem;">
        <button class="secondary" onclick="adoptTuiSession('${item.thread_key}')">Adopt TUI</button>
        <button class="secondary" onclick="rejectTuiSession('${item.thread_key}')">Keep Original</button>
      </div>
    </div>
  `).join('');
}

function renderArchivedThreads(items) {
  const root = document.getElementById('archived');
  if (!items.length) {
    root.innerHTML = '<p>No archived threads.</p>';
    return;
  }
  root.innerHTML = items.map(item => `
    <div style="border:1px solid var(--border);border-radius:12px;padding:1rem;margin-bottom:1rem;background:white;">
      <strong>${item.title || item.thread_key}</strong><br />
      thread_key: <code>${item.thread_key}</code><br />
      workspace: <code>${item.workspace_cwd || 'unbound'}</code><br />
      archived_at: <code>${item.archived_at || 'unknown'}</code>
      <div style="margin-top:0.75rem;">
        <button onclick='restoreThread(${JSON.stringify(item.thread_key)}, ${JSON.stringify(item.title || item.thread_key)})'>Restore</button>
      </div>
    </div>
  `).join('');
}

async function refresh() {
  const [setup, health, threads, workspaces, archived] = await Promise.all([
    renderJson('setup', '/api/setup'),
    renderJson('health', '/api/runtime-health'),
    fetch('/api/threads').then(r => r.json()),
    fetch('/api/workspaces').then(r => r.json()),
    fetch('/api/archived-threads').then(r => r.json()),
  ]);
  document.getElementById('authorized-user-ids').value = (setup.authorized_user_ids || []).join(',');
  document.getElementById('managed-codex-source').value = health.managed_codex?.source || 'brew';
  document.getElementById('managed-codex-source-repo').value =
    health.managed_codex?.build_defaults?.source_repo || '';
  document.getElementById('managed-codex-source-rs-dir').value =
    health.managed_codex?.build_defaults?.source_rs_dir || '';
  document.getElementById('managed-codex-build-profile').value =
    health.managed_codex?.build_defaults?.build_profile || 'dev';
  document.getElementById('onboarding-status').textContent = setup.control_chat_ready
    ? `Control chat is ready: ${setup.control_chat_id}`
    : 'Control chat is not ready. Send /start to the bot from the target Telegram chat first.';
  document.getElementById('runtime-summary-note').textContent =
    `Global summary is aggregated from managed workspaces. Workspace cards below show the owner heartbeat or workspace-state fallback used for each workspace.`;
  renderHealthSummary(health);
  renderThreads(threads);
  renderWorkspaceCards(workspaces);
  renderArchivedThreads(archived);
}

async function showLaunchConfig(threadKey) {
  const response = await fetch(`/api/workspaces/${threadKey}/launch-config`);
  const data = await response.json();
  const target = document.getElementById(`launch-${threadKey}`);
  target.style.display = 'block';
  target.textContent = JSON.stringify(data, null, 2);
}

async function launchNew(threadKey) {
  const response = await fetch(`/api/workspaces/${threadKey}/launch-new`, { method: 'POST' });
  const data = await response.json();
  if (!response.ok) {
    alert(data.error || 'Launch failed');
    return;
  }
  const target = document.getElementById(`launch-${threadKey}`);
  target.style.display = 'block';
  target.textContent = JSON.stringify(data, null, 2);
  await refresh();
}

async function launchResume(threadKey) {
  const sessionId = document.getElementById(`resume-${threadKey}`).value.trim();
  if (!sessionId) {
    alert('Enter a session id first');
    return;
  }
  await launchResumeWithSession(threadKey, sessionId);
}

async function launchResumeWithSession(threadKey, sessionId) {
  const response = await fetch(`/api/workspaces/${threadKey}/launch-resume`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ session_id: sessionId }),
  });
  const data = await response.json();
  if (!response.ok) {
    alert(data.error || 'Launch failed');
    return;
  }
  const target = document.getElementById(`launch-${threadKey}`);
  target.style.display = 'block';
  target.textContent = JSON.stringify(data, null, 2);
  await refresh();
}

async function reconnectCodex(threadKey) {
  const response = await fetch(`/api/workspaces/${threadKey}/reconnect`, { method: 'POST' });
  const data = await response.json();
  if (!response.ok) {
    alert(data.error || 'Reconnect failed');
    return;
  }
  await refresh();
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

async function bindWorkspace(threadKey) {
  const workspace = document.getElementById(`bind-${threadKey}`).value.trim();
  const response = await fetch(`/api/threads/${threadKey}/bind-workspace`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ workspace_cwd: workspace }),
  });
  const data = await response.json();
  if (!response.ok) {
    alert(data.error || 'Bind failed');
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
  if (!window.confirm(`Archive thread "${label}"? This only changes local threadBridge state and Telegram topic state.`)) {
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
  if (!window.confirm(`Restore archived thread "${label}"? This restores local metadata and Telegram topic state only.`)) {
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

async function createThread() {
  const response = await fetch('/api/threads', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ title: document.getElementById('new-thread-title').value || null }),
  });
  const data = await response.json();
  if (!response.ok) {
    alert(data.error || 'Create thread failed');
    return;
  }
  document.getElementById('new-thread-title').value = '';
  await refresh();
}

async function createAndBindThread() {
  const response = await fetch('/api/threads/create-and-bind', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      title: document.getElementById('create-bind-title').value || null,
      workspace_cwd: document.getElementById('create-bind-workspace').value.trim(),
    }),
  });
  const data = await response.json();
  if (!response.ok) {
    alert(data.error || 'Create and bind failed');
    return;
  }
  document.getElementById('create-bind-title').value = '';
  document.getElementById('create-bind-workspace').value = '';
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

refresh();
const events = new EventSource('/api/events');
events.onmessage = () => refresh();
