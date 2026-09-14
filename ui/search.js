const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;
const toast = document.querySelector('#toast');
const phaseEl = document.querySelector('#search-phase');
const messageEl = document.querySelector('#search-message');
const queryWrap = document.querySelector('#search-query');
const queryText = document.querySelector('#search-query-text');
const emptyEl = document.querySelector('#search-empty');
const nodesEl = document.querySelector('#search-nodes');
const cancelBtn = document.querySelector('#search-cancel');
const plots = [];

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
  const body = (node.rows || []).map(row => `<tr>${row.map(cell => `<td>${escapeHtml(cell)}</td>`).join('')}</tr>`).join('');
  return `<div class="search-table-wrap"><table class="search-table"><thead>${head}</thead><tbody>${body}</tbody></table></div>`;
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
        stroke: dataset.label?.toLowerCase().includes('b') ? '#3f6d60' : '#c65d48',
        width: 2,
        fill: (node.chart_type || node.chartType) === 'bar' ? 'rgba(198, 93, 72, 0.18)' : undefined,
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
            values: (_u, splits) => splits.map(split => node.labels[split] ?? ''),
          },
          {},
        ],
      }, data, container);
      plots.push(plot);
    } catch (_) {
      container.outerHTML = renderChartFallback(node);
    }
  });
}

function renderStatus(status) {
  const phase = status?.phase || 'idle';
  phaseEl.textContent = phase;
  phaseEl.className = `search-phase ${phase}`;
  messageEl.textContent = status?.message || '';
  cancelBtn.hidden = !(phase === 'listening' || phase === 'searching');
  if (status?.query) {
    queryWrap.hidden = false;
    queryText.textContent = status.query;
  }
}

function renderResult(payload) {
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

cancelBtn.addEventListener('click', async () => {
  renderStatus(await call('cancel_search'));
});

document.querySelector('#search-minimize').addEventListener('click', async () => {
  const windowApi = window.__TAURI__.window.getCurrentWindow();
  await windowApi.minimize();
});

document.querySelector('#search-close').addEventListener('click', async () => {
  const windowApi = window.__TAURI__.window.getCurrentWindow();
  await windowApi.hide();
});

listen('search-status', event => renderStatus(event.payload));
listen('search-result', event => {
  renderStatus({
    phase: 'complete',
    message: event.payload.warning || 'Answer ready',
    query: event.payload.query,
  });
  renderResult(event.payload);
});
listen('search-error', event => {
  showToast(String(event.payload), true);
});

call('get_search_status').then(renderStatus).catch(() => {});
