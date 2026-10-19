/* Pronto's tray menu.
   The page owns layout; Rust owns geometry. Everything drawn here is read from
   the engine at open time by `tray_menu_state`, so labels can never disagree
   with what the speech server is actually doing — the model row in particular
   offers whichever of load/offload is really available rather than flipping a
   remembered preference. */

const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;

const isMac = /Macintosh/.test(navigator.userAgent);
const panel = document.getElementById('panel');
const list = document.getElementById('list');
const flyout = document.getElementById('flyout');
const flyoutList = document.getElementById('flyout-list');
const flyoutCount = document.getElementById('flyout-count');
const engineState = document.getElementById('engine-state');
const engineStateText = document.getElementById('engine-state-text');

const FLYOUT_GAP = 6;
const HOVER_OPEN_DELAY = 90;
const HOVER_CLOSE_DELAY = 180;
const FLYOUT_MAX_LIST = 292;
const layer = document.querySelector('.tray-layer');

const ICONS = {
  window: '<rect x="3.5" y="5" width="17" height="14" rx="2.5"/><path d="M3.5 9.5h17"/><path d="M6.5 7.2h.01M9 7.2h.01"/>',
  mic: '<rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5.5 11a6.5 6.5 0 0 0 13 0M12 17.5V21"/>',
  stop: '<rect x="7" y="7" width="10" height="10" rx="2.5"/>',
  notes: '<circle cx="12" cy="12" r="7.5"/><circle cx="12" cy="12" r="3.2"/>',
  paste: '<rect x="7" y="4.5" width="10" height="4" rx="1.4"/><path d="M17 6.5h1.5A1.5 1.5 0 0 1 20 8v11a1.5 1.5 0 0 1-1.5 1.5h-13A1.5 1.5 0 0 1 4 19V8a1.5 1.5 0 0 1 1.5-1.5H7"/><path d="M9 12h6M9 15.5h4"/>',
  history: '<path d="M3.5 12a8.5 8.5 0 1 0 2.6-6.1L3.5 8.4"/><path d="M3.5 4.5v4h4"/><path d="M12 7.6V12l3 1.8"/>',
  model: '<rect x="7" y="7" width="10" height="10" rx="2"/><path d="M10 3v4M14 3v4M10 17v4M14 17v4M3 10h4M3 14h4M17 10h4M17 14h4"/>',
  quit: '<path d="M12 3.5v8"/><path d="M7.2 6.6a7.5 7.5 0 1 0 9.6 0"/>'
};

const svg = (paths, extra = '') =>
  `<svg viewBox="0 0 24 24" aria-hidden="true" ${extra}>${paths}</svg>`;

const keyLabels = isMac
  ? { control: '⌃', alt: '⌥', shift: '⇧', super: '⌘', Space: 'Space', ArrowUp: '↑', ArrowDown: '↓', ArrowLeft: '←', ArrowRight: '→' }
  : { control: 'Ctrl', alt: 'Alt', shift: 'Shift', super: 'Win', Space: 'Space', ArrowUp: '↑', ArrowDown: '↓', ArrowLeft: '←', ArrowRight: '→' };

function keyLabel(token) {
  if (keyLabels[token]) return keyLabels[token];
  if (token.startsWith('Key')) return token.slice(3);
  if (token.startsWith('Digit')) return token.slice(5);
  return token.replace('Numpad', 'Num ');
}

// Same shape as the main window's shortcut chips, so a shortcut read here
// matches the one read in Settings.
function shortcutChips(shortcut) {
  if (!shortcut) return '';
  const parts = shortcut.split('+').map(token => `<kbd>${keyLabel(token)}</kbd>`);
  return `<span class="tray-item-side">${parts.join('')}</span>`;
}

function formatDuration(ms) {
  const value = Number(ms);
  if (!Number.isFinite(value) || value < 1000) return `${Math.round(value)} ms`;
  const rounded = Math.round(value / 100) / 10;
  return `${Number.isInteger(rounded) ? rounded.toFixed(0) : rounded.toFixed(1)} s`;
}

function clockTime(ms) {
  return new Date(Number(ms)).toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' });
}

function dayStart(date) {
  return new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();
}

/** Today and yesterday get their usual time; anything older gets a date. */
function dayLabel(ms) {
  const date = new Date(Number(ms));
  const days = Math.round((dayStart(new Date()) - dayStart(date)) / 86_400_000);
  if (days <= 0) return clockTime(ms);
  if (days === 1) return 'Yest.';
  if (days < 7) return date.toLocaleDateString([], { weekday: 'short' });
  return date.toLocaleDateString([], { month: 'numeric', day: 'numeric' });
}

function plural(count, word) {
  return `${count.toLocaleString()} ${count === 1 ? word : `${word}s`}`;
}

// ------------------------------------------------------------------ rendering

let state = null;
let space = null;
let submenuOpen = false;

function row({ label, icon, side, action, arg, active, disabled, title, submenu }) {
  const element = document.createElement('button');
  element.type = 'button';
  element.className = 'tray-item';
  element.dataset.action = action;
  if (arg != null) element.dataset.arg = arg;
  if (active) element.classList.add('is-active');
  if (disabled) element.setAttribute('aria-disabled', 'true');
  if (title) element.title = title;
  element.setAttribute('role', 'menuitem');
  if (submenu) {
    element.setAttribute('aria-haspopup', 'menu');
    element.setAttribute('aria-expanded', String(submenuOpen));
  }
  element.innerHTML =
    `<span class="tray-item-mark">${svg(icon)}</span>` +
    `<span class="tray-item-label">${label}</span>` +
    (side || '') +
    (submenu ? `<span class="tray-item-side">${svg('<path d="m9.5 6.5 5.5 5.5-5.5 5.5"/>', 'class="tray-chevron"')}</span>` : '');
  return element;
}

function separator() {
  const element = document.createElement('div');
  element.className = 'tray-sep';
  element.setAttribute('role', 'separator');
  return element;
}

function modelRow() {
  const model = state.model;
  if (model.loading) {
    return row({
      label: 'Loading speech model…',
      icon: ICONS.model,
      side: `<span class="tray-item-side">${model.name}</span>`,
      action: 'noop',
      disabled: true,
      title: model.message
    });
  }
  const offloading = model.resident;
  return row({
    label: offloading ? 'Offload speech model' : 'Load speech model',
    icon: ICONS.model,
    side: `<span class="tray-item-side">${model.name}</span>`,
    action: offloading ? 'offload-model' : 'load-model',
    title: offloading
      ? `${model.backend} is holding memory. Offloading frees it; the next dictation loads it again.`
      : `${model.name} is offloaded. Load it to make dictation instant again.`
  });
}

function renderHeader() {
  const model = state.model;
  const tone = model.loading ? 'is-loading' : model.resident ? 'is-ready' : '';
  engineState.className = `tray-head-state ${tone}`.trim();
  engineStateText.textContent = model.loading ? 'Warming' : model.resident ? 'Warm' : 'Offloaded';
  engineState.title = model.message;
}

function renderRoot() {
  list.replaceChildren(
    row({
      label: 'Open Pronto',
      icon: ICONS.window,
      action: 'open'
    }),
    row({
      label: state.listening ? 'Stop Dictation' : 'Start Dictation',
      icon: state.listening ? ICONS.stop : ICONS.mic,
      side: shortcutChips(state.dictationHotkey),
      action: 'toggle-dictation',
      active: state.listening,
      title: state.listening ? 'Finish this dictation and transcribe it' : 'Start dictating'
    }),
    row({
      label: state.meetingRecording ? 'Stop taking notes' : 'Take meeting notes',
      icon: ICONS.notes,
      action: 'toggle-meeting',
      active: state.meetingRecording
    }),
    separator(),
    row({
      label: 'Paste Last Transcript',
      icon: ICONS.paste,
      side: shortcutChips(state.pasteHotkey),
      action: 'paste-last',
      disabled: state.historyTotal === 0,
      title: state.historyTotal === 0 ? 'There is no previous transcript yet' : ''
    }),
    row({
      label: 'Transcript history',
      icon: ICONS.history,
      side: state.historyTotal > state.history.length
        ? `<span class="tray-item-side">+${(state.historyTotal - state.history.length).toLocaleString()}</span>`
        : '',
      // Distinct from the flyout's "Open History" button, which runs the
      // action; this row only opens the flyout.
      action: 'open-submenu',
      submenu: true,
      title: `Your last ${plural(state.historyCount, 'transcript')}`
    }),
    separator(),
    modelRow(),
    separator(),
    row({
      label: 'Quit Pronto',
      icon: ICONS.quit,
      action: 'quit'
    })
  );
}

function renderFlyout() {
  flyoutCount.textContent = `${state.history.length} of ${state.historyTotal.toLocaleString()}`;
  if (!state.history.length) {
    const empty = document.createElement('p');
    empty.className = 'tray-empty';
    empty.textContent = 'Your transcripts will show up here after your first dictation.';
    flyoutList.replaceChildren(empty);
    return;
  }
  flyoutList.replaceChildren(
    ...state.history.map(entry => {
      const element = document.createElement('button');
      element.type = 'button';
      element.className = 'tray-transcript';
      element.dataset.action = 'paste-transcript';
      element.dataset.arg = entry.id;
      element.setAttribute('role', 'menuitem');
      element.title = 'Paste this transcript where you are typing';
      const parts = [plural(entry.wordCount, 'word')];
      if (entry.totalMs) parts.push(formatDuration(entry.totalMs));
      element.innerHTML =
        `<span class="tray-transcript-time">${dayLabel(entry.createdAtMs)}</span>` +
        `<span class="tray-transcript-body">` +
        `<p class="tray-transcript-text"></p>` +
        `<span class="tray-transcript-meta">${parts.join(' · ')}</span>` +
        `</span>`;
      element.querySelector('.tray-transcript-text').textContent = entry.preview;
      return element;
    })
  );
}

function render() {
  renderHeader();
  renderRoot();
  renderFlyout();
}

// ------------------------------------------------------------------- geometry

// Reports are chained, not fired: opening and closing the flyout in quick
// succession would otherwise let an older size land after a newer one.
let pendingReport = Promise.resolve();

/**
 * Report the box this menu needs and the point inside it that belongs under
 * the cursor. Rust flips the menu back inside the work area and only then
 * shows the window, so the page never has to know where on screen it is.
 */
function report() {
  pendingReport = pendingReport.then(async () => {
    const bodyStyle = getComputedStyle(document.body);
    const padLeft = parseFloat(bodyStyle.paddingLeft) || 0;
    const padRight = parseFloat(bodyStyle.paddingRight) || 0;
    const padTop = parseFloat(bodyStyle.paddingTop) || 0;
    const padBottom = parseFloat(bodyStyle.paddingBottom) || 0;
    const box = layer.getBoundingClientRect();
    const panelBox = panel.getBoundingClientRect();
    await invoke('tray_menu_ready', {
      layout: {
        width: Math.ceil(box.width + padLeft + padRight),
        height: Math.ceil(box.height + padTop + padBottom),
        anchorX: Math.round(panelBox.left),
        anchorY: Math.round(panelBox.top)
      }
    });
  }).catch(error => console.error(error));
  return pendingReport;
}

/**
 * Nothing may be taller than the screen. Rust moves the window back inside the
 * work area, so the room available to the flyout is the whole work area, not
 * just what is left under the cursor. The height snaps to a row boundary so a
 * transcript is never left cut in half with no sign there is more below.
 */
function fitFlyout() {
  if (!submenuOpen || !space) return;
  const chrome = flyout.offsetHeight - flyoutList.offsetHeight;
  const screen = space.workBottom - space.workTop;
  const room = Math.min(FLYOUT_MAX_LIST, screen - chrome - space.margin * 2);
  const rows = [...flyoutList.querySelectorAll('.tray-transcript')];
  const listPadding = 10; // .tray-flyout-list padding, top and bottom
  let fitted = 0;
  for (const entry of rows) {
    if (fitted + entry.offsetHeight > room) break;
    fitted += entry.offsetHeight;
  }
  // Always show one whole transcript, even on a very small screen.
  const floor = (rows[0]?.offsetHeight ?? 72) + listPadding + 4;
  flyoutList.style.maxHeight = `${Math.max(Math.min(fitted, room) + listPadding, Math.min(floor, room))}px`;
}

/**
 * Open beside the root menu, or flip to its left. A tray icon in the
 * notification area has almost no room to its right, which is the case worth
 * getting right; a taskbar on the left of the screen is the other.
 */
function setSubmenu(open) {
  if (open === submenuOpen) return;
  submenuOpen = open;
  flyout.hidden = !open;
  if (open) {
    const panelWidth = panel.getBoundingClientRect().width;
    const flyoutWidth = flyout.getBoundingClientRect().width;
    const openLeft = Boolean(
      space &&
        space.cursorX + panelWidth + FLYOUT_GAP + flyoutWidth + space.margin > space.workRight
    );
    layer.style.flexDirection = openLeft ? 'row-reverse' : 'row';
    fitFlyout();
  }
  historyTrigger()?.setAttribute('aria-expanded', String(open));
  report();
}

function historyTrigger() {
  return [...list.querySelectorAll('.tray-item')].find(item => item.hasAttribute('aria-haspopup'));
}

// -------------------------------------------------------------------- actions

function close() {
  invoke('tray_menu_close').catch(() => {});
}

function run(action, arg) {
  if (action === 'open-submenu') {
    setSubmenu(true);
    return;
  }
  // The backend dismisses the menu itself before running an action: an action
  // that types into the foreground window must never aim at Pronto's own menu.
  invoke('tray_menu_action', { action, arg: arg ?? null }).catch(error => console.error(error));
}

list.addEventListener('click', event => {
  const item = event.target.closest('.tray-item');
  if (!item || item.getAttribute('aria-disabled') === 'true') return;
  run(item.dataset.action, item.dataset.arg);
});

flyout.addEventListener('click', event => {
  const button = event.target.closest('.tray-foot-button, .tray-transcript');
  if (!button) return;
  run(button.dataset.action, button.dataset.arg);
});

let openTimer = null;
let closeTimer = null;
layer.addEventListener('mouseover', event => {
  const trigger = event.target.closest('.tray-item[aria-haspopup]');
  clearTimeout(closeTimer);
  if (!trigger) return;
  clearTimeout(openTimer);
  openTimer = setTimeout(() => setSubmenu(true), HOVER_OPEN_DELAY);
});

layer.addEventListener('mouseout', event => {
  const trigger = event.target.closest('.tray-item[aria-haspopup]');
  clearTimeout(openTimer);
  if (!trigger || submenuOpen) return;
  closeTimer = setTimeout(() => setSubmenu(false), HOVER_CLOSE_DELAY);
});

document.addEventListener('keydown', event => {
  const items = [...document.querySelectorAll('.tray-item:not([aria-disabled="true"])')];
  const focused = document.activeElement;
  if (event.key === 'Escape') {
    event.preventDefault();
    if (submenuOpen) {
      setSubmenu(false);
      historyTrigger()?.focus();
    } else close();
    return;
  }
  if (event.key === 'ArrowRight' && focused?.hasAttribute('aria-haspopup')) {
    event.preventDefault();
    setSubmenu(true);
    return;
  }
  if (event.key === 'ArrowLeft' && submenuOpen && flyout.contains(focused)) {
    event.preventDefault();
    setSubmenu(false);
    historyTrigger()?.focus();
    return;
  }
  if (!['ArrowDown', 'ArrowUp'].includes(event.key)) return;
  event.preventDefault();
  const pool = submenuOpen && flyout.contains(focused)
    ? [...flyoutList.querySelectorAll('.tray-transcript')]
    : items;
  if (!pool.length) return;
  const at = pool.indexOf(focused);
  const step = event.key === 'ArrowDown' ? 1 : -1;
  const next = at === -1
    ? (step === 1 ? 0 : pool.length - 1)
    : (at + step + pool.length) % pool.length;
  pool[next].focus();
});

// ----------------------------------------------------------------------- boot

// Two tray clicks in quick succession issue two reads; only the newer one may
// draw, or an older response could repaint a menu that no longer matches.
let openToken = 0;

async function refresh() {
  const token = ++openToken;
  const result = await invoke('tray_menu_state');
  if (token !== openToken) return;
  state = result.state;
  space = result.space;
  window.ProntoTheme?.initTheme(state.theme);
  setSubmenu(false);
  render();
  await report();
}

listen('tray-menu-open', () => {
  refresh().catch(error => console.error(error));
});

// The page also boots on its own the first time the window is created; the
// backend ignores that first report because no click has asked for a menu yet.
refresh().catch(error => console.error(error));
