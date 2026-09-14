const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;

const body = document.body;
const backdrop = document.querySelector('#backdrop');
const chrome = document.querySelector('#chrome');
const signal = document.querySelector('#signal');
const panel = document.querySelector('#panel');
const toast = document.querySelector('#toast');
const phaseEl = document.querySelector('#search-phase');
const messageEl = document.querySelector('#search-message');
const queryWrap = document.querySelector('#search-query');
const queryText = document.querySelector('#search-query-text');
const emptyEl = document.querySelector('#search-empty');
const nodesEl = document.querySelector('#search-nodes');
const cancelBtn = document.querySelector('#search-cancel');
const finishBtn = document.querySelector('#search-finish');
const panelClose = document.querySelector('#panel-close');
const plots = [];

let uiMode = 'idle';
let pendingResult = null;
let flyInFlight = false;
const reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

function showToast(text, error = false) {
  toast.textContent = text;
  toast.className = `toast show${error ? ' error' : ''}`;
  clearTimeout(showToast.timer);
  showToast.timer = setTimeout(() => { toast.className = 'toast'; }, 3200);
}

async function call(command, args = {}) {
  try {
    return await invoke(command, args);
  } catch (error) {
    showToast(String(error), true);
    throw error;
  }
}

function escapeHtml(value) {
  const node = document.createElement('div');
  node.textContent = value == null ? '' : String(value);
  return node.innerHTML;
}

function escapeAttr(value) {
  return escapeHtml(value).replaceAll('"', '&quot;');
}

function destroyPlots() {
  while (plots.length) {
    const plot = plots.pop();
    try { plot.destroy(); } catch (_) { /* ignore */ }
  }
}

function youtubeEmbed(url) {
  try {
    const parsed = new URL(url);
    if (parsed.hostname.includes('youtu.be')) {
      const id = parsed.pathname.replace(/^\//, '').split('/')[0];
      return id ? `https://www.youtube-nocookie.com/embed/${id}` : null;
    }
    if (parsed.hostname.includes('youtube.com')) {
      const id = parsed.searchParams.get('v')
        || (parsed.pathname.startsWith('/embed/') ? parsed.pathname.split('/')[2] : null)
        || (parsed.pathname.startsWith('/shorts/') ? parsed.pathname.split('/')[2] : null);
      return id ? `https://www.youtube-nocookie.com/embed/${id}` : null;
    }
  } catch (_) { /* ignore */ }
  return null;
}

function renderChartFallback(node) {
  const columns = ['Label', ...(node.datasets || []).map(dataset => dataset.label || 'Series')];
  const rows = (node.labels || []).map((label, index) => [
    label,
    ...(node.datasets || []).map(dataset => String((dataset.data || [])[index] ?? '')),
  ]);
  return renderTable({ columns, rows });
}

function renderTable(node) {
  const head = `<tr>${(node.columns || []).map(column => `<th>${escapeHtml(column)}</th>`).join('')}</tr>`;
  const bodyHtml = (node.rows || []).map(row => `<tr>${row.map(cell => `<td>${escapeHtml(cell)}</td>`).join('')}</tr>`).join('');
  return `<div class="search-table-wrap"><table class="search-table"><thead>${head}</thead><tbody>${bodyHtml}</tbody></table></div>`;
}

function renderNode(node) {
  switch (node.type) {
    case 'heading':
      return `<h2 class="search-node-heading">${escapeHtml(node.text)}</h2>`;
    case 'text':
      return `<p class="search-node-text">${escapeHtml(node.text)}</p>`;
    case 'divider':
      return `<hr class="search-node-divider" />`;
    case 'image_frame':
      return `<figure class="search-image-frame"><img src="${escapeAttr(node.src)}" alt="${escapeAttr(node.alt || '')}" loading="lazy" referrerpolicy="no-referrer" />${node.caption ? `<figcaption>${escapeHtml(node.caption)}</figcaption>` : ''}</figure>`;
    case 'youtube': {
      const embed = youtubeEmbed(node.url);
      if (!embed) return `<p class="search-node-text">${escapeHtml(node.title || node.url)}</p>`;
      return `<div class="search-youtube"><iframe src="${escapeAttr(embed)}" title="${escapeAttr(node.title || 'YouTube video')}" allow="accelerometer; autoplay; clipboard-write; encrypted-media; gyroscope; picture-in-picture" allowfullscreen loading="lazy"></iframe></div>`;
    }
    case 'button':
      return `<button type="button" class="search-node-button" data-action="${escapeAttr(node.action)}" data-value="${escapeAttr(node.value)}">${escapeHtml(node.label)}</button>`;
    case 'source_list': {
      const items = (node.items || []).map(item => `<li><span class="index">${escapeHtml(item.index)}</span><button type="button" data-action="open_url" data-value="${escapeAttr(item.url)}"><strong>${escapeHtml(item.title)}</strong>${item.snippet ? `<em>${escapeHtml(item.snippet)}</em>` : ''}</button></li>`).join('');
      return `<ol class="search-sources">${items}</ol>`;
    }
    case 'table':
      return renderTable(node);
    case 'chart':
      return `<div class="search-chart" data-chart="${escapeAttr(JSON.stringify(node))}"></div>`;
    default:
      return '';
  }
}

function mountCharts(root) {
  root.querySelectorAll('[data-chart]').forEach(container => {
    let node;
    try {
      node = JSON.parse(container.getAttribute('data-chart') || '{}');
    } catch (_) {
      container.outerHTML = renderChartFallback({});
      return;
    }
    if (!window.uPlot || !node.labels?.length || !node.datasets?.length) {
      container.outerHTML = renderChartFallback(node);
      return;
    }
    try {
      const series = [{ label: 'Label' }, ...node.datasets.map(dataset => ({
        label: dataset.label || 'Series',
        stroke: dataset.label?.toLowerCase().includes('b') ? '#7dff9a' : '#f4337a',
        width: 2,
        fill: (node.chart_type || node.chartType) === 'bar' ? 'rgba(244, 51, 122, 0.18)' : undefined,
      }))];
      const data = [
        node.labels.map((_, index) => index),
        ...node.datasets.map(dataset => dataset.data.map(Number)),
      ];
      const plot = new uPlot({
        width: Math.max(280, container.clientWidth || 640),
        height: 220,
        series,
        scales: { x: { time: false } },
        axes: [
          {
            stroke: '#b8bbb4',
            values: (_u, splits) => splits.map(split => node.labels[split] ?? ''),
          },
          { stroke: '#b8bbb4' },
        ],
      }, data, container);
      plots.push(plot);
    } catch (_) {
      container.outerHTML = renderChartFallback(node);
    }
  });
}

function setSignalShape(shape) {
  signal.classList.toggle('linear', shape === 'linear');
  signal.classList.toggle('radial', shape === 'radial');
}

async function setNativeStage(stage) {
  try {
    await invoke('set_search_overlay_stage', { stage });
  } catch (_) { /* backend may already own the stage */ }
}

function showChrome() {
  chrome.hidden = false;
}

function hideChrome() {
  chrome.hidden = true;
  chrome.classList.remove('morphing', 'flying', 'expand', 'pill', 'orb');
}

function showBackdrop(on) {
  if (on) {
    backdrop.hidden = false;
    requestAnimationFrame(() => backdrop.classList.add('visible'));
  } else {
    backdrop.classList.remove('visible');
    backdrop.hidden = true;
  }
}

function showPanel(on) {
  if (on) {
    panel.hidden = false;
    requestAnimationFrame(() => panel.classList.add('visible'));
  } else {
    panel.classList.remove('visible');
    panel.hidden = true;
  }
}

function resetResultSurface() {
  destroyPlots();
  emptyEl.hidden = false;
  nodesEl.hidden = true;
  nodesEl.innerHTML = '';
  queryWrap.hidden = true;
  queryText.textContent = '';
}

function paintResult(payload) {
  destroyPlots();
  emptyEl.hidden = true;
  nodesEl.hidden = false;
  const warning = payload.warning
    ? `<p class="search-warning">${escapeHtml(payload.warning)}</p>`
    : '';
  const nodes = (payload.ui?.nodes || []).map(renderNode).join('');
  nodesEl.innerHTML = `${warning}${nodes}`;
  mountCharts(nodesEl);
  if (payload.query) {
    queryWrap.hidden = false;
    queryText.textContent = payload.query;
  }
}

function wait(ms) {
  return new Promise(resolve => setTimeout(resolve, ms));
}

async function enterListening() {
  uiMode = 'listening';
  flyInFlight = false;
  pendingResult = null;
  body.className = 'mode-listening';
  chrome.className = 'search-chrome pill';
  setSignalShape('linear');
  showPanel(false);
  showBackdrop(false);
  resetResultSurface();
  showChrome();
  cancelBtn.hidden = false;
  finishBtn.hidden = false;
  await setNativeStage('pill');
}

async function enterSearching() {
  if (uiMode === 'searching' || uiMode === 'panel') return;
  uiMode = 'searching';
  showPanel(false);
  showChrome();
  cancelBtn.hidden = true;
  finishBtn.hidden = true;

  // Expand the native window first so the orb has room to fly later, while
  // the chrome still paints as the bottom pill.
  body.className = 'mode-listening';
  await setNativeStage('stage');
  showBackdrop(true);

  // Liquid morph: enable transitions, then switch to orb geometry.
  chrome.classList.add('morphing');
  await wait(16);
  body.className = 'mode-searching';
  await wait(reduceMotion ? 0 : 180);
  setSignalShape('radial');
  chrome.classList.remove('pill');
  chrome.classList.add('orb');
  await wait(reduceMotion ? 0 : 280);
  chrome.classList.remove('morphing');
}

async function flyToPanel(payload) {
  if (flyInFlight) {
    pendingResult = payload;
    paintResult(payload);
    return;
  }
  flyInFlight = true;
  pendingResult = payload;
  paintResult(payload);

  if (uiMode !== 'searching' && uiMode !== 'panel') {
    await enterSearching();
  }

  showBackdrop(true);

  if (reduceMotion) {
    hideChrome();
    showPanel(true);
    uiMode = 'panel';
    flyInFlight = false;
    return;
  }

  chrome.classList.add('flying');
  await wait(40);
  chrome.classList.add('expand');
  await wait(420);
  hideChrome();
  showPanel(true);
  uiMode = 'panel';
  flyInFlight = false;
  if (pendingResult && pendingResult !== payload) {
    paintResult(pendingResult);
  }
}

async function enterIdle() {
  uiMode = 'idle';
  flyInFlight = false;
  pendingResult = null;
  body.className = 'mode-idle';
  hideChrome();
  showPanel(false);
  showBackdrop(false);
  resetResultSurface();
  setSignalShape('linear');
}

function renderStatus(status) {
  const phase = status?.phase || 'idle';
  phaseEl.textContent = phase;
  phaseEl.className = `panel-phase ${phase}`;
  messageEl.textContent = status?.message || '';

  if (status?.query) {
    queryWrap.hidden = false;
    queryText.textContent = status.query;
  }

  if (phase === 'listening') {
    cancelBtn.hidden = false;
    finishBtn.hidden = false;
    enterListening();
  } else if (phase === 'searching') {
    cancelBtn.hidden = true;
    finishBtn.hidden = true;
    if (uiMode === 'listening' || uiMode === 'idle' || uiMode === 'pill') {
      enterSearching();
    }
  } else if (phase === 'idle') {
    enterIdle();
  } else if (phase === 'error') {
    cancelBtn.hidden = true;
    finishBtn.hidden = true;
    // Keep the stage up so the error toast / message is visible; blur still dismisses.
    showBackdrop(true);
    showPanel(true);
    emptyEl.hidden = false;
    nodesEl.hidden = true;
    uiMode = 'panel';
    hideChrome();
  } else if (phase === 'complete') {
    cancelBtn.hidden = true;
    finishBtn.hidden = true;
  }
}

nodesEl.addEventListener('click', async event => {
  const button = event.target.closest('[data-action]');
  if (!button) return;
  const action = button.getAttribute('data-action');
  const value = button.getAttribute('data-value') || '';
  if (action === 'open_url') {
    await call('open_search_result', { url: value });
  } else if (action === 'copy') {
    try {
      await navigator.clipboard.writeText(value);
      showToast('Copied');
    } catch (error) {
      showToast(String(error), true);
    }
  } else if (action === 'insert') {
    showToast('Insert is available from dictation; copy instead for search answers.');
    try {
      await navigator.clipboard.writeText(value);
    } catch (_) { /* ignore */ }
  }
});

finishBtn.addEventListener('click', async event => {
  event.stopPropagation();
  renderStatus(await call('stop_search_recording'));
});

cancelBtn.addEventListener('click', async event => {
  event.stopPropagation();
  renderStatus(await call('cancel_search'));
});

panelClose.addEventListener('click', async event => {
  event.stopPropagation();
  await call('dismiss_search_overlay');
  await enterIdle();
});

backdrop.addEventListener('click', async () => {
  await call('dismiss_search_overlay');
  await enterIdle();
});

panel.addEventListener('click', event => {
  event.stopPropagation();
});

chrome.addEventListener('click', event => {
  event.stopPropagation();
});

document.addEventListener('keydown', async event => {
  if (event.key === 'Escape') {
    await call('dismiss_search_overlay');
    await enterIdle();
  }
});

listen('search-status', event => renderStatus(event.payload));
listen('search-query', event => {
  if (event.payload?.query) {
    queryWrap.hidden = false;
    queryText.textContent = event.payload.query;
  }
});
listen('search-result', event => {
  const interim = event.payload.warning === 'Fetching a grounded answer…';
  renderStatus({
    phase: interim ? 'searching' : 'complete',
    message: interim ? 'Writing grounded answer…' : (event.payload.warning || 'Answer ready'),
    query: event.payload.query,
  });
  flyToPanel(event.payload);
});
listen('search-error', event => {
  showToast(String(event.payload), true);
});

call('get_search_status').then(status => {
  // Overlay should stay hidden while idle — never present at rest.
  if (!status || status.phase === 'idle' || status.phase === 'complete' || status.phase === 'error') {
    enterIdle();
    return;
  }
  renderStatus(status);
}).catch(() => enterIdle());
