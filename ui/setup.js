const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const $ = selector => document.querySelector(selector);

let info = null;
let selected = null;
let mode = 'install';
let current = null;
let tipTimer = null;

const TIPS = [
  'Hold Ctrl + Alt + Space to dictate in any app.',
  'Win + Shift + V pastes your last transcript.',
  'Your voice is transcribed on this PC.',
  'Add names to the Dictionary so they are spelled right.'
];

function show(name) {
  const next = document.querySelector(`.screen[data-screen="${name}"]`);
  if (current === next) return;
  if (current) {
    const leaving = current;
    leaving.classList.remove('active');
    leaving.classList.add('leaving');
    setTimeout(() => leaving.classList.remove('leaving'), 450);
  }
  next.classList.add('active');
  current = next;
  const focus = next.querySelector('.primary:not([hidden])');
  if (focus) setTimeout(() => focus.focus({ preventScroll: true }), 320);
}

function formatBytes(bytes) {
  const mb = Number(bytes) / (1024 * 1024);
  if (mb >= 1024) return `${(mb / 1024).toFixed(1)} GB`;
  return `${Math.max(1, Math.round(mb))} MB`;
}

function formatEta(seconds) {
  if (seconds == null) return '';
  if (seconds < 5) return 'almost done';
  if (seconds < 60) return `about ${Math.ceil(seconds / 5) * 5} s left`;
  return `about ${Math.round(seconds / 60)} min left`;
}

function select(model) {
  selected = model;
  document.querySelectorAll('.engine').forEach(card => {
    card.setAttribute('aria-checked', String(card.dataset.model === model));
  });
  const warning = $('#choice-warning');
  const parakeetNote = $('.engine[data-model="parakeet"] .engine-note');
  parakeetNote.classList.toggle('alert', !info.hasNvidia);
  const blocked = model === 'parakeet' && !info.hasNvidia;
  warning.textContent = blocked ? 'No NVIDIA GPU was found, so Parakeet will not run on this PC.' : '';
  warning.classList.toggle('quiet-hint', !blocked);
}

function renderChoices() {
  const gpu = $('#gpu');
  gpu.classList.toggle('nvidia', info.hasNvidia);
  gpu.classList.toggle('other', !info.hasNvidia && Boolean(info.gpuName));
  $('#gpu-name').textContent = info.gpuName ? info.gpuName : 'No dedicated graphics found';
  for (const model of info.models) {
    const card = document.querySelector(`.engine[data-model="${model.id}"]`);
    card.querySelector('.badge').hidden = !model.recommended;
    card.querySelector('.engine-size').textContent = model.downloadBytes
      ? `${formatBytes(model.downloadBytes)} download`
      : 'Already on this PC';
  }
  select(info.currentModel || info.recommended);
}

function rotateTips() {
  let index = 0;
  const tip = $('#tip');
  tip.textContent = TIPS[0];
  clearInterval(tipTimer);
  tipTimer = setInterval(() => {
    tip.style.opacity = 0;
    setTimeout(() => {
      index = (index + 1) % TIPS.length;
      tip.textContent = TIPS[index];
      tip.style.opacity = 1;
    }, 400);
  }, 5000);
}

function setProgress(fraction) {
  const percent = Math.max(0, Math.min(100, Math.round(fraction * 100)));
  $('#percent').textContent = `${percent}%`;
  $('#bar-fill').style.width = `${percent}%`;
  $('.bar').setAttribute('aria-valuenow', String(percent));
}

function beginProgress(label) {
  const screen = $('.screen[data-screen="progress"]');
  screen.classList.remove('failed');
  $('.screen[data-screen="progress"] .voice').className = 'voice live';
  $('#progress-actions').hidden = mode === 'uninstall';
  $('#error-actions').hidden = true;
  $('#stage-label').textContent = label;
  $('#detail').innerHTML = '&nbsp;';
  $('#tip').hidden = mode === 'uninstall';
  setProgress(0);
  show('progress');
  if (mode === 'install') rotateTips();
}

function onProgress(event) {
  const p = event.payload;
  if (p.stage === 'done') {
    setProgress(1);
    clearInterval(tipTimer);
    $('.screen[data-screen="progress"] .voice').className = 'voice settle';
    setTimeout(() => showDone(), 650);
    return;
  }
  if (p.stage === 'error' || p.stage === 'cancelled') {
    clearInterval(tipTimer);
    $('.screen[data-screen="progress"]').classList.add('failed');
    $('.screen[data-screen="progress"] .voice').className = 'voice still';
    $('#stage-label').textContent = p.stage === 'cancelled' ? 'Setup paused' : 'Something went wrong';
    $('#detail').textContent = p.stage === 'cancelled' ? 'Your download is saved. Try again to resume.' : p.message;
    $('#tip').hidden = true;
    $('#progress-actions').hidden = true;
    $('#error-actions').hidden = false;
    $('#retry').hidden = mode === 'uninstall';
    return;
  }
  setProgress(p.fraction);
  $('#stage-label').textContent = p.message;
  if (p.stage === 'download' && p.totalBytes) {
    const parts = [`${formatBytes(p.downloadedBytes)} of ${formatBytes(p.totalBytes)}`];
    if (p.bytesPerSec) parts.push(`${formatBytes(p.bytesPerSec)}/s`);
    const eta = formatEta(p.etaSecs);
    if (eta) parts.push(eta);
    $('#detail').textContent = parts.join(' · ');
  } else {
    $('#detail').innerHTML = '&nbsp;';
  }
  // Downloads can be cancelled; unpacking and registration cannot.
  $('#cancel').hidden = p.stage !== 'download';
}

function showDone() {
  if (mode === 'uninstall') {
    $('#done-title').textContent = 'Pronto was removed';
    $('#done-keys').hidden = true;
    $('#done-lede').textContent = $('#keep-data').checked ? 'Your history and settings were kept.' : 'Thanks for trying Pronto.';
    $('#launch').textContent = 'Close';
  } else if (info.upgrade) {
    $('#done-title').textContent = 'Pronto is up to date';
  }
  show('done');
}

async function startInstall() {
  mode = 'install';
  beginProgress('Getting ready');
  try {
    await invoke('start_install', { model: selected });
  } catch (error) {
    onProgress({ payload: { stage: 'error', message: String(error) } });
  }
}

async function init() {
  info = await invoke('setup_info');
  mode = info.mode;
  $('#version').textContent = `Version ${info.version}`;
  if (info.upgrade) {
    $('#welcome-lede').textContent = `Update to version ${info.version}. Your history and settings stay.`;
    $('#get-started').textContent = 'Continue';
  }
  renderChoices();
  show(mode === 'uninstall' ? 'uninstall' : 'welcome');
  await invoke('show_window');
}

document.querySelectorAll('.engine').forEach(card => card.addEventListener('click', () => select(card.dataset.model)));
document.querySelector('.engines').addEventListener('keydown', event => {
  if (!['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(event.key)) return;
  event.preventDefault();
  const next = selected === 'parakeet' ? 'phonon' : 'parakeet';
  select(next);
  document.querySelector(`.engine[data-model="${next}"]`).focus();
});
$('#get-started').addEventListener('click', () => show('choose'));
$('#install').addEventListener('click', startInstall);
$('#retry').addEventListener('click', startInstall);
$('#cancel').addEventListener('click', () => invoke('cancel_install'));
$('#error-close').addEventListener('click', () => invoke('close_window'));
$('#launch').addEventListener('click', () => invoke(mode === 'uninstall' ? 'close_window' : 'launch_pronto'));
$('#uninstall-cancel').addEventListener('click', () => invoke('close_window'));
$('#uninstall').addEventListener('click', async () => {
  beginProgress('Removing Pronto');
  try {
    await invoke('start_uninstall', { keepData: $('#keep-data').checked });
  } catch (error) {
    onProgress({ payload: { stage: 'error', message: String(error) } });
  }
});
$('#minimize').addEventListener('click', () => invoke('minimize_window'));
$('#close').addEventListener('click', () => invoke('close_window'));
document.addEventListener('keydown', event => {
  if (event.key === 'Enter' && current?.dataset.screen === 'welcome') $('#get-started').click();
});
listen('setup-progress', onProgress);
init().catch(error => {
  document.body.textContent = String(error);
  invoke('show_window');
});
