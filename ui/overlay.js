const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;

document.querySelector('#cancel').addEventListener('click', () => invoke('cancel_recording'));
document.querySelector('#finish').addEventListener('click', () => invoke('stop_recording'));

listen('engine-status', event => {
  document.documentElement.classList.toggle('processing', event.payload.phase === 'processing');
});
