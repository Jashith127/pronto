const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;

const body = document.body;
const backdrop = document.querySelector('#backdrop');
const chrome = document.querySelector('#chrome');
const panel = document.querySelector('#panel');
const toast = document.querySelector('#toast');
const statusRow = document.querySelector('#search-status');
const messageEl = document.querySelector('#search-message');
const queryLabel = document.querySelector('#search-query-label');
const emptyEl = document.querySelector('#search-empty');
const nodesEl = document.querySelector('#search-nodes');
const cancelBtn = document.querySelector('#search-cancel');
const finishBtn = document.querySelector('#search-finish');
const panelClose = document.querySelector('#panel-close');
const plots = [];

const INTERIM_WARNING = 'Fetching a grounded answer…';

let uiMode = 'idle';
let currentQuery = '';

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

function hostOf(url) {
  try {
    return new URL(url).hostname.replace(/^www\./, '');
  } catch (_) {
    return '';
  }
}

function normalizeForCompare(value) {
  return String(value || '')
    .toLowerCase()
    .replace(/[^a-z0-9\s]/g, '')
    .replace(/\s+/g, ' ')
    .trim();
}

function headingDuplicatesQuery(heading, query) {
  const h = normalizeForCompare(heading);
  const q = normalizeForCompare(query);
  if (!h || !q) return false;
  if (h === q) return true;
  if (h.includes(q) || q.includes(h)) return true;
  if (h.startsWith('results for')) return true;
  if (h === 'search results' || h === 'answer' || h === 'summary' || h === 'overview') {
    return true;
  }
  return false;
}

function formatAnswerText(text) {
  const escaped = escapeHtml(text);
  return escaped.replace(/\[(\d+)\]/g, '<sup class="cite">$1</sup>');
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

function renderSourceList(node, open) {
  const items = node.items || [];
  const rows = items.map(item => `
    <li>
      <button type="button" class="src-item" data-action="open_url" data-value="${escapeAttr(item.url)}">
        <span class="src-index">${escapeHtml(item.index)}</span>
        <span class="src-body">
          <strong>${escapeHtml(item.title)}</strong>
          ${item.snippet ? `<em>${escapeHtml(item.snippet)}</em>` : ''}
          <span class="src-host">${escapeHtml(hostOf(item.url))}</span>
        </span>
      </button>
    </li>`).join('');
  return `<details class="search-sources"${open ? ' open' : ''}>` +
    `<summary>Sources <span class="src-count">${items.length}</span></summary>` +
    `<ol>${rows}</ol></details>`;
}

function renderNode(node, context) {
  switch (node.type) {
    case 'heading':
      if (headingDuplicatesQuery(node.text, context.query)) return '';
      return `<h2 class="search-node-heading">${escapeHtml(node.text)}</h2>`;
    case 'text':
      return `<p class="search-node-text">${formatAnswerText(node.text)}</p>`;
    case 'divider':
      return `<hr class="search-node-divider" />`;
    case 'image_frame':
      return `<figure class="search-image-frame loading">
        <div class="image-skeleton" aria-hidden="true"></div>
        <img data-src="${escapeAttr(node.src)}" alt="${escapeAttr(node.alt || '')}" decoding="async" />
        ${node.caption ? `<figcaption>${escapeHtml(node.caption)}</figcaption>` : ''}
      </figure>`;
    case 'youtube': {
      const embed = youtubeEmbed(node.url);
      if (!embed) return `<p class="search-node-text">${escapeHtml(node.title || node.url)}</p>`;
      return `<div class="search-youtube"><iframe src="${escapeAttr(embed)}" title="${escapeAttr(node.title || 'YouTube video')}" allow="accelerometer; autoplay; clipboard-write; encrypted-media; gyroscope; picture-in-picture" allowfullscreen loading="lazy"></iframe></div>`;
    }
    case 'button':
      context.buttons.push(node);
      return '';
    case 'source_list':
      return renderSourceList(node, context.openSources);
    case 'table':
      return renderTable(node);
    case 'chart':
      return `<div class="search-chart" data-chart="${escapeAttr(JSON.stringify(node))}"></div>`;
    default:
      return '';
  }
}

const imageObjectUrls = new Set();

function revokeImageObjectUrls() {
  imageObjectUrls.forEach(url => {
    try { URL.revokeObjectURL(url); } catch (_) { /* ignore */ }
  });
  imageObjectUrls.clear();
}

async function loadSearchImage(img, url) {
  const frame = img.closest('.search-image-frame');
  if (!url || !frame) return;

  const markLoaded = () => {
    frame.classList.remove('loading');
    frame.classList.add('loaded');
  };
  const markError = () => {
    frame.classList.remove('loading');
    frame.classList.add('error');
  };

  try {
    const payload = await invoke('fetch_search_image', { url });
    const bytes = payload?.data instanceof Uint8Array
      ? payload.data
      : new Uint8Array(payload?.data || []);
    const mime = payload?.mime || 'image/jpeg';
    const blob = new Blob([bytes], { type: mime });
    const objectUrl = URL.createObjectURL(blob);
    imageObjectUrls.add(objectUrl);
    img.addEventListener('load', markLoaded, { once: true });
    img.addEventListener('error', markError, { once: true });
    img.src = objectUrl;
    return;
  } catch (_) { /* fall through to direct load */ }

  img.referrerPolicy = 'origin';
  img.addEventListener('load', markLoaded, { once: true });
  img.addEventListener('error', markError, { once: true });
  img.src = url;
}

function mountImages(root) {
  root.querySelectorAll('.search-image-frame img[data-src]').forEach(img => {
    const url = img.getAttribute('data-src');
    if (url) loadSearchImage(img, url);
  });
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
        stroke: '#f4f4ef',
        width: 2,
      }))];
      const data = [
        node.labels.map((_, index) => index),
        ...node.datasets.map(dataset => dataset.data.map(Number)),
      ];
      const plot = new uPlot({
        width: Math.max(280, container.clientWidth || 560),
        height: 220,
        series,
        scales: { x: { time: false } },
        axes: [
          {
            stroke: '#a8aba3',
            values: (_u, splits) => splits.map(split => node.labels[split] ?? ''),
          },
          { stroke: '#a8aba3' },
        ],
      }, data, container);
      plots.push(plot);
    } catch (_) {
      container.outerHTML = renderChartFallback(node);
    }
  });
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

function setQueryLabel(query) {
  currentQuery = query && query.trim() ? query.trim() : '';
  if (currentQuery) {
    queryLabel.hidden = false;
    queryLabel.textContent = currentQuery;
  } else {
    queryLabel.hidden = true;
    queryLabel.textContent = '';
  }
}

function setStatus(kind, text) {
  if (!text) {
    statusRow.hidden = true;
    messageEl.textContent = '';
    statusRow.classList.remove('error');
    return;
  }
  statusRow.hidden = false;
  statusRow.classList.toggle('error', kind === 'error');
  messageEl.textContent = text;
}

function resetResultSurface() {
  destroyPlots();
  revokeImageObjectUrls();
  emptyEl.hidden = false;
  nodesEl.hidden = true;
  nodesEl.innerHTML = '';
  setQueryLabel('');
}

function shouldShowWarning(warning) {
  if (!warning || !warning.trim()) return false;
  if (warning === INTERIM_WARNING) return false;
  return true;
}

function paintResult(payload) {
  destroyPlots();
  const nodes = payload.ui?.nodes || [];
  const query = payload.query || '';
  const hasAnswer = nodes.some(node => node.type === 'heading' || node.type === 'text');
  const sourceNode = nodes.find(node => node.type === 'source_list');

  const context = {
    buttons: [],
    openSources: !hasAnswer,
    query,
  };
  const parts = nodes.map(node => renderNode(node, context)).filter(Boolean);
  if (context.buttons.length) {
    parts.push(`<div class="node-actions">${context.buttons.map(node =>
      `<button type="button" class="search-node-button" data-action="${escapeAttr(node.action)}" data-value="${escapeAttr(node.value)}">${escapeHtml(node.label)}</button>`
    ).join('')}</div>`);
  }

  emptyEl.hidden = true;
  nodesEl.hidden = false;
  const warning = shouldShowWarning(payload.warning)
    ? `<p class="search-warning">${escapeHtml(payload.warning)}</p>`
    : '';
  nodesEl.innerHTML = `${warning}${parts.join('')}`;
  mountImages(nodesEl);
  mountCharts(nodesEl);

  setQueryLabel(query);
  setStatus('', '');
  nodesEl.scrollTop = 0;
}

async function enterListening() {
  uiMode = 'listening';
  body.className = 'mode-listening';
  chrome.classList.remove('processing');
  showPanel(false);
  showBackdrop(false);
  resetResultSurface();
  setStatus('', '');
  showChrome();
  cancelBtn.hidden = false;
  finishBtn.hidden = false;
  await setNativeStage('pill');
}

async function enterSearching() {
  if (uiMode === 'searching' || uiMode === 'panel') return;
  uiMode = 'searching';
  body.className = 'mode-searching';
  showPanel(false);
  showBackdrop(false);
  chrome.classList.add('processing');
  showChrome();
  cancelBtn.hidden = true;
  finishBtn.hidden = true;
  await setNativeStage('pill');
}

async function showResultPanel(payload) {
  paintResult(payload);
  uiMode = 'panel';
  body.className = 'mode-panel';
  hideChrome();
  await setNativeStage('stage');
  showBackdrop(true);
  showPanel(true);
}

async function enterIdle() {
  uiMode = 'idle';
  body.className = 'mode-idle';
  hideChrome();
  chrome.classList.remove('processing');
  showPanel(false);
  showBackdrop(false);
  resetResultSurface();
  setStatus('', '');
}

async function enterError(status) {
  uiMode = 'panel';
  body.className = 'mode-panel';
  hideChrome();
  resetResultSurface();
  setQueryLabel(status?.query || '');
  setStatus('error', status?.message || 'Search failed');
  await setNativeStage('stage');
  showBackdrop(true);
  showPanel(true);
}

function renderStatus(status) {
  const phase = status?.phase || 'idle';

  if (phase === 'listening') {
    enterListening();
  } else if (phase === 'searching') {
    enterSearching();
  } else if (phase === 'idle') {
    enterIdle();
  } else if (phase === 'error') {
    enterError(status);
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
  if (event.payload?.query) currentQuery = event.payload.query;
});
listen('search-result', event => {
  const payload = event.payload || {};
  if (payload.warning === INTERIM_WARNING) {
    if (payload.query) currentQuery = payload.query;
    enterSearching();
    return;
  }
  showResultPanel(payload);
});
listen('search-error', event => {
  showToast(String(event.payload), true);
});

call('get_search_status').then(status => {
  if (!status || status.phase === 'idle' || status.phase === 'complete' || status.phase === 'error') {
    enterIdle();
    return;
  }
  renderStatus(status);
}).catch(() => enterIdle());
