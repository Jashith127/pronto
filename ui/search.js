const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;

async function loadTheme() {
  try {
    const prefs = await invoke('get_preferences');
    window.ProntoTheme?.initTheme(prefs?.settings?.theme || 'system');
  } catch (_) {
    window.ProntoTheme?.initTheme('system');
  }
}
loadTheme();
listen('theme-changed', event => window.ProntoTheme?.initTheme(event.payload || 'system'));

const body = document.body;
const backdrop = document.querySelector('#backdrop');
const chrome = document.querySelector('#chrome');
const panelShell = document.querySelector('#panel-shell');
const panel = document.querySelector('#panel');
const panelContent = document.querySelector('#search-content');
const toast = document.querySelector('#toast');
const statusRow = document.querySelector('#search-status');
const messageEl = document.querySelector('#search-message');
const queryLabel = document.querySelector('#search-query-label');
const layoutBadge = document.querySelector('#search-layout-badge');

const LAYOUT_LABELS = {
  bio: 'Profile',
  article: 'Article',
  comparison: 'Comparison',
  steps: 'How-to',
  definition: 'Definition',
  timeline: 'Timeline',
  list: 'Ranking',
  yesno: 'Quick answer',
  location: 'Location',
  recipe: 'Recipe',
  stats: 'Stats'
};
const emptyEl = document.querySelector('#search-empty');
const nodesEl = document.querySelector('#search-nodes');
const cancelBtn = document.querySelector('#search-cancel');
const searchSubmit = document.querySelector('#search-submit');
const panelClose = document.querySelector('#panel-close');
const ddgBrand = document.querySelector('#ddg-brand');

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

function faviconForUrl(url) {
  const host = hostOf(url);
  return host ? `https://icons.duckduckgo.com/ip3/${host}.ico` : '';
}

const imageObjectUrls = new Set();

function revokeImageObjectUrls() {
  imageObjectUrls.forEach(url => {
    try { URL.revokeObjectURL(url); } catch (_) { /* ignore */ }
  });
  imageObjectUrls.clear();
}

function renderBannerImage(image, layout = 'article') {
  const inlineSrc = image?.dataUrl || image?.data_url;
  const remoteSrc = image?.src;
  const src = inlineSrc || remoteSrc;
  if (!src) return '';
  const link = image.linkUrl || image.link_url;
  const bioClass = layout === 'bio' ? ' bio-image' : '';
  const caption = image.caption ? `<figcaption>${escapeHtml(image.caption)}</figcaption>` : '';
  const loadedClass = inlineSrc ? ' loaded' : ' loading';
  const linkClass = link ? ' search-image-link' : '';
  const linkAttrs = link
    ? ` data-action="open_url" data-value="${escapeAttr(link)}" role="button" tabindex="0" title="Open image source on Wikimedia Commons"`
    : '';
  const imgTag = inlineSrc
    ? `<img src="${escapeAttr(inlineSrc)}" alt="${escapeAttr(image.alt || '')}" decoding="async" />`
    : `<img data-src="${escapeAttr(remoteSrc)}" alt="${escapeAttr(image.alt || '')}" decoding="async" />`;
  const skeleton = inlineSrc ? '' : '<div class="image-skeleton" aria-hidden="true"></div>';
  return `<figure class="search-image-frame${loadedClass}${bioClass}${linkClass}"${linkAttrs}>
    ${skeleton}
    ${imgTag}
    ${caption}
  </figure>`;
}

function loadImageSource(img, source) {
  const frame = img.closest('.search-image-frame');
  return new Promise((resolve, reject) => {
    const markLoaded = () => {
      frame?.classList.remove('loading');
      frame?.classList.add('loaded');
    };
    const onLoad = () => {
      markLoaded();
      resolve();
    };
    const onError = () => reject(new Error('image load failed'));
    img.addEventListener('load', onLoad, { once: true });
    img.addEventListener('error', onError, { once: true });
    img.referrerPolicy = 'origin';
    img.decoding = 'async';
    img.src = source;
    if (img.complete && img.naturalWidth > 0) onLoad();
  });
}

async function loadSearchImage(img, url) {
  const frame = img.closest('.search-image-frame');
  if (!url || !frame) return;

  const markError = () => {
    frame.classList.remove('loading');
    frame.classList.add('error');
  };

  try {
    await loadImageSource(img, url);
    return;
  } catch (_) { /* try proxy */ }

  try {
    const payload = await invoke('fetch_search_image', { url });
    const bytes = payload?.data instanceof Uint8Array
      ? payload.data
      : new Uint8Array(payload?.data || []);
    const mime = payload?.mime || 'image/jpeg';
    const blob = new Blob([bytes], { type: mime });
    const objectUrl = URL.createObjectURL(blob);
    imageObjectUrls.add(objectUrl);
    await loadImageSource(img, objectUrl);
  } catch (_) {
    markError();
  }
}

function mountImages(root) {
  root.querySelectorAll('.search-image-frame img[data-src]').forEach(img => {
    const url = img.getAttribute('data-src');
    if (url) loadSearchImage(img, url);
  });
  root.querySelectorAll('.search-image-frame.loaded img:not([data-src])').forEach(img => {
    const frame = img.closest('.search-image-frame');
    if (frame && img.complete && img.naturalWidth > 0) {
      frame.classList.remove('loading');
      frame.classList.add('loaded');
    }
  });
}

function renderSourceListFromHits(sources, open) {
  const items = sources || [];
  const rows = items.map((item, index) => {
    const favicon = faviconForUrl(item.url);
    const faviconHtml = favicon
      ? `<img class="src-favicon" src="${escapeAttr(favicon)}" alt="" width="18" height="18" decoding="async" loading="lazy" />`
      : '<span class="src-favicon" aria-hidden="true"></span>';
    return `
    <li>
      <button type="button" class="src-item" data-action="open_url" data-value="${escapeAttr(item.url)}">
        ${faviconHtml}
        <span class="src-index">${index + 1}</span>
        <span class="src-body">
          <strong>${escapeHtml(item.title)}</strong>
          ${item.snippet ? `<p class="src-snippet">${escapeHtml(item.snippet)}</p>` : ''}
          <span class="src-host">${escapeHtml(hostOf(item.url))}</span>
        </span>
      </button>
    </li>`;
  }).join('');
  if (!rows) return '';
  return `<details class="search-sources"${open ? ' open' : ''}>` +
    `<summary>Sources <span class="src-count">${items.length}</span></summary>` +
    `<ol>${rows}</ol></details>`;
}

function hasBannerImage(banner) {
  return Boolean(banner?.src || banner?.dataUrl || banner?.data_url);
}

function renderAnswerBlock(markdown, layout, banner) {
  const splitLead = window.splitMarkdownLead || (() => ({ lead: '', rest: markdown }));
  const renderMd = window.renderMarkdown || (text => `<p class="search-node-text">${escapeHtml(text)}</p>`);
  const { lead, rest } = splitLead(markdown);
  const leadHtml = lead ? renderMd(lead) : '';
  const bodyHtml = renderMd(rest || (!lead ? markdown : rest));
  const imageHtml = hasBannerImage(banner) ? renderBannerImage(banner, layout) : '';
  const safeLayout = escapeAttr(layout || 'article');

  if (layout === 'bio' && imageHtml) {
    const bioBody = leadHtml && rest.trim()
      ? `<div class="search-markdown bio-body">${bodyHtml}</div>`
      : '';
    const intro = leadHtml || bodyHtml;
    return `<div class="search-layout search-layout-bio">
      <div class="bio-hero">
        ${imageHtml}
        <div class="bio-intro search-markdown">${intro}</div>
      </div>
      ${bioBody}
    </div>`;
  }

  if (!leadHtml) {
    return `<div class="search-layout search-layout-${safeLayout}">
      <div class="search-markdown">
        ${bodyHtml}
        ${imageHtml}
      </div>
    </div>`;
  }

  if (!rest.trim()) {
    return `<div class="search-layout search-layout-${safeLayout}">
      <div class="search-markdown">
        ${leadHtml}
        ${imageHtml}
      </div>
    </div>`;
  }

  return `<div class="search-layout search-layout-${safeLayout}">
    <div class="search-markdown">
      ${leadHtml}
      ${imageHtml}
      <div class="answer-body">${bodyHtml}</div>
    </div>
  </div>`;
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
    panelShell.hidden = false;
    requestAnimationFrame(() => panelShell.classList.add('visible'));
  } else {
    panelShell.classList.remove('visible');
    panelShell.hidden = true;
  }
}

function setLayoutBadge(layout) {
  const key = (layout || 'article').toLowerCase();
  const label = LAYOUT_LABELS[key] || 'Article';
  if (layoutBadge) {
    layoutBadge.hidden = false;
    layoutBadge.textContent = label;
    layoutBadge.dataset.layout = key;
  }
}

function clearLayoutBadge() {
  if (layoutBadge) {
    layoutBadge.hidden = true;
    layoutBadge.textContent = '';
    layoutBadge.removeAttribute('data-layout');
  }
}

function renderKeyFacts(facts) {
  const items = facts || [];
  if (!items.length) return '';
  const chips = items.map(fact => {
    const label = escapeHtml(fact.label || fact.key || '');
    const value = escapeHtml(fact.value || '');
    return `<span class="key-fact"><strong>${label}</strong><span>${value}</span></span>`;
  }).join('');
  return `<div class="search-key-facts" role="list">${chips}</div>`;
}

function renderFollowups(followups) {
  const items = (followups || []).filter(Boolean);
  if (!items.length) return '';
  const chips = items.map(text =>
    `<button type="button" class="followup-chip" data-followup="${escapeAttr(text)}">${escapeHtml(text)}</button>`
  ).join('');
  return `<div class="search-followups"><span class="search-followups-label">Ask next</span><div class="search-followups-row">${chips}</div></div>`;
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
  if (ddgBrand) {
    ddgBrand.disabled = !currentQuery;
    ddgBrand.title = currentQuery
      ? `Search “${currentQuery}” on DuckDuckGo`
      : 'Search on DuckDuckGo';
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
  revokeImageObjectUrls();
  emptyEl.hidden = false;
  nodesEl.hidden = true;
  nodesEl.innerHTML = '';
  setQueryLabel('');
  clearLayoutBadge();
}

function shouldShowWarning(warning) {
  return Boolean(warning && warning.trim());
}

function paintResult(payload) {
  const query = payload.query || '';
  const markdown = payload.markdown || '';
  const layout = (payload.layout || 'article').toLowerCase();
  const sources = payload.sources || [];
  const hasAnswer = markdown.trim().length > 0;
  const banner = payload.bannerImage || payload.banner_image;

  const keyFacts = payload.keyFacts || payload.key_facts || [];
  const followups = payload.followups || [];

  const warning = shouldShowWarning(payload.warning)
    ? `<p class="search-warning">${escapeHtml(payload.warning)}</p>`
    : '';
  const factsHtml = renderKeyFacts(keyFacts);
  const answerHtml = renderAnswerBlock(markdown, layout, banner);
  const followupsHtml = renderFollowups(followups);
  const sourcesHtml = sources.length
    ? renderSourceListFromHits(sources, !hasAnswer)
    : '';

  emptyEl.hidden = true;
  nodesEl.hidden = false;
  nodesEl.innerHTML = `${warning}${factsHtml}${answerHtml}${followupsHtml}${sourcesHtml}`;
  mountImages(nodesEl);

  setQueryLabel(query);
  setLayoutBadge(layout);
  setStatus('', '');
  if (panelContent) panelContent.scrollTop = 0;
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
  searchSubmit.hidden = false;
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
  searchSubmit.hidden = true;
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
    searchSubmit.hidden = true;
  }
}

nodesEl.addEventListener('click', async event => {
  const followup = event.target.closest('.followup-chip');
  if (followup) {
    const text = followup.getAttribute('data-followup') || '';
    if (text) showToast(`Say: “${text}”`);
    return;
  }
  const target = event.target.closest('[data-action]');
  if (!target) return;
  const action = target.getAttribute('data-action');
  const value = target.getAttribute('data-value') || '';
  if (action === 'open_url') {
    event.preventDefault();
    await call('open_search_result', { url: value });
  }
});

nodesEl.addEventListener('keydown', async event => {
  if (event.key !== 'Enter' && event.key !== ' ') return;
  const target = event.target.closest('[data-action="open_url"]');
  if (!target?.classList.contains('search-image-link')) return;
  event.preventDefault();
  await call('open_search_result', { url: target.getAttribute('data-value') || '' });
});

ddgBrand?.addEventListener('click', async event => {
  event.stopPropagation();
  if (!currentQuery) return;
  await call('open_ddg_search', { query: currentQuery });
});

searchSubmit.addEventListener('click', async event => {
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
  showResultPanel(event.payload || {});
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
