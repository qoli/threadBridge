const appState = {
  setup: null,
  health: null,
  workspaces: [],
  archived: [],
  transcripts: {},
  sessions: {},
  workspacePanels: {},
  executionModeDrafts: {},
  collaborationModeDrafts: {},
  ui: {
    route: parseRoute(window.location.hash),
    addWorkspaceStatus: '',
    setupStatus: '',
    managedCodexStatus: '',
    launchOutputs: {},
    resumeSessionDrafts: {},
  },
  drafts: {
    setup: {
      telegramToken: '',
      authorizedUserIds: '',
      dirtyAuthorizedUserIds: false,
    },
    managedCodex: {
      source: 'brew',
      sourceRepo: '',
      sourceRsDir: '',
      buildProfile: 'dev',
      dirtySource: false,
      dirtySourceRepo: false,
      dirtySourceRsDir: false,
      dirtyBuildProfile: false,
    },
  },
}

const WORKSPACE_PANEL_KEYS = ['launch', 'sessions', 'transcript', 'advanced']

const pendingObservabilityRefreshThreadKeys = new Set()
let renderScheduled = false
let observabilityRefreshScheduled = false
let fullRefreshScheduled = false
let initialSnapshotLoaded = false

function parseRoute(hash) {
  const normalized = String(hash || '').trim().replace(/^#/, '')
  const path = normalized || '/overview'
  const segments = path.split('/').filter(Boolean)
  if (!segments.length || segments[0] === 'overview') {
    return { page: 'overview' }
  }
  if (segments[0] === 'attention') {
    return { page: 'attention' }
  }
  if (segments[0] === 'workspaces' && segments.length > 1) {
    return { page: 'workspace', threadKey: decodeURIComponent(segments[1]) }
  }
  if (segments[0] === 'workspaces') {
    return { page: 'workspaces' }
  }
  if (segments[0] === 'settings') {
    return { page: 'settings' }
  }
  if (segments[0] === 'archive') {
    return { page: 'archive' }
  }
  return { page: 'overview' }
}

function routeHash(route) {
  switch (route.page) {
    case 'attention':
      return '#/attention'
    case 'workspaces':
      return '#/workspaces'
    case 'workspace':
      return `#/workspaces/${encodeURIComponent(route.threadKey || '')}`
    case 'settings':
      return '#/settings'
    case 'archive':
      return '#/archive'
    default:
      return '#/overview'
  }
}

function navigateToHash(hash) {
  if (window.location.hash === hash) {
    appState.ui.route = parseRoute(hash)
    scheduleRender()
    return
  }
  window.location.hash = hash
}

function escapeHtml(value) {
  return String(value ?? '')
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;')
}

function formatMetaValue(value) {
  const normalized = String(value ?? '').trim()
  if (!normalized) {
    return 'Unknown'
  }
  return normalized
    .replaceAll(/[_-]+/g, ' ')
    .replace(/\b([a-z])/g, match => match.toUpperCase())
}

function toneForStatus(value) {
  switch (value) {
    case 'running':
    case 'ready':
    case 'healthy':
    case 'active':
    case 'yes':
    case 'configured':
    case 'available':
    case 'default':
      return 'good'
    case 'degraded':
    case 'pending_adoption':
    case 'pending':
    case 'idle':
    case 'missing':
    case 'plan':
      return 'warn'
    case 'broken':
    case 'conflict':
    case 'unavailable':
    case 'stale':
    case 'invalid':
    case 'error':
      return 'bad'
    default:
      return 'neutral'
  }
}

function pill(label, value) {
  return `<span class="pill pill-${toneForStatus(value)}">${escapeHtml(label)}: ${escapeHtml(value)}</span>`
}

function workspaceNeedsAttention(item) {
  return item.conflict || item.binding_status === 'broken' || item.runtime_readiness !== 'ready'
}

function renderMetricCell(label, value, note = '') {
  return `
    <div class="summary-cell">
      <div class="summary-label">${escapeHtml(label)}</div>
      <div class="summary-value">${escapeHtml(value)}</div>
      ${note ? `<div class="summary-note">${escapeHtml(note)}</div>` : ''}
    </div>
  `
}

function formatRelativeTime(value) {
  if (!value || value === 'never') {
    return 'never'
  }
  const timestamp = new Date(value)
  const millis = timestamp.getTime()
  if (!Number.isFinite(millis)) {
    return String(value)
  }
  const diffSeconds = Math.round((millis - Date.now()) / 1000)
  const absoluteSeconds = Math.abs(diffSeconds)
  if (absoluteSeconds < 45) {
    return diffSeconds >= 0 ? 'soon' : 'just now'
  }
  const units = [
    ['year', 60 * 60 * 24 * 365],
    ['month', 60 * 60 * 24 * 30],
    ['week', 60 * 60 * 24 * 7],
    ['day', 60 * 60 * 24],
    ['hour', 60 * 60],
    ['minute', 60],
  ]
  const formatter = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto', style: 'short' })
  for (const [unit, secondsPerUnit] of units) {
    if (absoluteSeconds >= secondsPerUnit) {
      return formatter.format(Math.round(diffSeconds / secondsPerUnit), unit)
    }
  }
  return formatter.format(diffSeconds, 'second')
}

function renderDefinitionGrid(items) {
  return `
    <div class="definition-grid">
      ${items.map(([label, value]) => `
        <dl class="definition-item">
          <dt>${escapeHtml(label)}</dt>
          <dd>${String(label).includes('path') || String(label).includes('thread') ? `<code>${escapeHtml(value)}</code>` : escapeHtml(value)}</dd>
        </dl>
      `).join('')}
    </div>
  `
}

function transcriptCache(threadKey) {
  if (!appState.transcripts[threadKey]) {
    appState.transcripts[threadKey] = {
      loaded: false,
      loading: false,
      error: null,
      entries: [],
    }
  }
  return appState.transcripts[threadKey]
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
    }
  }
  return appState.sessions[threadKey]
}

function workspacePanelState(threadKey) {
  if (!appState.workspacePanels[threadKey]) {
    appState.workspacePanels[threadKey] = {
      launch: false,
      sessions: true,
      transcript: false,
      advanced: false,
    }
  }
  return appState.workspacePanels[threadKey]
}

function workspaceByThreadKey(threadKey) {
  return (appState.workspaces || []).find(item => item.thread_key === threadKey) || null
}

function workspaceIndexByCwd(workspaceCwd) {
  return (appState.workspaces || []).findIndex(item => item.workspace_cwd === workspaceCwd)
}

function archivedThreadIndexByKey(threadKey) {
  return (appState.archived || []).findIndex(item => item.thread_key === threadKey)
}

function upsertArrayItem(items, index, value) {
  if (index >= 0) {
    items[index] = value
    return items
  }
  items.push(value)
  return items
}

function removeArrayItem(items, index) {
  if (index < 0) {
    return items
  }
  items.splice(index, 1)
  return items
}

function effectiveExecutionModeValue(item) {
  return appState.executionModeDrafts[item.thread_key] || item.workspace_execution_mode || 'full_auto'
}

function effectiveCollaborationModeValue(item) {
  return appState.collaborationModeDrafts[item.thread_key] || item.current_collaboration_mode || 'default'
}

function setExecutionModeDraft(threadKey, value) {
  const workspace = workspaceByThreadKey(threadKey)
  if (!workspace || !value || value === workspace.workspace_execution_mode) {
    delete appState.executionModeDrafts[threadKey]
    return
  }
  appState.executionModeDrafts[threadKey] = value
}

function setCollaborationModeDraft(threadKey, value) {
  const workspace = workspaceByThreadKey(threadKey)
  if (!workspace || !value || value === (workspace.current_collaboration_mode || 'default')) {
    delete appState.collaborationModeDrafts[threadKey]
    return
  }
  appState.collaborationModeDrafts[threadKey] = value
}

function reconcileDrafts() {
  for (const [threadKey, draftValue] of Object.entries(appState.executionModeDrafts || {})) {
    const workspace = workspaceByThreadKey(threadKey)
    if (!workspace || workspace.workspace_execution_mode === draftValue) {
      delete appState.executionModeDrafts[threadKey]
    }
  }
  for (const [threadKey, draftValue] of Object.entries(appState.collaborationModeDrafts || {})) {
    const workspace = workspaceByThreadKey(threadKey)
    if (!workspace || (workspace.current_collaboration_mode || 'default') === draftValue) {
      delete appState.collaborationModeDrafts[threadKey]
    }
  }
}

function syncDraftsFromData() {
  const setup = appState.setup || {}
  const managedCodex = appState.health?.managed_codex || {}
  const managedCodexDraft = appState.drafts.managedCodex
  if (!appState.drafts.setup.dirtyAuthorizedUserIds) {
    appState.drafts.setup.authorizedUserIds = (setup.authorized_user_ids || []).join(',')
  }
  if (!managedCodexDraft.dirtySource) {
    managedCodexDraft.source = managedCodex.source || 'brew'
  }
  if (!managedCodexDraft.dirtySourceRepo) {
    managedCodexDraft.sourceRepo = managedCodex.build_defaults?.source_repo || ''
  }
  if (!managedCodexDraft.dirtySourceRsDir) {
    managedCodexDraft.sourceRsDir = managedCodex.build_defaults?.source_rs_dir || ''
  }
  if (!managedCodexDraft.dirtyBuildProfile) {
    managedCodexDraft.buildProfile = managedCodex.build_defaults?.build_profile || 'dev'
  }
}

function workspacePrimaryLabel(item) {
  const workspace = String(item.workspace_cwd || '').trim()
  if (!workspace) {
    return item.title || item.thread_key || 'Workspace'
  }
  const segments = workspace.split('/').filter(Boolean)
  return segments[segments.length - 1] || workspace
}

function workspaceSecondaryLabel(item) {
  if (item.title && item.title !== workspacePrimaryLabel(item)) {
    return item.title
  }
  return null
}

function sortByLastUsed(items) {
  return [...items].sort((a, b) => String(b.last_used_at || '').localeCompare(String(a.last_used_at || '')))
}

function attentionWorkspaces(items = appState.workspaces) {
  return sortByLastUsed(items.filter(item => workspaceNeedsAttention(item)))
}

function visibleManagedWorkspaces(items) {
  return items.filter(item => !workspaceNeedsAttention(item))
}

function prettyLabel(value) {
  const text = String(value || 'unknown').replaceAll('_', ' ')
  return text.charAt(0).toUpperCase() + text.slice(1)
}

function workspaceStatusDescriptor(item) {
  if (item.conflict) {
    return { label: 'Conflict', tone: 'bad' }
  }
  if (item.binding_status === 'broken') {
    return { label: 'Broken Binding', tone: 'bad' }
  }
  if (item.runtime_readiness !== 'ready') {
    return { label: `Runtime ${prettyLabel(item.runtime_readiness)}`, tone: toneForStatus(item.runtime_readiness) }
  }
  if (item.run_status === 'running') {
    return { label: 'Running', tone: 'good' }
  }
  return { label: 'Ready', tone: 'good' }
}

function workspaceAuxDescriptor(item) {
  if (item.current_collaboration_mode === 'plan') {
    return { label: 'Plan Mode', tone: 'warn' }
  }
  if (item.mode_drift) {
    return { label: 'Mode Drift', tone: 'warn' }
  }
  if (item.workspace_execution_mode && item.workspace_execution_mode !== 'full_auto') {
    return { label: prettyLabel(item.workspace_execution_mode), tone: 'neutral' }
  }
  return null
}

function workspaceSupportText(item) {
  if (item.recovery_hint) {
    return item.recovery_hint
  }
  if (item.session_broken_reason) {
    return `Session issue: ${item.session_broken_reason}`
  }
  if (item.run_status === 'running') {
    return 'Active turn is running in the current binding.'
  }
  if (item.current_collaboration_mode === 'plan') {
    return 'Collaboration mode is set to plan.'
  }
  if (item.mode_drift) {
    return 'Current session mode differs from the workspace default.'
  }
  return ''
}

function renderStatusToken(label, tone) {
  return `<span class="status-token status-token-${escapeHtml(tone)}">${escapeHtml(label)}</span>`
}

function renderPageEmpty(title, detail = '', actionHtml = '') {
  return `
    <div class="page-empty">
      <div class="page-empty-copy">
        <h2>${escapeHtml(title)}</h2>
        ${detail ? `<p>${escapeHtml(detail)}</p>` : ''}
      </div>
      ${actionHtml ? `<div class="page-empty-actions">${actionHtml}</div>` : ''}
    </div>
  `
}

function renderRecentLedgerRow(item) {
  const dominant = workspaceStatusDescriptor(item)
  const aux = workspaceAuxDescriptor(item)
  return `
    <article class="list-row recent-ledger-row">
      <div class="recent-ledger-main">
        <div class="recent-ledger-head">
          <div class="recent-ledger-copy">
            <h3 class="list-row-headline">${escapeHtml(workspacePrimaryLabel(item))}</h3>
            ${workspaceSecondaryLabel(item) ? `<div class="secondary-label">${escapeHtml(workspaceSecondaryLabel(item))}</div>` : ''}
          </div>
          <div class="summary-row-tags">
            ${renderStatusToken(dominant.label, dominant.tone)}
            ${aux ? renderStatusToken(aux.label, aux.tone) : ''}
          </div>
        </div>
        <div class="row-meta-line">
          <time class="recent-ledger-time">${escapeHtml(formatRelativeTime(item.last_used_at || 'unknown'))}</time>
          <span class="row-path">${escapeHtml(item.workspace_cwd)}</span>
        </div>
      </div>
      <div class="action-slot">
        <a class="button button-secondary" href="#/workspaces/${encodeURIComponent(item.thread_key || '')}">View Detail</a>
      </div>
    </article>
  `
}

function renderWorkspaceSummaryRow(item, { showOpenAction = false, emphasizeProblem = false } = {}) {
  const dominant = workspaceStatusDescriptor(item)
  const aux = workspaceAuxDescriptor(item)
  const support = workspaceSupportText(item)
  return `
    <article class="list-row summary-row ${emphasizeProblem ? 'is-problem' : ''}">
      <div class="summary-row-main">
        <div class="summary-row-head">
          <div class="summary-row-title">
            <h3 class="list-row-headline">${escapeHtml(workspacePrimaryLabel(item))}</h3>
            ${workspaceSecondaryLabel(item) ? `<div class="secondary-label">${escapeHtml(workspaceSecondaryLabel(item))}</div>` : ''}
          </div>
          <div class="summary-row-tags">
            ${renderStatusToken(dominant.label, dominant.tone)}
            ${aux ? renderStatusToken(aux.label, aux.tone) : ''}
          </div>
        </div>
        <div class="row-meta-line">
          <span class="row-path">${escapeHtml(item.workspace_cwd)}</span>
          ${support ? `<span class="support-line">${escapeHtml(support)}</span>` : ''}
        </div>
      </div>
      <div class="action-slot">
        <a class="button button-secondary" href="#/workspaces/${encodeURIComponent(item.thread_key || '')}">View Detail</a>
        ${showOpenAction ? `<button class="button button-secondary" onclick="openWorkspace(${JSON.stringify(item.thread_key)})">Open Workspace</button>` : ''}
      </div>
    </article>
  `
}

function configureAddWorkspaceState() {
  const setup = appState.setup || {}
  if (setup.native_workspace_picker_available) {
    if (setup.telegram_polling_state !== 'active') {
      return {
        disabled: true,
        message: 'Telegram bot is not active yet. Save setup or wait for desktop runtime to reconnect polling.',
      }
    }
    if (!setup.control_chat_ready) {
      return {
        disabled: true,
        message: 'Send /start to the bot from the target Telegram chat first. Add Workspace creates a Telegram topic for that workspace.',
      }
    }
    return {
      disabled: false,
      message: 'Desktop runtime will open the system folder picker and create or reuse the managed workspace thread.',
    }
  }
  return {
    disabled: true,
    message: 'Add Workspace requires threadbridge_desktop. Headless runtime does not expose the native folder picker.',
  }
}

function routeTitle(route) {
  switch (route.page) {
    case 'attention':
      return {
        title: 'Attention',
      }
    case 'workspaces':
      return {
        title: 'Managed Workspaces',
      }
    case 'workspace': {
      const item = workspaceByThreadKey(route.threadKey)
      return {
        title: item ? workspacePrimaryLabel(item) : 'Workspace Detail',
      }
    }
    case 'settings':
      return {
        title: 'Settings',
      }
    case 'archive':
      return {
        title: 'Archive',
      }
    default:
      return {
        title: 'Overview',
      }
  }
}

function navItemsForRoute(route) {
  const setup = appState.setup || {}
  const attentionCount = attentionWorkspaces().length
  const items = [
    { page: 'overview', label: 'Overview', count: '' },
    { page: 'workspaces', label: 'Workspaces', count: appState.workspaces.length || '' },
  ]
  if (attentionCount || route.page === 'attention') {
    items.push({ page: 'attention', label: 'Attention', count: attentionCount })
  }
  items.push(
    { page: 'settings', label: 'Settings', count: !setup.telegram_token_configured || !setup.control_chat_ready ? '!' : '' },
    { page: 'archive', label: 'Archive', count: appState.archived.length || '' },
  )
  return items
}

function renderShellHeader(route) {
  const setup = appState.setup || {}
  const health = appState.health || {}
  return `
    <header class="shell-header">
      <div class="shell-bar">
        <div class="shell-brand">
          <div class="brand-copy-block">
            <div class="shell-brand-title">threadBridge</div>
          </div>
        </div>
        <div class="shell-meta">
          <span class="meta-inline"><strong>Bind</strong>${escapeHtml(window.THREADBRIDGE_MANAGEMENT_BIND_ADDR || appState.health?.management_bind_addr || 'unknown')}</span>
          <span class="meta-inline"><strong>Polling</strong>${escapeHtml(formatMetaValue(setup.telegram_polling_state || 'unknown'))}</span>
          <span class="meta-inline"><strong>Owner</strong>${escapeHtml(formatMetaValue(health.runtime_owner?.state || 'inactive'))}</span>
          <span class="meta-inline"><strong>Runtime</strong>${escapeHtml(formatMetaValue(health.runtime_readiness || 'unknown'))}</span>
        </div>
      </div>
      ${renderRouteNav(route)}
    </header>
  `
}

function renderRouteNav(route) {
  const navItems = navItemsForRoute(route)
  return `
    <nav class="route-nav" aria-label="Primary">
      <div class="route-nav-track">
        ${navItems.map(item => {
          const targetRoute = { page: item.page }
          const isActive = route.page === item.page || (route.page === 'workspace' && item.page === 'workspaces')
          return `
            <a class="route-link ${isActive ? 'is-active' : ''}" href="${routeHash(targetRoute)}">
              <strong>${escapeHtml(item.label)}</strong>
              ${item.count !== '' ? `<span class="route-link-count">${escapeHtml(item.count)}</span>` : ''}
            </a>
          `
        }).join('')}
      </div>
    </nav>
  `
}

function renderTopbar(route) {
  if (route.page === 'workspace') {
    return ''
  }
  const meta = routeTitle(route)
  const addWorkspaceState = route.page === 'workspaces' ? configureAddWorkspaceState() : null
  return `
    <header class="page-header">
      <div class="page-header-main">
        <h1>${escapeHtml(meta.title)}</h1>
      </div>
      <div class="page-header-actions">
        <button class="button button-primary" onclick="reconcileRuntimeOwner()">Reconcile Runtime Owner</button>
        ${route.page === 'workspaces'
          ? `<button class="button button-secondary" onclick="pickAndAddWorkspace()" ${addWorkspaceState?.disabled ? 'disabled' : ''}>Add Workspace</button>`
          : ''}
        ${route.page === 'archive'
          ? '<button class="button button-danger" onclick="purgeArchivedThreads()">Purge Archived Threads</button>'
          : ''}
      </div>
    </header>
  `
}

function renderGlobalBanner(route) {
  if (route.page !== 'settings' && appState.ui.managedCodexStatus) {
    return `<div class="route-banner"><strong>Runtime Action</strong><div class="muted">${escapeHtml(appState.ui.managedCodexStatus)}</div></div>`
  }
  return ''
}

function renderOverviewPage() {
  const setup = appState.setup || {}
  const health = appState.health || {}
  const nextSteps = []
  if (!setup.telegram_token_configured) {
    nextSteps.push('Save a Telegram bot token and authorized user IDs in Settings.')
  }
  if (setup.telegram_token_configured && !setup.control_chat_ready) {
    nextSteps.push('Send /start to the bot from the target Telegram chat so the control chat exists.')
  }
  if (setup.telegram_token_configured && setup.control_chat_ready && !appState.workspaces.length) {
    nextSteps.push('Add the first workspace from Workspaces to complete the minimum viable setup.')
  }

  const attention = attentionWorkspaces().slice(0, 6)
  const recent = sortByLastUsed(appState.workspaces).slice(0, 6)
  const sections = []

  if (nextSteps.length) {
    sections.push(`
      <section class="section-block">
        <div class="section-head">
          <div class="section-copy">
            <h2>Next Steps</h2>
          </div>
        </div>
        <div class="list">
          ${nextSteps.map(step => `<div class="status-note">${escapeHtml(step)}</div>`).join('')}
        </div>
      </section>
    `)
  }

  if (attention.length) {
    sections.push(`
      <a class="route-banner attention-strip" href="#/attention">
        <strong>${escapeHtml(attention.length)} workspace${attention.length === 1 ? '' : 's'} need attention</strong>
        <div class="muted">${escapeHtml(workspacePrimaryLabel(attention[0]))}${attention.length > 1 ? ` + ${escapeHtml(attention.length - 1)} more` : ''}</div>
      </a>
    `)
  }

  if (recent.length) {
    sections.push(`
      <section class="section-block">
        <div class="section-head">
          <div class="section-copy">
            <h2>Recent</h2>
          </div>
        </div>
        <div class="list recent-ledger">
          ${recent.map(item => renderRecentLedgerRow(item)).join('')}
        </div>
      </section>
    `)
  }

  return `
    <div class="page-body overview-layout">
      ${health.recovery_hint ? `<div class="route-banner"><strong>Runtime Recovery Hint</strong><div class="muted">${escapeHtml(health.recovery_hint)}</div></div>` : ''}
      <section class="summary-strip">
        ${renderMetricCell('Running', health.running_workspaces ?? 0, 'workspaces')}
        ${renderMetricCell('Ready', health.ready_workspaces ?? 0, 'runtime healthy')}
        ${renderMetricCell('Degraded', health.degraded_workspaces ?? 0, 'need attention')}
        ${renderMetricCell('Broken', health.broken_threads ?? 0, 'thread continuity')}
        ${renderMetricCell('Owner Last Success', formatRelativeTime(health.runtime_owner?.last_successful_reconcile_at))}
        ${renderMetricCell('Managed Codex', health.managed_codex?.version || 'unknown', health.managed_codex?.binary_ready ? 'binary ready' : 'binary unavailable')}
      </section>
      ${sections.join('')}
    </div>
  `
}

function renderWorkspaceSection(title, items, options = {}) {
  if (!items.length) {
    return ''
  }
  return `
    <section class="section-block">
      <div class="section-head">
        <div class="section-copy">
          <h2>${escapeHtml(title)}</h2>
        </div>
        <span class="section-count">${escapeHtml(items.length)}</span>
      </div>
      <div class="list summary-list">
        ${items.map(item => renderWorkspaceSummaryRow(item, options)).join('')}
      </div>
    </section>
  `
}

function renderAttentionPage() {
  const items = attentionWorkspaces()
  if (!items.length) {
    return `
      <div class="page-body">
        ${renderPageEmpty('No workspaces need attention.')}
      </div>
    `
  }
  return `
    <div class="page-body">
      <div class="summary-list attention-list">
        ${items.map(item => renderWorkspaceSummaryRow(item, { showOpenAction: true, emphasizeProblem: true })).join('')}
      </div>
    </div>
  `
}

function renderWorkspacesPage() {
  const attention = attentionWorkspaces()
  const visible = visibleManagedWorkspaces(appState.workspaces)
  const active = visible.filter(item => item.run_status === 'running' || item.binding_status === 'active')
  const other = visible.filter(item => !active.includes(item))
  const sections = [
    renderWorkspaceSection('Active And Ready', active),
    renderWorkspaceSection('Other Managed Workspaces', other),
  ].filter(Boolean).join('')
  let pageContent = sections
  if (!appState.workspaces.length) {
    pageContent = renderPageEmpty('No managed workspaces yet.')
  } else if (!visible.length) {
    pageContent = renderPageEmpty(
      'All workspaces are in Attention.',
      '',
      attention.length ? '<a class="button button-secondary" href="#/attention">Open Attention</a>' : '',
    )
  }
  return `
    <div class="page-body section-stack">
      ${pageContent}
    </div>
  `
}

function renderSettingsPage() {
  const setup = appState.setup || {}
  const managedCodex = appState.health?.managed_codex || {}
  return `
    <div class="page-body section-stack">
      <section class="section-block">
        <div class="section-head">
          <div class="section-copy">
            <h2>Setup</h2>
          </div>
          <div class="pill-row">
            ${pill('token', setup.telegram_token_configured ? 'configured' : 'missing')}
            ${pill('polling', setup.telegram_polling_state || 'disconnected')}
            ${pill('control chat', setup.control_chat_ready ? 'ready' : 'missing')}
          </div>
        </div>
        <form class="form-grid" onsubmit="return submitSetupForm(event)">
          <div class="field">
            <span>Telegram Bot Token</span>
            <input type="password" value="${escapeHtml(appState.drafts.setup.telegramToken)}" placeholder="Configured tokens stay masked." oninput="updateSetupDraft('telegramToken', this.value)" />
          </div>
          <div class="field">
            <span>Authorized User IDs</span>
            <input type="text" value="${escapeHtml(appState.drafts.setup.authorizedUserIds)}" placeholder="Comma separated Telegram user IDs" oninput="updateSetupDraft('authorizedUserIds', this.value)" />
          </div>
          <div class="button-row">
            <button class="button button-primary" type="submit">Save Setup</button>
            <div class="muted">${escapeHtml(appState.ui.setupStatus)}</div>
          </div>
        </form>
        ${setup.control_chat_ready ? '' : '<div class="status-note">Control chat missing. Send /start in the target chat.</div>'}
      </section>
      <section class="section-block">
        <div class="section-head">
          <div class="section-copy">
            <h2>Source And Build Defaults</h2>
          </div>
          <div class="pill-row">
            ${pill('source', managedCodex.source || 'unknown')}
            ${pill('binary', managedCodex.binary_ready ? 'ready' : 'unavailable')}
          </div>
        </div>
        <div class="form-grid">
          <div class="field">
            <span>Codex Source</span>
            <select onchange="updateManagedCodexDraft('source', this.value)">
              <option value="brew" ${appState.drafts.managedCodex.source === 'brew' ? 'selected' : ''}>brew</option>
              <option value="source" ${appState.drafts.managedCodex.source === 'source' ? 'selected' : ''}>source</option>
            </select>
          </div>
          <div class="button-row">
            <button class="button button-secondary" onclick="updateManagedCodexPreference()">Apply Codex Source</button>
            <button class="button button-secondary" onclick="refreshManagedCodexCache()">Refresh Managed Cache</button>
            <button class="button button-primary" onclick="buildManagedCodexSource()">Build Source Codex</button>
          </div>
          <div class="field">
            <span>Source Repo</span>
            <input type="text" value="${escapeHtml(appState.drafts.managedCodex.sourceRepo)}" placeholder="/abs/codex/repo" oninput="updateManagedCodexDraft('sourceRepo', this.value)" />
          </div>
          <div class="field">
            <span>Source Rs Dir</span>
            <input type="text" value="${escapeHtml(appState.drafts.managedCodex.sourceRsDir)}" placeholder="/abs/codex-rs" oninput="updateManagedCodexDraft('sourceRsDir', this.value)" />
          </div>
          <div class="field">
            <span>Build Profile</span>
            <select onchange="updateManagedCodexDraft('buildProfile', this.value)">
              <option value="dev" ${appState.drafts.managedCodex.buildProfile === 'dev' ? 'selected' : ''}>dev</option>
              <option value="release" ${appState.drafts.managedCodex.buildProfile === 'release' ? 'selected' : ''}>release</option>
            </select>
          </div>
          <div class="button-row">
            <button class="button button-secondary" onclick="saveManagedCodexBuildDefaults()">Save Build Defaults</button>
            <div class="muted">${escapeHtml(appState.ui.managedCodexStatus)}</div>
          </div>
          ${renderDefinitionGrid([
            ['Binary Path', managedCodex.binary_path || 'unknown'],
            ['Source File', managedCodex.source_file_path || 'unknown'],
            ['Build Config', managedCodex.build_config_file_path || 'unknown'],
            ['Build Info', managedCodex.build_info_file_path || 'unknown'],
            ['Version', managedCodex.version || 'unknown'],
          ])}
        </div>
      </section>
    </div>
  `
}

function renderArchivePage() {
  if (!appState.archived.length) {
    return `
      <div class="page-body">
        ${renderPageEmpty('No archived workspaces.')}
      </div>
    `
  }
  return `
    <div class="page-body">
      <section class="section-block">
        <div class="section-head">
          <div class="section-copy">
            <h2>Archived Workspaces</h2>
          </div>
          <span class="section-count">${escapeHtml(appState.archived.length)}</span>
        </div>
        <div class="list">
          ${appState.archived.map(item => `
            <article class="list-row archive-row">
              <div class="archive-row-main">
                <h3 class="list-row-headline">${escapeHtml(workspacePrimaryLabel(item))}</h3>
                ${workspaceSecondaryLabel(item) ? `<div class="secondary-label">${escapeHtml(workspaceSecondaryLabel(item))}</div>` : ''}
                <div class="row-meta-line">
                  <span class="row-path">${escapeHtml(item.workspace_cwd || item.thread_key)}</span>
                  <span class="support-line">${escapeHtml(item.archived_at || 'unknown')}</span>
                </div>
              </div>
              <div class="action-slot">
                <button class="button button-secondary" onclick='restoreThread(${JSON.stringify(item.thread_key)}, ${JSON.stringify(item.title || item.thread_key)})'>Restore</button>
              </div>
            </article>
          `).join('')}
        </div>
      </section>
    </div>
  `
}

function renderWorkspaceDetailPage(threadKey) {
  const item = workspaceByThreadKey(threadKey)
  if (!item) {
    return `
      <section class="section-block">
        <div class="empty-state">
          No active binding for <code>${escapeHtml(threadKey || 'unknown')}</code>.
        </div>
      </section>
    `
  }

  const panels = workspacePanelState(threadKey)
  const selectedExecutionMode = effectiveExecutionModeValue(item)
  const selectedCollaborationMode = effectiveCollaborationModeValue(item)
  const resumeDraft = appState.ui.resumeSessionDrafts[threadKey] || ''
  const interruptDisabled = item.conflict || item.interrupt_status !== 'available'

  return `
    <div class="detail-shell">
      <section class="detail-command-header">
        <div class="detail-command-copy">
          <div class="detail-back-row">
            <a class="button button-secondary" href="#/workspaces">Back To Workspaces</a>
          </div>
          <div>
            <h1 class="detail-title">${escapeHtml(workspacePrimaryLabel(item))}</h1>
            ${workspaceSecondaryLabel(item) ? `<p class="muted">${escapeHtml(workspaceSecondaryLabel(item))}</p>` : ''}
            <div class="path-code">${escapeHtml(item.workspace_cwd)}</div>
          </div>
          ${item.recovery_hint ? `<div class="hint">${escapeHtml(item.recovery_hint)}</div>` : ''}
          ${item.mode_drift ? `<div class="status-note">Mode drift. Next turn or resume restores <code>${escapeHtml(item.workspace_execution_mode)}</code>.</div>` : ''}
          ${item.interrupt_note ? `<div class="status-note">${escapeHtml(item.interrupt_note)}</div>` : ''}
        </div>
        <div class="detail-command-actions">
          <div class="pill-row">
            ${pill('binding', item.binding_status)}
            ${pill('run', item.run_status)}
            ${pill('phase', item.run_phase)}
            ${pill('runtime', item.runtime_readiness)}
            ${pill('interrupt', item.interrupt_status)}
            ${item.conflict ? pill('conflict', 'conflict') : ''}
          </div>
          <div class="button-row">
            <button class="button button-primary" ${item.conflict ? 'disabled' : ''} onclick="startFreshSession(${JSON.stringify(threadKey)})">Start Fresh Session</button>
            <button class="button button-secondary" ${interruptDisabled ? 'disabled' : ''} onclick="interruptRunningTurn(${JSON.stringify(threadKey)})">
              ${item.interrupt_status === 'pending' ? 'Interrupt Requested' : 'Interrupt Active Turn'}
            </button>
            <button class="button button-secondary" onclick="openWorkspace(${JSON.stringify(threadKey)})">Open Workspace</button>
          </div>
        </div>
      </section>
      <div class="detail-matrix">
        <section class="matrix-panel matrix-panel-wide">
          <div class="panel-head">
            <div class="section-copy">
              <h2 class="section-title">Mode, Continuity, And Active Turn</h2>
            </div>
          </div>
          <div class="form-split">
            <div class="field">
              <span>Execution Mode</span>
              <select ${item.conflict ? 'disabled' : ''} onchange="setExecutionModeDraft(${JSON.stringify(threadKey)}, this.value); scheduleRender()">
                <option value="full_auto" ${selectedExecutionMode === 'full_auto' ? 'selected' : ''}>full_auto</option>
                <option value="yolo" ${selectedExecutionMode === 'yolo' ? 'selected' : ''}>yolo</option>
              </select>
            </div>
            <div class="field">
              <span>Collaboration Mode</span>
              <select ${item.conflict ? 'disabled' : ''} onchange="setCollaborationModeDraft(${JSON.stringify(threadKey)}, this.value); scheduleRender()">
                <option value="default" ${selectedCollaborationMode === 'default' ? 'selected' : ''}>default</option>
                <option value="plan" ${selectedCollaborationMode === 'plan' ? 'selected' : ''}>plan</option>
              </select>
            </div>
          </div>
          <div class="button-row">
            <button class="button button-secondary" ${item.conflict ? 'disabled' : ''} onclick="updateExecutionMode(${JSON.stringify(threadKey)})">Save Execution Mode</button>
            <button class="button button-secondary" ${item.conflict ? 'disabled' : ''} onclick="updateCollaborationMode(${JSON.stringify(threadKey)})">Save Collaboration Mode</button>
            <button class="button button-secondary" ${item.conflict ? 'disabled' : ''} onclick="repairContinuity(${JSON.stringify(threadKey)}, ${JSON.stringify(item.binding_status)}, ${item.tui_session_adoption_pending ? 'true' : 'false'})">
              ${item.tui_session_adoption_pending ? 'Adopt TUI' : 'Repair Session'}
            </button>
            ${item.tui_active_codex_thread_id ? `<button class="button button-secondary" onclick="rejectTuiSession(${JSON.stringify(threadKey)})">Keep Original Binding</button>` : ''}
          </div>
        </section>
        <section class="matrix-panel">
          <div class="panel-head">
            <div class="section-copy">
              <h2 class="section-title">Binding And Runtime</h2>
            </div>
          </div>
          <div class="summary-strip compact">
            ${renderMetricCell('App Server', item.app_server_status || 'unknown')}
            ${renderMetricCell('Ingress', item.hcodex_ingress_status || 'unknown')}
            ${renderMetricCell('Readiness', item.runtime_readiness || 'unknown')}
            ${renderMetricCell('Interrupt', item.interrupt_status || 'unknown')}
          </div>
          ${renderDefinitionGrid([
            ['Binding', item.binding_status],
            ['Run Status', item.run_status],
            ['Run Phase', item.run_phase],
            ['Workspace Mode', item.workspace_execution_mode || 'full_auto'],
          ])}
        </section>
        <section class="matrix-panel">
          <div class="panel-head">
            <div class="section-copy">
              <h2 class="section-title">Managed hcodex Entry</h2>
            </div>
          </div>
          <div class="button-row">
            <button class="button button-primary" ${item.conflict ? 'disabled' : ''} onclick="launchHcodexNew(${JSON.stringify(threadKey)})">Launch Local Session (new)</button>
            <button class="button button-secondary" ${item.conflict || !item.current_codex_thread_id ? 'disabled' : ''} onclick="launchHcodexContinueCurrent(${JSON.stringify(threadKey)})">Continue Current</button>
            <button class="button button-secondary" ${item.conflict ? 'disabled' : ''} onclick="showLaunchConfig(${JSON.stringify(threadKey)})">Show Launch Commands</button>
          </div>
          <div class="field">
            <span>Resume Specific Session</span>
            <input type="text" value="${escapeHtml(resumeDraft)}" placeholder="session id to resume" oninput="updateResumeSessionDraft(${JSON.stringify(threadKey)}, this.value)" />
          </div>
          <div class="button-row">
            <button class="button button-secondary" ${item.conflict ? 'disabled' : ''} onclick="launchHcodexResume(${JSON.stringify(threadKey)})">Launch Resume</button>
            <button class="button button-secondary" onclick="repairRuntime(${JSON.stringify(threadKey)})">Repair Runtime</button>
            <button class="button button-danger" onclick='archiveThread(${JSON.stringify(threadKey)}, ${JSON.stringify(workspacePrimaryLabel(item))})'>Archive</button>
          </div>
          <div class="resume-strip">
            <div class="field-caption">Recent Sessions</div>
            <div class="resume-list">
              ${(item.recent_codex_sessions || []).map(session => `
                <span class="session-chip">
                  <code class="session-token">${escapeHtml(session.session_id)}</code>
                  <span class="muted">${escapeHtml(session.execution_mode || 'unknown')}</span>
                  <button class="button button-secondary btn-sm" ${item.conflict ? 'disabled' : ''} onclick="launchHcodexResumeWithSession(${JSON.stringify(threadKey)}, ${JSON.stringify(session.session_id)})">Resume</button>
                </span>
              `).join('') || '<span class="muted">No recent sessions to resume.</span>'}
            </div>
          </div>
        </section>
        <section class="matrix-panel matrix-panel-wide">
            <button class="summary-toggle" onclick="toggleWorkspacePanel(${JSON.stringify(threadKey)}, 'sessions')">
              <strong>Sessions</strong>
              <span>${panels.sessions ? 'Hide' : 'Show'}</span>
            </button>
            ${panels.sessions ? `<div class="collapsible-body">${renderWorkingSessions(threadKey)}</div>` : ''}
        </section>
        <section class="matrix-panel matrix-panel-wide">
            <button class="summary-toggle" onclick="toggleWorkspacePanel(${JSON.stringify(threadKey)}, 'transcript')">
              <strong>Transcript</strong>
              <span>${panels.transcript ? 'Hide' : 'Show'}</span>
            </button>
            ${panels.transcript ? `<div class="collapsible-body">${renderWorkspaceTranscript(threadKey)}</div>` : ''}
        </section>
        <section class="matrix-panel">
            <button class="summary-toggle" onclick="toggleWorkspacePanel(${JSON.stringify(threadKey)}, 'launch')">
              <strong>Launch Output</strong>
              <span>${panels.launch ? 'Hide' : 'Show'}</span>
            </button>
            ${panels.launch ? `
              <div class="collapsible-body">
                <pre class="raw-block">${escapeHtml(appState.ui.launchOutputs[threadKey] || 'No launch output yet.')}</pre>
              </div>
            ` : ''}
        </section>
        <section class="matrix-panel">
            <button class="summary-toggle" onclick="toggleWorkspacePanel(${JSON.stringify(threadKey)}, 'advanced')">
              <strong>Advanced Workspace Details</strong>
              <span>${panels.advanced ? 'Hide' : 'Show'}</span>
            </button>
            ${panels.advanced ? `
              <div class="collapsible-body">
                ${renderDefinitionGrid([
                  ['thread_key', item.thread_key || 'none'],
                  ['workspace_execution_mode', item.workspace_execution_mode || 'full_auto'],
                  ['current_execution_mode', item.current_execution_mode || 'unknown'],
                  ['current_collaboration_mode', item.current_collaboration_mode || 'default'],
                  ['current_approval_policy', item.current_approval_policy || 'unknown'],
                  ['current_sandbox_policy', item.current_sandbox_policy || 'unknown'],
                  ['runtime_source', item.runtime_health_source || 'unknown'],
                  ['owner_checked_at', item.heartbeat_last_checked_at || 'n/a'],
                  ['owner_last_error', item.heartbeat_last_error || 'none'],
                  ['session_broken_reason', item.session_broken_reason || 'none'],
                  ['current_codex_thread_id', item.current_codex_thread_id || 'none'],
                  ['tui_active_codex_thread_id', item.tui_active_codex_thread_id || 'none'],
                  ['hcodex_path', item.hcodex_path || 'none'],
                  ['last_used_at', item.last_used_at || 'unknown'],
                ])}
              </div>
            ` : ''}
        </section>
      </div>
    </div>
  `
}

function transcriptEntriesForDelivery(entries, delivery) {
  return (entries || []).filter(entry => delivery === 'all' || entry.delivery === delivery)
}

function formatTranscriptEntry(entry) {
  const phase = entry.phase ? ` · ${entry.phase}` : ''
  const origin = entry.origin ? ` · ${entry.origin}` : ''
  return `
    <div class="transcript-entry">
      <div class="transcript-meta">${escapeHtml(entry.timestamp || 'unknown')}${escapeHtml(phase)}${escapeHtml(origin)}</div>
      <div>${escapeHtml(entry.text || '')}</div>
    </div>
  `
}

function renderTranscriptSection(entries, delivery, emptyLabel) {
  const filtered = transcriptEntriesForDelivery(entries, delivery)
  if (!filtered.length) {
    return `<div class="empty-state">${escapeHtml(emptyLabel)}</div>`
  }
  return `<div class="transcript-list">${filtered.map(formatTranscriptEntry).join('')}</div>`
}

function renderWorkspaceTranscript(threadKey) {
  const cache = transcriptCache(threadKey)
  if (cache.loading && !cache.loaded) {
    return '<div class="empty-state">Loading transcript…</div>'
  }
  if (cache.error) {
    return `<div class="hint">${escapeHtml(cache.error)}</div>`
  }
  if (!cache.loaded) {
    return `
      <div class="button-row">
        <button class="button button-secondary" onclick="loadTranscript(${JSON.stringify(threadKey)}, true)">Load Transcript</button>
      </div>
    `
  }
  return `
    <div class="button-row">
      <button class="button button-secondary" onclick="loadTranscript(${JSON.stringify(threadKey)}, true)">Refresh Transcript</button>
    </div>
    <div class="subpanel">
      <strong>Process Transcript</strong>
      ${renderTranscriptSection(cache.entries, 'process', 'No process entries.')}
    </div>
    <div class="subpanel">
      <strong>Final Transcript</strong>
      ${renderTranscriptSection(cache.entries, 'final', 'No final entries.')}
    </div>
  `
}

function sessionRunStatus(summary) {
  return summary?.run_status || 'idle'
}

function compactOriginList(summary) {
  const items = summary?.origins_seen || []
  return items.length ? items.join(', ') : 'none'
}

function formatSessionSummary(summary, threadKey) {
  const selected = sessionCache(threadKey).selectedSessionId === summary.session_id
  return `
    <div class="transcript-entry ${selected ? 'is-selected' : ''}">
      <div class="transcript-meta">
        <strong>${escapeHtml(summary.session_id || 'unknown-session')}</strong>
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
      <div class="button-row">
        <button class="button button-secondary btn-sm" onclick="selectWorkingSession(${JSON.stringify(threadKey)}, ${JSON.stringify(summary.session_id)})">View Records</button>
      </div>
    </div>
  `
}

function formatSessionRecord(record) {
  const meta = [
    record.timestamp || 'unknown',
    record.kind || 'unknown',
    record.origin || 'n/a',
    record.delivery || 'n/a',
    record.phase || 'n/a',
    record.source_ref || 'n/a',
  ]
  return `
    <div class="transcript-entry">
      <div class="transcript-meta">${meta.map(value => escapeHtml(value)).join(' · ')}</div>
      <div>${escapeHtml(record.text || '')}</div>
    </div>
  `
}

function renderWorkingSessionRecords(threadKey, sessionId) {
  const cache = sessionCache(threadKey)
  if (!sessionId) {
    return '<div class="empty-state">Select a session.</div>'
  }
  if (cache.recordLoadingBySessionId[sessionId] && !cache.recordsBySessionId[sessionId]) {
    return '<div class="empty-state">Loading session records…</div>'
  }
  const error = cache.recordErrorBySessionId[sessionId]
  if (error) {
    return `<div class="hint">${escapeHtml(error)}</div>`
  }
  const records = cache.recordsBySessionId[sessionId] || []
  if (!records.length) {
    return '<div class="empty-state">No records yet.</div>'
  }
  return `<div class="transcript-list">${records.map(formatSessionRecord).join('')}</div>`
}

function renderWorkingSessions(threadKey) {
  const cache = sessionCache(threadKey)
  if (cache.loading && !cache.loaded) {
    return '<div class="empty-state">Loading sessions…</div>'
  }
  if (cache.error) {
    return `<div class="hint">${escapeHtml(cache.error)}</div>`
  }
  if (!cache.loaded) {
    return `
      <div class="button-row">
        <button class="button button-secondary" onclick="loadWorkingSessions(${JSON.stringify(threadKey)}, true)">Load Sessions</button>
      </div>
    `
  }
  if (!cache.summaries.length) {
    return '<div class="empty-state">No sessions yet.</div>'
  }
  return `
    <div class="button-row">
      <button class="button button-secondary" onclick="loadWorkingSessions(${JSON.stringify(threadKey)}, true)">Refresh Sessions</button>
    </div>
    <div class="subpanel">
      <strong>Sessions</strong>
      <div class="transcript-list">${cache.summaries.map(summary => formatSessionSummary(summary, threadKey)).join('')}</div>
    </div>
    <div class="subpanel">
      <strong>Session Records</strong>
      ${renderWorkingSessionRecords(threadKey, cache.selectedSessionId)}
    </div>
  `
}

function renderRoute(route) {
  switch (route.page) {
    case 'attention':
      return renderAttentionPage()
    case 'workspaces':
      return renderWorkspacesPage()
    case 'workspace':
      return renderWorkspaceDetailPage(route.threadKey)
    case 'settings':
      return renderSettingsPage()
    case 'archive':
      return renderArchivePage()
    default:
      return renderOverviewPage()
  }
}

function renderApp() {
  syncDraftsFromData()
  reconcileDrafts()
  const route = appState.ui.route
  const root = document.getElementById('app')
  root.innerHTML = `
    <div class="app-shell">
      ${renderShellHeader(route)}
      <main class="content-shell">
        ${renderTopbar(route)}
        ${renderGlobalBanner(route)}
        ${renderRoute(route)}
      </main>
    </div>
  `
}

function scheduleRender() {
  if (renderScheduled) {
    return
  }
  renderScheduled = true
  window.setTimeout(() => {
    renderScheduled = false
    renderApp()
  }, 16)
}

function markObservabilityRefresh(threadKey) {
  if (!threadKey) {
    return
  }
  if (transcriptCache(threadKey).loaded || sessionCache(threadKey).loaded) {
    pendingObservabilityRefreshThreadKeys.add(threadKey)
  }
}

async function refreshPendingObservability() {
  const threadKeys = [...pendingObservabilityRefreshThreadKeys]
  pendingObservabilityRefreshThreadKeys.clear()
  await Promise.all(threadKeys.flatMap(threadKey => {
    const tasks = []
    if (transcriptCache(threadKey).loaded) {
      tasks.push(loadTranscript(threadKey, false))
    }
    if (sessionCache(threadKey).loaded) {
      tasks.push(loadWorkingSessions(threadKey, false))
    }
    return tasks
  }))
}

function scheduleObservabilityRefresh() {
  if (observabilityRefreshScheduled || !pendingObservabilityRefreshThreadKeys.size) {
    return
  }
  observabilityRefreshScheduled = true
  window.setTimeout(async () => {
    observabilityRefreshScheduled = false
    await refreshPendingObservability()
  }, 50)
}

function scheduleFullRefresh() {
  if (fullRefreshScheduled) {
    return
  }
  fullRefreshScheduled = true
  window.setTimeout(async () => {
    fullRefreshScheduled = false
    await refresh()
  }, 150)
}

function applyRuntimeEvent(payload) {
  if (!initialSnapshotLoaded || !payload?.kind) {
    scheduleFullRefresh()
    return
  }

  let shouldRender = false
  switch (payload.kind) {
    case 'setup_changed':
      if (payload.op === 'upsert' && payload.current) {
        appState.setup = payload.current
        shouldRender = true
      }
      break
    case 'runtime_health_changed':
      if (payload.op === 'upsert' && payload.current) {
        appState.health = payload.current
        shouldRender = true
      }
      break
    case 'managed_codex_changed':
      if (payload.op === 'remove') {
        appState.health = { ...(appState.health || {}), managed_codex: null }
        shouldRender = true
        break
      }
      if (payload.op === 'upsert' && payload.current) {
        appState.health = { ...(appState.health || {}), managed_codex: payload.current }
        shouldRender = true
      }
      break
    case 'workspace_state_changed': {
      const key = payload.key
      const existingIndex = typeof key === 'string' ? workspaceIndexByCwd(key) : -1
      const previousThreadKey = existingIndex >= 0 ? appState.workspaces[existingIndex]?.thread_key : null
      if (payload.op === 'remove') {
        removeArrayItem(appState.workspaces, existingIndex)
        markObservabilityRefresh(previousThreadKey)
        shouldRender = true
        break
      }
      if (payload.op === 'upsert' && payload.current) {
        upsertArrayItem(appState.workspaces, existingIndex, payload.current)
        markObservabilityRefresh(payload.current.thread_key || previousThreadKey)
        shouldRender = true
      }
      break
    }
    case 'archived_thread_changed': {
      const key = payload.key
      const existingIndex = typeof key === 'string' ? archivedThreadIndexByKey(key) : -1
      if (payload.op === 'remove') {
        removeArrayItem(appState.archived, existingIndex)
        shouldRender = true
        break
      }
      if (payload.op === 'upsert' && payload.current) {
        upsertArrayItem(appState.archived, existingIndex, payload.current)
        shouldRender = true
      }
      break
    }
    case 'thread_state_changed':
      markObservabilityRefresh(payload.key || payload.current?.thread_key || null)
      break
    case 'error':
      scheduleFullRefresh()
      return
    default:
      scheduleFullRefresh()
      return
  }

  if (shouldRender) {
    scheduleRender()
  }
  scheduleObservabilityRefresh()
}

async function fetchJson(url, options = {}, fallbackMessage = 'Request failed') {
  const response = await fetch(url, options)
  let data = null
  try {
    data = await response.json()
  } catch (_error) {
    data = null
  }
  if (!response.ok) {
    throw new Error(data?.error || fallbackMessage)
  }
  return data
}

async function refresh() {
  const [setup, health, workspaces, archived] = await Promise.all([
    fetchJson('/api/setup', {}, 'Setup fetch failed'),
    fetchJson('/api/runtime-health', {}, 'Runtime health fetch failed'),
    fetchJson('/api/workspaces', {}, 'Workspace fetch failed'),
    fetchJson('/api/archived-threads', {}, 'Archive fetch failed'),
  ])
  appState.setup = setup
  appState.health = health
  appState.workspaces = workspaces
  appState.archived = archived
  initialSnapshotLoaded = true
  scheduleRender()
  await Promise.all([refreshLoadedTranscripts(), refreshLoadedSessions()])
}

async function postRuntimeControlAction(threadKey, payload, failureText) {
  try {
    const data = await fetchJson(`/api/threads/${threadKey}/actions`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    }, failureText)
    openLaunchOutput(threadKey, data)
    await refresh()
    return data
  } catch (error) {
    alert(error instanceof Error ? error.message : failureText)
    return null
  }
}

function openLaunchOutput(threadKey, data) {
  workspacePanelState(threadKey).launch = true
  appState.ui.launchOutputs[threadKey] = JSON.stringify(data, null, 2)
  scheduleRender()
}

function updateResumeSessionDraft(threadKey, value) {
  appState.ui.resumeSessionDrafts[threadKey] = value
}

function updateSetupDraft(field, value) {
  appState.drafts.setup[field] = value
  if (field === 'authorizedUserIds') {
    appState.drafts.setup.dirtyAuthorizedUserIds = true
  }
}

function updateManagedCodexDraft(field, value) {
  const draft = appState.drafts.managedCodex
  draft[field] = value
  if (field === 'source') {
    draft.dirtySource = true
  }
  if (field === 'sourceRepo') {
    draft.dirtySourceRepo = true
  }
  if (field === 'sourceRsDir') {
    draft.dirtySourceRsDir = true
  }
  if (field === 'buildProfile') {
    draft.dirtyBuildProfile = true
  }
}

function toggleWorkspacePanel(threadKey, panelKey) {
  if (!WORKSPACE_PANEL_KEYS.includes(panelKey)) {
    return
  }
  const panels = workspacePanelState(threadKey)
  panels[panelKey] = !panels[panelKey]
  if (panels[panelKey] && panelKey === 'sessions' && !sessionCache(threadKey).loaded) {
    loadWorkingSessions(threadKey, false)
  }
  if (panels[panelKey] && panelKey === 'transcript' && !transcriptCache(threadKey).loaded) {
    loadTranscript(threadKey, false)
  }
  scheduleRender()
}

async function submitSetupForm(event) {
  event.preventDefault()
  appState.ui.setupStatus = 'Saving...'
  scheduleRender()
  try {
    const data = await fetchJson('/api/setup/telegram', {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        telegram_token: appState.drafts.setup.telegramToken,
        authorized_user_ids: appState.drafts.setup.authorizedUserIds
          .split(',')
          .map(x => x.trim())
          .filter(Boolean)
          .map(x => Number(x)),
      }),
    }, 'Save failed')
    appState.drafts.setup.telegramToken = ''
    appState.drafts.setup.dirtyAuthorizedUserIds = false
    appState.ui.setupStatus = data.restart_required
      ? 'Saved. Restart required before polling can start.'
      : 'Saved. Desktop runtime will retry polling automatically.'
    await refresh()
  } catch (error) {
    appState.ui.setupStatus = error instanceof Error ? error.message : 'Save failed'
    scheduleRender()
  }
  return false
}

async function updateManagedCodexPreference() {
  appState.ui.managedCodexStatus = 'Applying...'
  scheduleRender()
  try {
    const data = await fetchJson('/api/managed-codex/preference', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ source: appState.drafts.managedCodex.source }),
    }, 'Apply failed')
    appState.ui.managedCodexStatus = `Applied. Synced ${data.synced_workspaces} workspaces.`
    appState.drafts.managedCodex.dirtySource = false
    await refresh()
  } catch (error) {
    appState.ui.managedCodexStatus = error instanceof Error ? error.message : 'Apply failed'
    scheduleRender()
  }
}

async function refreshManagedCodexCache() {
  appState.ui.managedCodexStatus = 'Refreshing cache...'
  scheduleRender()
  try {
    const data = await fetchJson('/api/managed-codex/refresh-cache', { method: 'POST' }, 'Refresh failed')
    appState.ui.managedCodexStatus = `Cache refreshed: ${data.version || data.binary_path}`
    await refresh()
  } catch (error) {
    appState.ui.managedCodexStatus = error instanceof Error ? error.message : 'Refresh failed'
    scheduleRender()
  }
}

async function buildManagedCodexSource() {
  appState.ui.managedCodexStatus = 'Building source Codex...'
  scheduleRender()
  try {
    const data = await fetchJson('/api/managed-codex/build-source', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        source_repo: appState.drafts.managedCodex.sourceRepo.trim() || null,
        source_rs_dir: appState.drafts.managedCodex.sourceRsDir.trim() || null,
        build_profile: appState.drafts.managedCodex.buildProfile,
      }),
    }, 'Build failed')
    appState.ui.managedCodexStatus = `Source build ready: ${data.version || data.binary_path}`
    appState.drafts.managedCodex.dirtySourceRepo = false
    appState.drafts.managedCodex.dirtySourceRsDir = false
    appState.drafts.managedCodex.dirtyBuildProfile = false
    await refresh()
  } catch (error) {
    appState.ui.managedCodexStatus = error instanceof Error ? error.message : 'Build failed'
    scheduleRender()
  }
}

async function saveManagedCodexBuildDefaults() {
  appState.ui.managedCodexStatus = 'Saving build defaults...'
  scheduleRender()
  try {
    const data = await fetchJson('/api/managed-codex/build-defaults', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        source_repo: appState.drafts.managedCodex.sourceRepo.trim(),
        source_rs_dir: appState.drafts.managedCodex.sourceRsDir.trim(),
        build_profile: appState.drafts.managedCodex.buildProfile,
      }),
    }, 'Save defaults failed')
    appState.ui.managedCodexStatus = `Build defaults saved: ${data.build_defaults.build_profile}`
    appState.drafts.managedCodex.dirtySourceRepo = false
    appState.drafts.managedCodex.dirtySourceRsDir = false
    appState.drafts.managedCodex.dirtyBuildProfile = false
    await refresh()
  } catch (error) {
    appState.ui.managedCodexStatus = error instanceof Error ? error.message : 'Save defaults failed'
    scheduleRender()
  }
}

async function reconcileRuntimeOwner() {
  appState.ui.managedCodexStatus = 'Reconciling runtime owner...'
  scheduleRender()
  try {
    const data = await fetchJson('/api/runtime-owner/reconcile', { method: 'POST' }, 'Runtime owner reconcile failed')
    appState.ui.managedCodexStatus =
      `Reconciled ${data.report?.scanned_workspaces ?? 0} workspaces. Owner state: ${data.status?.state || 'unknown'}.`
    await refresh()
  } catch (error) {
    appState.ui.managedCodexStatus = error instanceof Error ? error.message : 'Runtime owner reconcile failed'
    scheduleRender()
  }
}

async function pickAndAddWorkspace() {
  appState.ui.addWorkspaceStatus = 'Waiting for workspace selection...'
  scheduleRender()
  try {
    const data = await fetchJson('/api/workspaces/pick-and-add', { method: 'POST' }, 'Add workspace failed')
    if (data.cancelled) {
      appState.ui.addWorkspaceStatus = 'Workspace selection cancelled.'
      scheduleRender()
      return
    }
    if (data.probe_report) {
      appState.ui.addWorkspaceStatus = data.probe_report
      if (data.thread_key) {
        await refresh()
      } else {
        scheduleRender()
      }
      return
    }
    const label = workspacePrimaryLabel(data)
    appState.ui.addWorkspaceStatus = data.created
      ? `Added workspace: ${label}`
      : `Workspace already managed: ${label}`
    await refresh()
  } catch (error) {
    appState.ui.addWorkspaceStatus = error instanceof Error ? error.message : 'Add workspace failed'
    scheduleRender()
  }
}

async function updateExecutionMode(threadKey) {
  const executionMode = appState.executionModeDrafts[threadKey] || workspaceByThreadKey(threadKey)?.workspace_execution_mode
  if (!executionMode) {
    alert('Pick an execution mode first')
    return
  }
  const data = await postRuntimeControlAction(
    threadKey,
    { action: 'set_workspace_execution_mode', execution_mode: executionMode },
    'Execution mode update failed',
  )
  if (data) {
    delete appState.executionModeDrafts[threadKey]
  }
}

async function updateCollaborationMode(threadKey) {
  const mode = appState.collaborationModeDrafts[threadKey] || workspaceByThreadKey(threadKey)?.current_collaboration_mode || 'default'
  const data = await postRuntimeControlAction(
    threadKey,
    { action: 'set_thread_collaboration_mode', mode },
    'Collaboration mode update failed',
  )
  if (data) {
    delete appState.collaborationModeDrafts[threadKey]
  }
}

async function interruptRunningTurn(threadKey) {
  await postRuntimeControlAction(
    threadKey,
    { action: 'interrupt_running_turn' },
    'Interrupt request failed',
  )
}

async function launchHcodexNew(threadKey) {
  await postRuntimeControlAction(
    threadKey,
    { action: 'launch_local_session', target: 'new' },
    'Launch failed',
  )
}

async function launchHcodexContinueCurrent(threadKey) {
  await postRuntimeControlAction(
    threadKey,
    { action: 'launch_local_session', target: 'continue_current' },
    'Launch failed',
  )
}

async function launchHcodexResume(threadKey) {
  const sessionId = (appState.ui.resumeSessionDrafts[threadKey] || '').trim()
  if (!sessionId) {
    alert('Enter a session id first')
    return
  }
  await launchHcodexResumeWithSession(threadKey, sessionId)
}

async function launchHcodexResumeWithSession(threadKey, sessionId) {
  await postRuntimeControlAction(
    threadKey,
    { action: 'launch_local_session', target: 'resume', session_id: sessionId },
    'Launch failed',
  )
}

async function startFreshSession(threadKey) {
  await postRuntimeControlAction(
    threadKey,
    { action: 'start_fresh_session' },
    'Start fresh session failed',
  )
}

async function repairSessionBinding(threadKey) {
  await postRuntimeControlAction(
    threadKey,
    { action: 'repair_session_binding' },
    'Session repair failed',
  )
}

async function repairContinuity(threadKey, _bindingStatus, adoptionPending) {
  if (adoptionPending) {
    await adoptTuiSession(threadKey)
    return
  }
  await repairSessionBinding(threadKey)
}

async function loadTranscript(threadKey, userInitiated = false) {
  const cache = transcriptCache(threadKey)
  if (cache.loading) {
    return
  }
  cache.loading = true
  cache.error = null
  if (userInitiated) {
    scheduleRender()
  }
  try {
    const data = await fetchJson(`/api/threads/${threadKey}/transcript?delivery=all&limit=40`, {}, 'Transcript fetch failed')
    cache.entries = data
    cache.loaded = true
  } catch (error) {
    cache.error = error instanceof Error ? error.message : 'Transcript fetch failed'
    cache.entries = []
    cache.loaded = false
  } finally {
    cache.loading = false
    scheduleRender()
  }
}

async function loadWorkingSessions(threadKey, userInitiated = false) {
  const cache = sessionCache(threadKey)
  if (cache.loading) {
    return
  }
  cache.loading = true
  cache.error = null
  if (userInitiated) {
    scheduleRender()
  }
  try {
    const data = await fetchJson(`/api/threads/${threadKey}/sessions`, {}, 'Session fetch failed')
    cache.summaries = data
    cache.loaded = true
    const sessionIds = new Set(cache.summaries.map(item => item.session_id))
    if (!cache.selectedSessionId || !sessionIds.has(cache.selectedSessionId)) {
      cache.selectedSessionId = cache.summaries[0]?.session_id || null
    }
    if (cache.selectedSessionId) {
      await loadWorkingSessionRecords(threadKey, cache.selectedSessionId, false)
    }
  } catch (error) {
    cache.error = error instanceof Error ? error.message : 'Session fetch failed'
    cache.summaries = []
    cache.loaded = false
  } finally {
    cache.loading = false
    scheduleRender()
  }
}

async function loadWorkingSessionRecords(threadKey, sessionId, userInitiated = false) {
  const cache = sessionCache(threadKey)
  if (!sessionId || cache.recordLoadingBySessionId[sessionId]) {
    return
  }
  cache.recordLoadingBySessionId[sessionId] = true
  delete cache.recordErrorBySessionId[sessionId]
  if (userInitiated) {
    scheduleRender()
  }
  try {
    const data = await fetchJson(`/api/threads/${threadKey}/sessions/${encodeURIComponent(sessionId)}/records`, {}, 'Session records fetch failed')
    cache.recordsBySessionId[sessionId] = data
  } catch (error) {
    cache.recordErrorBySessionId[sessionId] = error instanceof Error ? error.message : 'Session records fetch failed'
    cache.recordsBySessionId[sessionId] = []
  } finally {
    cache.recordLoadingBySessionId[sessionId] = false
    scheduleRender()
  }
}

async function selectWorkingSession(threadKey, sessionId) {
  const cache = sessionCache(threadKey)
  cache.selectedSessionId = sessionId
  scheduleRender()
  await loadWorkingSessionRecords(threadKey, sessionId, true)
}

async function refreshLoadedTranscripts() {
  const threadKeys = Object.entries(appState.transcripts)
    .filter(([, cache]) => cache.loaded)
    .map(([threadKey]) => threadKey)
  await Promise.all(threadKeys.map(threadKey => loadTranscript(threadKey, false)))
}

async function refreshLoadedSessions() {
  const threadKeys = Object.entries(appState.sessions)
    .filter(([, cache]) => cache.loaded)
    .map(([threadKey]) => threadKey)
  await Promise.all(threadKeys.map(threadKey => loadWorkingSessions(threadKey, false)))
}

async function showLaunchConfig(threadKey) {
  try {
    const data = await fetchJson(`/api/workspaces/${threadKey}/launch-config`, {}, 'Launch config fetch failed')
    openLaunchOutput(threadKey, data)
  } catch (error) {
    alert(error instanceof Error ? error.message : 'Launch config fetch failed')
  }
}

async function openWorkspace(threadKey) {
  try {
    await fetchJson(`/api/workspaces/${threadKey}/open`, { method: 'POST' }, 'Open workspace failed')
  } catch (error) {
    alert(error instanceof Error ? error.message : 'Open workspace failed')
  }
}

async function repairRuntime(threadKey) {
  try {
    await fetchJson(`/api/workspaces/${threadKey}/repair-runtime`, { method: 'POST' }, 'Runtime repair failed')
    await refresh()
  } catch (error) {
    alert(error instanceof Error ? error.message : 'Runtime repair failed')
  }
}

async function adoptTuiSession(threadKey) {
  try {
    await fetchJson(`/api/threads/${threadKey}/adopt-tui`, { method: 'POST' }, 'Adopt TUI failed')
    await refresh()
  } catch (error) {
    alert(error instanceof Error ? error.message : 'Adopt TUI failed')
  }
}

async function rejectTuiSession(threadKey) {
  try {
    await fetchJson(`/api/threads/${threadKey}/reject-tui`, { method: 'POST' }, 'Reject TUI failed')
    await refresh()
  } catch (error) {
    alert(error instanceof Error ? error.message : 'Reject TUI failed')
  }
}

async function archiveThread(threadKey, label) {
  if (!window.confirm(`Archive workspace "${label}"? This only changes local threadBridge state and Telegram topic state.`)) {
    return
  }
  try {
    await fetchJson(`/api/threads/${threadKey}/archive`, { method: 'POST' }, 'Archive failed')
    if (appState.ui.route.page === 'workspace' && appState.ui.route.threadKey === threadKey) {
      navigateToHash('#/archive')
    }
    await refresh()
  } catch (error) {
    alert(error instanceof Error ? error.message : 'Archive failed')
  }
}

async function restoreThread(threadKey, label) {
  if (!window.confirm(`Restore archived workspace "${label}"? This restores local metadata and Telegram topic state only.`)) {
    return
  }
  try {
    await fetchJson(`/api/threads/${threadKey}/restore`, { method: 'POST' }, 'Restore failed')
    await refresh()
  } catch (error) {
    alert(error instanceof Error ? error.message : 'Restore failed')
  }
}

async function purgeArchivedThreads() {
  if (!window.confirm('Purge all archived threadBridge data? This cannot be undone.')) {
    return
  }
  try {
    const data = await fetchJson('/api/archived-threads/purge', { method: 'POST' }, 'Purge archived threads failed')
    await refresh()
    alert(`Purged ${data.purged} archived thread record(s).`)
  } catch (error) {
    alert(error instanceof Error ? error.message : 'Purge archived threads failed')
  }
}

window.addEventListener('hashchange', () => {
  appState.ui.route = parseRoute(window.location.hash)
  scheduleRender()
})

refresh()
const events = new EventSource('/api/events')

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
      applyRuntimeEvent(JSON.parse(event.data))
    } catch (error) {
      console.warn(`management SSE ${eventName} parse failed`, error)
      scheduleFullRefresh()
    }
  })
}

events.addEventListener('error', event => {
  console.warn('management SSE error event', event)
  scheduleFullRefresh()
})

events.onerror = error => {
  console.warn('management SSE transport error', error)
  scheduleFullRefresh()
}
