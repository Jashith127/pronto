const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;
const microphoneLabel = document.querySelector('#microphone-label');
let microphoneTimer;
let persistentNotice = false;

async function showNotice(text) {
  microphoneLabel.textContent = text;
  microphoneLabel.hidden = false;
  await new Promise(requestAnimationFrame);
  const responsiveWidth = Math.max(96, Math.ceil(microphoneLabel.getBoundingClientRect().width + 4));
  await invoke('resize_microphone_overlay', { width: responsiveWidth });
}

async function hideNotice() {
  microphoneLabel.hidden = true;
  await invoke('compact_overlay');
}

document.querySelector('#cancel').addEventListener('click', () => invoke('cancel_recording'));
document.querySelector('#finish').addEventListener('click', () => invoke('stop_recording'));

listen('engine-status', event => {
  document.documentElement.classList.toggle('processing', event.payload.phase === 'processing');
});

listen('microphone-activated', async event => {
  if (persistentNotice) return;
  await showNotice(event.payload.name);
  clearTimeout(microphoneTimer);
  microphoneTimer = setTimeout(async () => {
    if (!persistentNotice) await hideNotice();
  }, 3000);
});

listen('dictation-notice', async event => {
  persistentNotice = true;
  clearTimeout(microphoneTimer);
  await showNotice(event.payload.message);
});

listen('dictation-notice-clear', async () => {
  persistentNotice = false;
  clearTimeout(microphoneTimer);
  await hideNotice();
});
