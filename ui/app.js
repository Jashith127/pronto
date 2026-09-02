const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;
const root = document.documentElement;
const toast = document.querySelector('#toast');
const hotkeyDialog = document.querySelector('#hotkey-dialog');
const hotkeyCapture = document.querySelector('#hotkey-capture');
let preferences = null;
let hotkeyStatus = null;
let microphoneStatus = null;
let history = [];
let engineStatus = null;
let pendingShortcut = '';

function showToast(text, error = false) {
  toast.textContent = text;
  toast.className = `toast show${error ? ' error' : ''}`;
  clearTimeout(showToast.timer);
  showToast.timer = setTimeout(() => toast.className = 'toast', 3200);
}

async function call(command, args = {}) {
  try { return await invoke(command, args); }
  catch (error) { showToast(String(error), true); throw error; }
}

function setView(id) {
  document.querySelectorAll('.view').forEach(view => view.classList.toggle('active', view.id === id));
  document.querySelectorAll('.nav').forEach(button => button.classList.toggle('active', button.dataset.view === id));
}

function renderStatus(next) {
  engineStatus = next;
  root.classList.toggle('listening', next.phase === 'listening');
}

function renderModel(next) {
  document.querySelector('#engine-message').textContent = next.message;
}

function historyMarkup(entries) {
  if (!entries.length) return '<div class="history-empty">Your transcripts will appear here after your first dictation.</div>';
  return entries.map(entry => {
    const date = new Date(Number(entry.createdAtMs));
    const time = date.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' });
    const cleanup = entry.cleanupApplied ? 'DeepSeek cleanup' : 'Local cleanup';
    return `<article class="history-item"><time class="history-time">${time}</time><div class="history-copy"><p>${escapeHtml(entry.finalText)}</p><small>${date.toLocaleDateString()} · ${cleanup}</small></div><div class="history-actions"><span class="latency">${entry.totalMs} ms</span><button class="copy-transcript" data-copy="${entry.id}" aria-label="Copy transcript" title="Copy transcript"><svg viewBox="0 0 24 24" aria-hidden="true"><rect x="8" y="8" width="11" height="11" rx="2"/><path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2"/></svg></button></div></article>`;
  }).join('');
}

function renderHistory() {
  document.querySelector('#recent-list').innerHTML = historyMarkup(history.slice(0, 3));
  document.querySelector('#history-list').innerHTML = historyMarkup(history);
  document.querySelector('#dictation-count').textContent = history.length.toLocaleString();
  const words = history.reduce((total, entry) => total + entry.finalText.trim().split(/\s+/).filter(Boolean).length, 0);
  document.querySelector('#word-count').textContent = words.toLocaleString();
  const timedEntries = history.filter(entry => Number(entry.audioMs) > 0);
  const timedWords = timedEntries.reduce((total, entry) => total + entry.finalText.trim().split(/\s+/).filter(Boolean).length, 0);
  const audioMs = timedEntries.reduce((total, entry) => total + Number(entry.audioMs), 0);
  const wpm = audioMs > 0 ? Math.round(timedWords * 60000 / audioMs) : 0;
  document.querySelector('#average-wpm').textContent = wpm || '—';
  const recentPaces = timedEntries.slice(0, 7).reverse().map(entry => {
    const entryWords = entry.finalText.trim().split(/\s+/).filter(Boolean).length;
    return Math.round(entryWords * 60000 / Number(entry.audioMs));
  }).filter(Number.isFinite);
  const chart = document.querySelector('#wpm-chart');
  chart.classList.toggle('empty', recentPaces.length === 0);
  chart.innerHTML = recentPaces.map(pace => {
    const height = Math.max(8, Math.min(100, Math.round(pace / 220 * 100)));
    return `<i style="height:${height}%" title="${pace} WPM" aria-label="${pace} words per minute"></i>`;
  }).join('');
  document.querySelector('#wpm-note').textContent = recentPaces.length
    ? `${recentPaces.length} most recent measured ${recentPaces.length === 1 ? 'dictation' : 'dictations'}`
    : 'Your pace will appear after a dictation';
  const average = history.length ? Math.round(history.reduce((total, entry) => total + Number(entry.totalMs), 0) / history.length) : 0;
  document.querySelector('#average-latency').textContent = average ? `${average} ms` : '—';
}

function renderDictionary() {
  const terms = preferences.settings.dictionary;
  document.querySelector('#dictionary-list').innerHTML = terms.map(term => `<div class="dictionary-item"><span>${escapeHtml(term)}</span><button data-remove="${escapeAttr(term)}" aria-label="Remove ${escapeAttr(term)}">×</button></div>`).join('');
  document.querySelector('#dictionary-empty').hidden = terms.length > 0;
  document.querySelector('#term-count').textContent = `${terms.length} ${terms.length === 1 ? 'term' : 'terms'}`;
}

function keyLabel(token) {
  const labels = { control: 'Ctrl', alt: 'Alt', shift: 'Shift', super: 'Win', Space: 'Space', ArrowUp: '↑', ArrowDown: '↓', ArrowLeft: '←', ArrowRight: '→' };
  if (labels[token]) return labels[token];
  if (token.startsWith('Key')) return token.slice(3);
  if (token.startsWith('Digit')) return token.slice(5);
  return token.replace('Numpad', 'Num ');
}

function shortcutMarkup(shortcut) {
  if (!shortcut) return '<span class="unavailable">Not set</span>';
  return shortcut.split('+').map((token, index) => `${index ? '<span class="plus">+</span>' : ''}<kbd>${escapeHtml(keyLabel(token))}</kbd>`).join('');
}

function renderHotkey(next) {
  hotkeyStatus = next;
  document.querySelector('#settings-hotkey').innerHTML = shortcutMarkup(next.shortcut);
  if (engineStatus?.phase === 'idle') renderStatus(engineStatus);
  if (next.error) showToast(next.error, true);
}

function renderMicrophones() {
  if (!microphoneStatus) return;
  const select = document.querySelector('#microphone');
  const defaultDevice = microphoneStatus.devices.find(device => device.isDefault);
  const systemDefault = defaultDevice ? `System default — ${defaultDevice.name}` : 'System default';
  const options = [{ id: '', name: systemDefault }, ...microphoneStatus.devices.map(device => ({ id: device.id, name: device.name }))];
  if (microphoneStatus.selectedId && !options.some(option => option.id === microphoneStatus.selectedId)) {
    options.push({ id: microphoneStatus.selectedId, name: `${preferences.settings.microphoneName || 'Saved microphone'} — unavailable` });
  }
  select.innerHTML = options.map(option => `<option value="${escapeAttr(option.id)}">${escapeHtml(option.name)}</option>`).join('');
  select.value = microphoneStatus.selectedId || '';
  document.querySelector('#microphone-status').textContent = microphoneStatus.fallback
    ? `Saved microphone unavailable — using ${microphoneStatus.activeName}`
    : `Using ${microphoneStatus.activeName}`;
}

function renderPreferences() {
  document.querySelector('#cleanup-enabled').checked = preferences.settings.cleanupEnabled;
  document.querySelector('#auto-insert').checked = preferences.settings.autoInsert;
  document.querySelector('#duck-audio').checked = preferences.settings.duckAudio;
  document.querySelector('#launch-at-startup').checked = preferences.settings.launchAtStartup;
  document.querySelector('#gpu-memory-management').checked = preferences.settings.gpuMemoryManagement;
  document.querySelector('#dictation-sounds').checked = preferences.settings.dictationSounds;
  document.querySelector('#language').value = preferences.settings.language;
  document.querySelectorAll('[data-activation]').forEach(button => button.classList.toggle('active', button.dataset.activation === preferences.settings.activationMode));
  document.querySelector('#api-status').textContent = preferences.apiKeyConfigured ? 'Stored securely in Windows Credential Manager' : 'Not configured — local cleanup will be used';
  const promptInput = document.querySelector('#cleanup-prompt');
  const effectivePrompt = preferences.settings.cleanupPrompt || preferences.defaultCleanupPrompt;
  if (document.activeElement !== promptInput) promptInput.value = effectivePrompt;
  document.querySelector('#cleanup-prompt-status').textContent = preferences.settings.cleanupPrompt ? 'Custom prompt' : 'Built-in prompt';
  document.querySelector('#cleanup-prompt-count').textContent = `${promptInput.value.length.toLocaleString()} / 16,000`;
  renderMicrophones();
  renderDictionary();
}

async function persistSettings() {
  const settings = {
    ...preferences.settings,
    cleanupEnabled: document.querySelector('#cleanup-enabled').checked,
    autoInsert: document.querySelector('#auto-insert').checked,
    duckAudio: document.querySelector('#duck-audio').checked,
    launchAtStartup: document.querySelector('#launch-at-startup').checked,
    gpuMemoryManagement: document.querySelector('#gpu-memory-management').checked,
    dictationSounds: document.querySelector('#dictation-sounds').checked,
    language: document.querySelector('#language').value
  };
  preferences = await call('save_settings', { settings });
  renderPreferences();
  showToast('Settings saved');
}

function escapeHtml(value) { const node = document.createElement('div'); node.textContent = value; return node.innerHTML; }
function escapeAttr(value) { return escapeHtml(value).replaceAll('"', '&quot;'); }

function capturedShortcut(event) {
  const modifiers = [];
  if (event.ctrlKey) modifiers.push('control');
  if (event.altKey) modifiers.push('alt');
  if (event.shiftKey) modifiers.push('shift');
  if (event.metaKey) modifiers.push('super');
  const modifierCodes = new Set(['ControlLeft', 'ControlRight', 'AltLeft', 'AltRight', 'ShiftLeft', 'ShiftRight', 'MetaLeft', 'MetaRight']);
  if (modifierCodes.has(event.code)) return { preview: modifiers.join('+'), complete: false };
  if (!event.code || event.code === 'Escape') return { preview: '', complete: false };
  return { preview: [...modifiers, event.code].join('+'), complete: true };
}

function openHotkeyDialog() {
  document.querySelector('#hotkey-error').textContent = '';
  document.querySelector('#capture-keys').innerHTML = '';
  document.querySelector('.capture-prompt').hidden = false;
  document.querySelector('#save-hotkey').disabled = true;
  pendingShortcut = '';
  hotkeyDialog.showModal();
  setTimeout(() => hotkeyCapture.focus(), 0);
}

document.querySelectorAll('[data-view]').forEach(button => button.addEventListener('click', () => setView(button.dataset.view)));
document.querySelectorAll('[data-view-link]').forEach(button => button.addEventListener('click', () => setView(button.dataset.viewLink)));
const bindWindowAction = (selector, command) => {
  const button = document.querySelector(selector);
  button.addEventListener('pointerdown', event => event.stopPropagation());
  button.addEventListener('click', event => {
    event.preventDefault();
    event.stopPropagation();
    call(command);
  });
};

bindWindowAction('#minimize', 'minimize_main_window');
bindWindowAction('#close', 'hide_main_window');

function audioBufferToWav(audioBuffer) {
  const targetRate = 16000;
  const outputLength = Math.ceil(audioBuffer.duration * targetRate);
  const wav = new ArrayBuffer(44 + outputLength * 2);
  const view = new DataView(wav);
  const writeText = (offset, text) => [...text].forEach((character, index) => view.setUint8(offset + index, character.charCodeAt(0)));
  writeText(0, 'RIFF'); view.setUint32(4, 36 + outputLength * 2, true); writeText(8, 'WAVE');
  writeText(12, 'fmt '); view.setUint32(16, 16, true); view.setUint16(20, 1, true); view.setUint16(22, 1, true);
  view.setUint32(24, targetRate, true); view.setUint32(28, targetRate * 2, true); view.setUint16(32, 2, true); view.setUint16(34, 16, true);
  writeText(36, 'data'); view.setUint32(40, outputLength * 2, true);
  const channels = Array.from({ length: audioBuffer.numberOfChannels }, (_, index) => audioBuffer.getChannelData(index));
  const ratio = audioBuffer.sampleRate / targetRate;
  for (let index = 0; index < outputLength; index++) {
    const position = index * ratio;
    const left = Math.min(Math.floor(position), audioBuffer.length - 1);
    const right = Math.min(left + 1, audioBuffer.length - 1);
    const fraction = position - left;
    let sample = 0;
    for (const channel of channels) sample += channel[left] * (1 - fraction) + channel[right] * fraction;
    sample = Math.max(-1, Math.min(1, sample / channels.length));
    view.setInt16(44 + index * 2, sample < 0 ? sample * 32768 : sample * 32767, true);
  }
  return new Uint8Array(wav);
}

async function importMedia(file) {
  const card = document.querySelector('#import-card');
  const button = document.querySelector('#choose-media');
  const status = document.querySelector('#import-status');
  card.classList.add('busy'); button.disabled = true;
  try {
    status.textContent = `Reading ${file.name}…`;
    const context = new AudioContext();
    let decoded;
    try { decoded = await context.decodeAudioData(await file.arrayBuffer()); }
    finally { await context.close(); }
    if (!decoded.numberOfChannels || !decoded.length) throw new Error('No audio track was found in this file.');
    if (decoded.duration > 90 * 60) throw new Error('Choose a file shorter than 90 minutes.');
    status.textContent = 'Preparing audio for local transcription…';
    await new Promise(resolve => setTimeout(resolve, 20));
    const wavBytes = audioBufferToWav(decoded);
    status.textContent = 'Transcribing locally with Parakeet…';
    await call('transcribe_media_file', { fileName: file.name, wavBytes: Array.from(wavBytes) });
    showToast(`${file.name} is being transcribed`);
  } catch (error) {
    const detail = String(error).replace(/^Error:\s*/, '');
    showToast(detail.includes('Unable to decode') || detail.includes('EncodingError') ? 'This media format or audio track is not supported.' : detail, true);
  } finally {
    card.classList.remove('busy'); button.disabled = false;
    status.textContent = 'MP3, WAV, M4A, MP4, MOV, WebM, and other supported media';
    document.querySelector('#media-file').value = '';
  }
}

document.querySelector('#choose-media').addEventListener('click', () => document.querySelector('#media-file').click());
document.querySelector('#media-file').addEventListener('change', event => {
  const file = event.target.files?.[0];
  if (file) importMedia(file);
});

document.querySelector('#dictionary-form').addEventListener('submit', async event => {
  event.preventDefault();
  const input = document.querySelector('#dictionary-input');
  if (!input.value.trim()) return;
  preferences.settings = await call('add_dictionary_term', { term: input.value });
  input.value = '';
  renderDictionary();
});
document.querySelector('#dictionary-list').addEventListener('click', async event => {
  const term = event.target.dataset.remove;
  if (!term) return;
  preferences.settings = await call('remove_dictionary_term', { term });
  renderDictionary();
});
document.querySelectorAll('#cleanup-enabled,#auto-insert,#duck-audio,#dictation-sounds,#launch-at-startup,#gpu-memory-management,#language').forEach(input => input.addEventListener('change', persistSettings));
document.querySelectorAll('[data-activation]').forEach(button => button.addEventListener('click', async () => {
  preferences.settings.activationMode = button.dataset.activation;
  await persistSettings();
}));
document.querySelector('#microphone').addEventListener('change', async event => {
  const previousId = microphoneStatus?.selectedId || '';
  event.target.disabled = true;
  try {
    microphoneStatus = await call('set_microphone', { deviceId: event.target.value || null });
    preferences.settings.microphoneId = microphoneStatus.selectedId;
    preferences.settings.microphoneName = microphoneStatus.activeName;
    renderMicrophones();
    showToast(`Microphone changed to ${microphoneStatus.activeName}`);
  } catch (_) {
    event.target.value = previousId;
  } finally {
    event.target.disabled = false;
  }
});
document.querySelector('#save-key').addEventListener('click', async () => {
  const input = document.querySelector('#api-key');
  if (!input.value.trim()) { showToast('Enter a DeepSeek API key first', true); return; }
  preferences = await call('save_api_key', { apiKey: input.value });
  input.value = '';
  renderPreferences();
  showToast('DeepSeek key saved securely');
});
document.querySelector('#cleanup-prompt').addEventListener('input', event => {
  document.querySelector('#cleanup-prompt-status').textContent = 'Unsaved changes';
  document.querySelector('#cleanup-prompt-count').textContent = `${event.target.value.length.toLocaleString()} / 16,000`;
});
document.querySelector('#save-cleanup-prompt').addEventListener('click', async () => {
  const input = document.querySelector('#cleanup-prompt');
  const prompt = input.value.trim();
  if (!prompt) { showToast('The cleanup prompt cannot be empty', true); return; }
  preferences.settings.cleanupPrompt = prompt === preferences.defaultCleanupPrompt ? null : prompt;
  preferences = await call('save_settings', { settings: preferences.settings });
  renderPreferences();
  showToast('Cleanup prompt saved');
});
document.querySelector('#reset-cleanup-prompt').addEventListener('click', async () => {
  preferences.settings.cleanupPrompt = null;
  preferences = await call('save_settings', { settings: preferences.settings });
  document.querySelector('#cleanup-prompt').blur();
  renderPreferences();
  showToast('Default cleanup prompt restored');
});
document.querySelector('#clear-history').addEventListener('click', async () => {
  if (!confirm('Clear all locally stored transcripts?')) return;
  await call('clear_history');
  history = [];
  renderHistory();
  showToast('History cleared');
});
async function copyTranscript(event) {
  const button = event.target.closest('[data-copy]');
  if (!button) return;
  await call('copy_transcript', { id: button.dataset.copy });
  button.classList.add('copied');
  button.setAttribute('aria-label', 'Copied');
  showToast('Transcript copied');
  setTimeout(() => { button.classList.remove('copied'); button.setAttribute('aria-label', 'Copy transcript'); }, 1200);
}
document.querySelector('#recent-list').addEventListener('click', copyTranscript);
document.querySelector('#history-list').addEventListener('click', copyTranscript);

document.querySelector('#change-hotkey').addEventListener('click', openHotkeyDialog);
hotkeyCapture.addEventListener('keydown', event => {
  event.preventDefault();
  event.stopPropagation();
  if (event.code === 'Escape') { hotkeyDialog.close(); return; }
  const captured = capturedShortcut(event);
  document.querySelector('.capture-prompt').hidden = Boolean(captured.preview);
  document.querySelector('#capture-keys').innerHTML = shortcutMarkup(captured.preview);
  document.querySelector('#hotkey-error').textContent = '';
  pendingShortcut = captured.preview;
  const modifierCount = captured.preview.split('+').filter(token => ['control', 'alt', 'shift', 'super'].includes(token)).length;
  document.querySelector('#save-hotkey').disabled = !(captured.complete || modifierCount >= 2);
});
document.querySelector('#cancel-hotkey').addEventListener('click', () => hotkeyDialog.close());
document.querySelector('#save-hotkey').addEventListener('click', async () => {
  if (!pendingShortcut) return;
  try {
    renderHotkey(await call('set_hotkey', { hotkey: pendingShortcut }));
    hotkeyDialog.close();
    showToast('Shortcut updated');
  } catch (error) {
    document.querySelector('#hotkey-error').textContent = String(error);
    setTimeout(() => hotkeyCapture.focus(), 0);
  }
});

listen('engine-status', event => renderStatus(event.payload));
listen('model-status', event => renderModel(event.payload));
listen('hotkey-status', event => renderHotkey(event.payload));
listen('audio-warning', event => showToast(event.payload, true));
listen('tray-message', event => showToast(event.payload.message, event.payload.error));
listen('history-updated', event => {
  history.unshift(event.payload);
  history = history.slice(0, 100);
  renderHistory();
});

Promise.all([
  call('get_status'),
  call('get_model_status'),
  call('get_preferences'),
  call('get_history'),
  call('get_hotkey_status'),
  call('get_microphones')
]).then(([engine, model, prefs, items, shortcut, microphones]) => {
  preferences = prefs;
  microphoneStatus = microphones;
  history = items;
  renderStatus(engine);
  renderModel(model);
  renderPreferences();
  renderHistory();
  renderHotkey(shortcut);
});
