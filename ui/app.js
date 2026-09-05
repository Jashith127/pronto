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
let meetings = [];
let selectedMeetingId = null;
let meetingRecording = false;
let meetingStartedAt = 0;

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
bindWindowAction('#maximize', 'toggle_maximize_main_window');
bindWindowAction('#close', 'hide_main_window');
document.querySelector('.titlebar')?.addEventListener('dblclick', event => {
  if (event.target.closest('.window-actions')) return;
  call('toggle_maximize_main_window');
});

const yieldToUI = () => new Promise(resolve => setTimeout(resolve, 0));

async function audioBufferToWavAsync(audioBuffer, onProgress) {
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
  // Process in slices so long files never block the webview (previously the
  // synchronous loop froze the whole app for tens of seconds on long audio).
  const SLICE = 400000;
  for (let start = 0; start < outputLength; start += SLICE) {
    const end = Math.min(start + SLICE, outputLength);
    for (let index = start; index < end; index++) {
      const position = index * ratio;
      const left = Math.min(Math.floor(position), audioBuffer.length - 1);
      const right = Math.min(left + 1, audioBuffer.length - 1);
      const fraction = position - left;
      let sample = 0;
      for (const channel of channels) sample += channel[left] * (1 - fraction) + channel[right] * fraction;
      sample = Math.max(-1, Math.min(1, sample / channels.length));
      view.setInt16(44 + index * 2, sample < 0 ? sample * 32768 : sample * 32767, true);
    }
    if (onProgress) onProgress(end / outputLength);
    await yieldToUI();
  }
  return new Uint8Array(wav);
}

function audioBufferToWav(audioBuffer) {
  // Kept for short-path compat; long files must use audioBufferToWavAsync.
  const targetRate = 16000;
  const outputLength = Math.ceil(audioBuffer.duration * targetRate);
  if (outputLength > 16000 * 120) throw new Error('Use async conversion for long audio');
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

// Sends WAV bytes in small IPC chunks so one giant JSON payload never freezes
// the webview. Falls back to a single invoke for very small files.
async function sendWavForTranscription({ uploadId, fileName, wavBytes, skipHistory, onProgress }) {
  const CHUNK = 512 * 1024;
  if (wavBytes.length <= CHUNK) {
    await call('transcribe_media_file', { fileName, wavBytes: Array.from(wavBytes), skipHistory, uploadId });
    return;
  }
  await call('start_media_upload', { uploadId, fileName, totalBytes: wavBytes.length });
  try {
    for (let offset = 0; offset < wavBytes.length; offset += CHUNK) {
      const slice = wavBytes.subarray(offset, Math.min(offset + CHUNK, wavBytes.length));
      await call('append_media_chunk', { uploadId, chunk: Array.from(slice) });
      if (onProgress) onProgress(Math.min(1, (offset + slice.length) / wavBytes.length));
      await yieldToUI();
    }
    await call('finish_media_upload', { uploadId, skipHistory });
  } catch (error) {
    try { await call('abort_media_upload', { uploadId }); } catch (_) {}
    throw error;
  }
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
    status.textContent = 'Preparing audio (0%)…';
    const wavBytes = await audioBufferToWavAsync(decoded, fraction => {
      status.textContent = `Preparing audio (${Math.round(fraction * 100)}%)…`;
    });
    status.textContent = 'Uploading audio (0%)…';
    // Dictation History imports stay in history; Note Taker uses skipHistory.
    await sendWavForTranscription({
      uploadId: `dict-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
      fileName: file.name,
      wavBytes,
      skipHistory: false,
      onProgress: fraction => { status.textContent = `Uploading audio (${Math.round(fraction * 100)}%)…`; }
    });
    status.textContent = 'Transcribing locally with Parakeet…';
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
listen('meeting-updated', event => {
  meetings = [event.payload, ...meetings.filter(item => item.id !== event.payload.id)];
  selectedMeetingId = event.payload.id;
  notetakerDetail = { kind: 'meeting', id: event.payload.id };
  setView('notetaker');
  renderMeetings();
  showToast('Meeting notes are ready');
});
listen('meeting-processing-error', event => showToast(event.payload, true));
listen('meeting-status', event => {
  meetingRecording = Boolean(event.payload.recording);
  if (meetingRecording) meetingStartedAt = Date.now() - Number(event.payload.elapsedSeconds || 0) * 1000;
  renderMeetingStatus();
});
listen('history-updated', event => {
  history.unshift(event.payload);
  history = history.slice(0, 100);
  renderHistory();
});
listen('notetaker-transcription', event => {
  const payload = event.payload || {};
  notetakerAttachTranscript(payload.entry || payload, payload.uploadId);
});
listen('engine-status', event => {
  if (event.payload && event.payload.phase === 'error' && pendingNotetakerItemId) {
    const found = notetakerFindItem(pendingNotetakerItemId);
    pendingNotetakerItemId = null;
    if (found.item) {
      found.item.status = 'error';
      saveNotetaker();
      renderNotetaker();
    }
  }
});

// ---- Note Taker: file explorer, background transcription, focused reader ----
const NOTETAKER_KEY = 'pronto.notetaker.v1';
const DEFAULT_FOLDER_ID = 'folder-default';
const folderIcon = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3.5 7.5h6l2-2h3l2 2h4v10.5a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2z"/></svg>';
const transcriptIcon = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M7 3.5h7l4 4V20H7zM14 3.5v4h4M10 12h5M10 15.5h5"/></svg>';
let notetaker = loadNotetaker();
let notetakerAudioUrls = new Map();
let pendingNotetakerItemId = null;
let notetakerDetail = null;

function loadNotetaker() {
  let parsed = null;
  try { parsed = JSON.parse(localStorage.getItem(NOTETAKER_KEY) || 'null'); } catch (_) {}
  const state = parsed && Array.isArray(parsed.folders)
    ? { folders: parsed.folders, selectedFolderId: parsed.selectedFolderId || null, meetingCleanups: parsed.meetingCleanups || {} }
    : { folders: [], selectedFolderId: null, meetingCleanups: {} };
  let defaultFolder = state.folders.find(folder => folder.id === DEFAULT_FOLDER_ID);
  if (!defaultFolder) {
    defaultFolder = { id: DEFAULT_FOLDER_ID, name: 'My recordings', createdAt: 0, isDefault: true, items: [] };
    state.folders.unshift(defaultFolder);
  }
  defaultFolder.isDefault = true;
  defaultFolder.items = defaultFolder.items || [];
  state.folders.forEach(folder => { folder.items = folder.items || []; });
  if (!state.folders.some(folder => folder.id === state.selectedFolderId)) state.selectedFolderId = DEFAULT_FOLDER_ID;
  return state;
}
function saveNotetaker() {
  try { localStorage.setItem(NOTETAKER_KEY, JSON.stringify(notetaker)); } catch (_) {}
}
function notetakerFolder(id) { return notetaker.folders.find(folder => folder.id === id) || null; }
function notetakerFindItem(itemId) {
  for (const folder of notetaker.folders) {
    const item = folder.items.find(entry => entry.id === itemId);
    if (item) return { folder, item };
  }
  return {};
}
function wordsIn(text) { return String(text || '').trim().split(/\s+/).filter(Boolean).length; }
let notetakerMeetingTab = {};
let openRowMenu = null;

function renderMarkdown(text) {
  const lines = String(text || '').replace(/\r\n?/g, '\n').split('\n');
  let html = '';
  let listKind = null;
  const closeList = () => { if (listKind) { html += listKind === 'ol' ? '</ol>' : '</ul>'; listKind = null; } };
  const inline = (raw) => {
    let out = escapeHtml(raw);
    out = out.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
    out = out.replace(/(^|[\s(])\*([^*\n]+)\*/g, '$1<em>$2</em>');
    return out;
  };
  for (const line of lines) {
    const trimmed = line.trim();
    if (!trimmed) { closeList(); continue; }
    const heading = trimmed.match(/^(#{1,3})\s+(.*)$/);
    if (heading) {
      closeList();
      const level = heading[1].length;
      html += `<h${level}>${inline(heading[2])}</h${level}>`;
      continue;
    }
    const ordered = trimmed.match(/^(\d+)[.)]\s+(.*)$/);
    const bullet = trimmed.match(/^[-*]\s+(.*)$/);
    if (ordered || bullet) {
      const kind = ordered ? 'ol' : 'ul';
      const content = ordered ? ordered[2] : bullet[1];
      if (listKind !== kind) { closeList(); html += kind === 'ol' ? '<ol>' : '<ul>'; listKind = kind; }
      html += `<li>${inline(content)}</li>`;
      continue;
    }
    closeList();
    html += `<p>${inline(trimmed)}</p>`;
  }
  closeList();
  return html || '<p></p>';
}

function markdownToPlain(text) {
  return String(text || '')
    .replace(/^#{1,3}\s+/gm, '')
    .replace(/\*\*([^*]+)\*\*/g, '$1')
    .replace(/(^|[\s(])\*([^*\n]+)\*/g, '$1$2')
    .replace(/^[-*]\s+/gm, '• ')
    .trim();
}

function meetingTabFor(id, hasNotes) {
  if (!hasNotes) return 'transcript';
  return notetakerMeetingTab[id] || 'notes';
}
function currentTranscript() {
  if (!notetakerDetail) return null;
  if (notetakerDetail.kind === 'meeting') {
    const item = meetings.find(entry => entry.id === notetakerDetail.id);
    return item ? { kind: 'meeting', item, folder: notetakerFolder(DEFAULT_FOLDER_ID), text: notetaker.meetingCleanups[item.id] || item.transcript || '' } : null;
  }
  const found = notetakerFindItem(notetakerDetail.id);
  return found.item ? { kind: 'upload', ...found, text: found.item.cleanedText || found.item.rawText || '' } : null;
}
function folderEntries(folder) {
  const uploads = folder.items.map(item => ({ kind: 'upload', item, createdAt: Number(item.createdAt) || 0 }));
  const recorded = folder.id === DEFAULT_FOLDER_ID
    ? meetings.map(item => ({ kind: 'meeting', item, createdAt: new Date(item.createdAt).getTime() || 0 }))
    : [];
  return [...uploads, ...recorded].sort((a, b) => b.createdAt - a.createdAt);
}
function entryStatus(entry) {
  if (entry.kind === 'meeting' && entry.item.status === 'ready') return 'Notes ready';
  if (entry.item.status === 'ready') return 'Transcript ready';
  if (entry.item.status === 'error' || entry.item.status === 'interrupted') return 'Needs attention';
  if (entry.item.status === 'recording') return 'Recording now';
  if (entry.kind === 'meeting') return 'Creating notes';
  return entry.item.status === 'preparing' ? 'Preparing audio' : 'Transcribing';
}
function entryMarkup(entry) {
  const item = entry.item;
  const ready = item.status === 'ready';
  const failed = item.status === 'error' || item.status === 'interrupted';
  const text = entry.kind === 'meeting' ? item.transcript : item.rawText;
  const detail = ready
    ? (entry.kind === 'meeting'
      ? `${wordsIn(text).toLocaleString()} words · ${formatMeetingDuration(Number(item.durationSeconds || 0))} · notes`
      : `${wordsIn(text).toLocaleString()} words${item.cleanedText ? ' · cleaned' : ' · verbatim'}`)
    : entry.kind === 'meeting' ? 'Recorded locally' : escapeHtml(item.fileName || 'Audio upload');
  const timestamp = entry.kind === 'meeting' ? item.createdAt : item.createdAt;
  const data = entry.kind === 'meeting' ? `data-meeting-id="${escapeAttr(item.id)}"` : `data-item="${escapeAttr(item.id)}"`;
  const menuData = entry.kind === 'meeting' ? `data-row-menu="meeting:${escapeAttr(item.id)}"` : `data-row-menu="upload:${escapeAttr(item.id)}"`;
  return `<div class="notes-file-row${ready ? '' : ' processing'}${failed ? ' failed' : ''}"><button class="notes-file" ${data} ${ready ? '' : 'disabled'}><span class="notes-file-name"><span class="notes-file-icon">${transcriptIcon}</span><span><strong>${escapeHtml(item.title || item.name)}</strong><em>${detail}</em></span></span><span class="notes-file-status">${entryStatus(entry)}</span><span class="notes-file-date">${new Date(timestamp).toLocaleDateString()}</span></button><button class="notes-row-menu-btn" type="button" ${menuData} aria-label="More actions" title="More actions">⋮</button></div>`;
}

function closeRowMenu() {
  openRowMenu = null;
  const layer = document.querySelector('#notetaker-menu-layer');
  if (layer) layer.hidden = true;
}

function openRowMenuAt(kind, id, anchor) {
  const layer = document.querySelector('#notetaker-menu-layer');
  const menu = document.querySelector('#notetaker-menu');
  if (!layer || !menu) return;
  const isMeeting = kind === 'meeting';
  const item = isMeeting ? meetings.find(entry => entry.id === id) : (notetakerFindItem(id).item || null);
  if (!item) return;
  const ready = item.status === 'ready';
  openRowMenu = { kind, id };
  menu.innerHTML = `<button type="button" data-menu-action="rename" role="menuitem">Edit name</button>`
    + (ready ? '' : `<button type="button" data-menu-action="retry" role="menuitem">Try again</button>`)
    + `<button type="button" data-menu-action="delete" class="danger" role="menuitem">Delete</button>`;
  layer.hidden = false;
  const host = document.querySelector('#notetaker');
  const hostRect = host ? host.getBoundingClientRect() : { left: 0, top: 0 };
  const anchorRect = anchor.getBoundingClientRect();
  const menuEl = menu;
  menuEl.style.top = `${Math.max(8, anchorRect.bottom - hostRect.top + 4)}px`;
  menuEl.style.left = `${Math.max(8, anchorRect.right - hostRect.left - 180)}px`;
  const first = menu.querySelector('button');
  if (first) first.focus();
}
function renderNotetaker() {
  const folderList = document.querySelector('#notetaker-folder-list');
  const fileList = document.querySelector('#notetaker-file-list');
  if (!folderList || !fileList) return;
  if (!notetakerFolder(notetaker.selectedFolderId)) notetaker.selectedFolderId = DEFAULT_FOLDER_ID;
  folderList.innerHTML = notetaker.folders.map(folder => {
    const count = folderEntries(folder).length;
    const remove = folder.isDefault ? '<span></span>' : `<button class="notes-folder-delete" data-delete-folder="${escapeAttr(folder.id)}" aria-label="Delete ${escapeAttr(folder.name)}" title="Delete folder">×</button>`;
    return `<div class="notes-folder-row"><button class="notes-folder${folder.id === notetaker.selectedFolderId ? ' active' : ''}" data-folder="${escapeAttr(folder.id)}">${folderIcon}<span>${escapeHtml(folder.name)}</span><small>${count}</small></button>${remove}</div>`;
  }).join('');
  const folder = notetakerFolder(notetaker.selectedFolderId);
  const entries = folderEntries(folder);
  document.querySelector('#notetaker-files-title').textContent = folder.name;
  document.querySelector('#notetaker-files-count').textContent = `${entries.length} ${entries.length === 1 ? 'item' : 'items'}`;
  fileList.innerHTML = entries.length ? entries.map(entryMarkup).join('') : '<div class="empty">This folder is empty.<br><br><button class="secondary-action" type="button" data-empty-upload>Upload audio</button></div>';
  const activeJobs = [
    ...notetaker.folders.flatMap(item => item.items).filter(item => item.status === 'preparing' || item.status === 'transcribing'),
    ...meetings.filter(item => item.status === 'processing')
  ];
  document.querySelector('#notetaker-background-status').textContent = activeJobs.length ? `${activeJobs.length} upload${activeJobs.length === 1 ? '' : 's'} processing in background` : '';
  renderNotetakerDetail();
}
function renderNotetakerDetail() {
  const explorer = document.querySelector('#notetaker-explorer');
  const detailView = document.querySelector('#notetaker-detail');
  const current = currentTranscript();
  if (!current || current.item.status !== 'ready' || !current.text) {
    notetakerDetail = null;
    explorer.hidden = false;
    detailView.hidden = true;
    return;
  }
  explorer.hidden = true;
  detailView.hidden = false;
  const item = current.item;
  const cleaned = current.kind === 'meeting' ? Boolean(notetaker.meetingCleanups[item.id]) : Boolean(item.cleanedText);
  document.querySelector('#notetaker-viewer-title').textContent = item.title || item.name;
  const durationLabel = current.kind === 'meeting' ? ` · ${formatMeetingDuration(Number(item.durationSeconds || 0))}` : '';
  document.querySelector('#notetaker-viewer-meta').textContent = `${current.folder.name} · ${new Date(item.createdAt).toLocaleString()}${durationLabel} · ${wordsIn(current.text).toLocaleString()} words`;
  const audio = document.querySelector('#notetaker-audio');
  const url = current.kind === 'upload' ? notetakerAudioUrls.get(item.id) : null;
  if (url) { audio.src = url; audio.hidden = false; } else { audio.hidden = true; audio.removeAttribute('src'); }
  const tabs = document.querySelector('#notetaker-tabs');
  const summary = document.querySelector('#notetaker-meeting-notes');
  const body = document.querySelector('#notetaker-viewer-body');
  const cleanupButton = document.querySelector('#notetaker-cleanup');
  if (current.kind === 'meeting') {
    const tab = meetingTabFor(item.id, Boolean(item.notes));
    tabs.hidden = false;
    tabs.querySelectorAll('[data-notetaker-tab]').forEach(btn => {
      const active = btn.dataset.notetakerTab === tab;
      btn.classList.toggle('active', active);
      btn.setAttribute('aria-selected', String(active));
    });
    summary.hidden = tab !== 'notes' || !item.notes;
    if (!summary.hidden) summary.querySelector('.notes-markdown').innerHTML = renderMarkdown(item.notes);
    body.hidden = tab !== 'transcript';
    if (!body.hidden) body.innerHTML = cleaned ? `<span class="cleaned-label">Cleaned version</span><div>${escapeHtml(current.text)}</div>` : escapeHtml(current.text);
    cleanupButton.disabled = tab !== 'transcript';
  } else {
    tabs.hidden = true;
    summary.hidden = true;
    body.hidden = false;
    body.innerHTML = cleaned ? `<span class="cleaned-label">Cleaned version</span><div>${escapeHtml(current.text)}</div>` : escapeHtml(current.text);
    cleanupButton.disabled = false;
  }
  const status = document.querySelector('#notetaker-cleanup-status');
  if (!status.dataset.pinned) status.textContent = cleaned && (current.kind !== 'meeting' || meetingTabFor(item.id, Boolean(item.notes)) === 'transcript') ? 'Showing cleaned speech. The original transcript is still preserved.' : '';
}
function notetakerAttachTranscript(entry, uploadId) {
  // Note Taker uploads use skipHistory and arrive via notetaker-transcription
  // with their uploadId; they must never touch Dictation History.
  // Meeting records never enter history either (meeting-updated only).
  const targetId = uploadId || pendingNotetakerItemId;
  if (!targetId) return;
  const found = notetakerFindItem(targetId);
  if (!found.item) {
    if (targetId === pendingNotetakerItemId) pendingNotetakerItemId = null;
    renderNotetaker();
    return;
  }
  pendingNotetakerItemId = null;
  found.item.rawText = entry.finalText || entry.rawText || '';
  found.item.status = found.item.rawText ? 'ready' : 'error';
  found.item.historyId = null;
  saveNotetaker();
  renderNotetaker();
  showToast(`Transcript ready in ${found.folder.name}`);
}
async function notetakerUpload(file) {
  if (pendingNotetakerItemId) { showToast('Another upload is already being transcribed', true); return; }
  const folder = notetakerFolder(notetaker.selectedFolderId) || notetakerFolder(DEFAULT_FOLDER_ID);
  const item = { id: `nt-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`, name: file.name.replace(/\.[^.]+$/, ''), fileName: file.name, createdAt: Date.now(), status: 'preparing', rawText: '', cleanedText: '' };
  folder.items.unshift(item);
  pendingNotetakerItemId = item.id;
  try { notetakerAudioUrls.set(item.id, URL.createObjectURL(file)); } catch (_) {}
  saveNotetaker();
  renderNotetaker();
  try {
    const context = new AudioContext();
    let decoded;
    try { decoded = await context.decodeAudioData(await file.arrayBuffer()); }
    finally { await context.close(); }
    if (!decoded.numberOfChannels || !decoded.length) throw new Error('No audio track was found in this file.');
    if (decoded.duration > 90 * 60) throw new Error('Choose a file shorter than 90 minutes.');
    item.status = 'transcribing';
    saveNotetaker();
    renderNotetaker();
    const wavBytes = await audioBufferToWavAsync(decoded, null);
    await sendWavForTranscription({
      uploadId: item.id,
      fileName: file.name,
      wavBytes,
      skipHistory: true,
      onProgress: null
    });
  } catch (error) {
    pendingNotetakerItemId = null;
    item.status = 'error';
    saveNotetaker();
    renderNotetaker();
    showToast(String(error).replace(/^Error:\s*/, ''), true);
  }
}
async function notetakerCleanup() {
  const current = currentTranscript();
  const status = document.querySelector('#notetaker-cleanup-status');
  const button = document.querySelector('#notetaker-cleanup');
  if (!current) return;
  const original = current.kind === 'meeting' ? current.item.transcript : current.item.rawText;
  button.disabled = true;
  status.dataset.pinned = '1';
  status.textContent = 'Cleaning up speech with the long-form prompt…';
  try {
    const cleaned = await call('cleanup_notetaker_transcript', { text: original });
    if (current.kind === 'meeting') notetaker.meetingCleanups[current.item.id] = cleaned;
    else current.item.cleanedText = cleaned;
    saveNotetaker();
    renderNotetakerDetail();
    status.textContent = 'Showing cleaned speech. The original transcript is still preserved.';
    showToast('Speech cleaned');
  } catch (error) {
    status.textContent = '';
    showToast(String(error).replace(/^Error:\s*/, ''), true);
  } finally {
    delete status.dataset.pinned;
    button.disabled = false;
  }
}
function setNewFolderOpen(open) {
  const form = document.querySelector('#notetaker-new-folder');
  form.hidden = !open;
  document.querySelector('#notetaker-sidebar-plus')?.setAttribute('aria-expanded', String(open));
  if (open) setTimeout(() => document.querySelector('#notetaker-folder-input').focus(), 0);
}

document.querySelector('#notetaker-new-folder')?.addEventListener('submit', event => {
  event.preventDefault();
  const input = document.querySelector('#notetaker-folder-input');
  const name = input.value.trim();
  if (!name) return;
  const folder = { id: `folder-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`, name: name.slice(0, 60), createdAt: Date.now(), items: [] };
  notetaker.folders.push(folder);
  notetaker.selectedFolderId = folder.id;
  input.value = '';
  setNewFolderOpen(false);
  saveNotetaker();
  renderNotetaker();
});
document.querySelector('#notetaker-sidebar-plus')?.addEventListener('click', () => setNewFolderOpen(document.querySelector('#notetaker-new-folder').hidden));
document.querySelector('#notetaker-folder-list')?.addEventListener('click', event => {
  const deleteId = event.target.closest('[data-delete-folder]')?.dataset.deleteFolder;
  if (deleteId) {
    const folder = notetakerFolder(deleteId);
    if (folder?.items.some(item => item.status === 'preparing' || item.status === 'transcribing')) { showToast('Wait for this folder’s upload to finish', true); return; }
    if (!confirm('Delete this folder and all its transcripts?')) return;
    notetaker.folders = notetaker.folders.filter(folder => folder.id !== deleteId);
    if (notetaker.selectedFolderId === deleteId) notetaker.selectedFolderId = DEFAULT_FOLDER_ID;
    saveNotetaker();
    renderNotetaker();
    return;
  }
  const folderId = event.target.closest('[data-folder]')?.dataset.folder;
  if (folderId) { notetaker.selectedFolderId = folderId; saveNotetaker(); renderNotetaker(); }
});
document.querySelector('#notetaker-file-list')?.addEventListener('click', event => {
  if (event.target.closest('[data-empty-upload]')) { document.querySelector('#notetaker-file').click(); return; }
  const menuBtn = event.target.closest('[data-row-menu]');
  if (menuBtn) {
    event.stopPropagation();
    const [kind, id] = String(menuBtn.dataset.rowMenu || '').split(':');
    if (kind && id) {
      if (openRowMenu && openRowMenu.kind === kind && openRowMenu.id === id && !document.querySelector('#notetaker-menu-layer').hidden) closeRowMenu();
      else openRowMenuAt(kind, id, menuBtn);
    }
    return;
  }
  const meetingId = event.target.closest('[data-meeting-id]')?.dataset.meetingId;
  const itemId = event.target.closest('[data-item]')?.dataset.item;
  if (meetingId) notetakerDetail = { kind: 'meeting', id: meetingId };
  if (itemId) notetakerDetail = { kind: 'upload', id: itemId };
  if (meetingId || itemId) renderNotetaker();
});
document.querySelector('#notetaker-menu-layer')?.addEventListener('click', event => {
  if (event.target.id === 'notetaker-menu-layer') closeRowMenu();
});
document.querySelector('#notetaker-menu')?.addEventListener('click', async event => {
  const actionBtn = event.target.closest('[data-menu-action]');
  if (!actionBtn || !openRowMenu) return;
  const { kind, id } = openRowMenu;
  const action = actionBtn.dataset.menuAction;
  closeRowMenu();
  if (action === 'rename') await renameRowItem(kind, id);
  else if (action === 'delete') await deleteRowItem(kind, id);
  else if (action === 'retry') await retryRowItem(kind, id);
});
document.addEventListener('keydown', event => {
  if (event.key === 'Escape' && openRowMenu) closeRowMenu();
});
document.querySelectorAll('#notetaker-tabs [data-notetaker-tab]')?.forEach(btn => btn.addEventListener('click', () => {
  if (!notetakerDetail || notetakerDetail.kind !== 'meeting') return;
  notetakerMeetingTab[notetakerDetail.id] = btn.dataset.notetakerTab;
  renderNotetakerDetail();
}));

async function renameRowItem(kind, id) {
  if (kind === 'meeting') {
    const item = meetings.find(entry => entry.id === id);
    if (!item) return;
    const next = prompt('Rename recording', item.title || '');
    if (next === null) return;
    const title = next.trim();
    if (!title || title === item.title) return;
    try {
      const updated = await call('rename_meeting', { id, title: title.slice(0, 120) });
      meetings = [updated, ...meetings.filter(entry => entry.id !== id)];
      renderNotetaker();
      showToast('Recording renamed');
    } catch (error) { showToast(String(error).replace(/^Error:\s*/, ''), true); }
  } else {
    const found = notetakerFindItem(id);
    if (!found.item) return;
    const next = prompt('Rename recording', found.item.name || '');
    if (next === null) return;
    const name = next.trim();
    if (!name || name === found.item.name) return;
    found.item.name = name.slice(0, 60);
    saveNotetaker();
    renderNotetaker();
    showToast('Recording renamed');
  }
}

async function deleteRowItem(kind, id) {
  if (kind === 'meeting') {
    const item = meetings.find(entry => entry.id === id);
    if (!item) return;
    if (item.status === 'processing' || item.status === 'recording') { showToast('Wait until recording and notes finish before deleting', true); return; }
    if (!confirm(`Delete "${item.title || 'this meeting'}" and its saved audio?`)) return;
    try {
      await call('delete_meeting', { id });
      meetings = meetings.filter(entry => entry.id !== id);
      delete notetakerMeetingTab[id];
      delete notetaker.meetingCleanups[id];
      saveNotetaker();
      if (notetakerDetail && notetakerDetail.kind === 'meeting' && notetakerDetail.id === id) notetakerDetail = null;
      renderNotetaker();
      showToast('Recording deleted');
    } catch (error) { showToast(String(error).replace(/^Error:\s*/, ''), true); }
  } else {
    const found = notetakerFindItem(id);
    if (!found.item) return;
    if (found.item.status === 'preparing' || found.item.status === 'transcribing') { showToast('Wait for this upload to finish', true); return; }
    if (!confirm(`Delete "${found.item.name || 'this transcript'}"?`)) return;
    found.folder.items = found.folder.items.filter(entry => entry.id !== id);
    try { notetakerAudioUrls.get(id) && URL.revokeObjectURL(notetakerAudioUrls.get(id)); } catch (_) {}
    notetakerAudioUrls.delete(id);
    try { await call('delete_notetaker_audio', { itemId: id }); } catch (_) {}
    if (notetakerDetail && notetakerDetail.kind === 'upload' && notetakerDetail.id === id) notetakerDetail = null;
    if (pendingNotetakerItemId === id) pendingNotetakerItemId = null;
    saveNotetaker();
    renderNotetaker();
    showToast('Recording deleted');
  }
}

async function retryRowItem(kind, id) {
  try {
    if (kind === 'meeting') {
      showToast('Regenerating meeting notes…');
      const updated = await call('retry_meeting', { id });
      meetings = [updated, ...meetings.filter(entry => entry.id !== id)];
      renderNotetaker();
    } else {
      const found = notetakerFindItem(id);
      if (!found.item) return;
      if (pendingNotetakerItemId) { showToast('Another upload is already being transcribed', true); return; }
      found.item.status = 'transcribing';
      pendingNotetakerItemId = id;
      saveNotetaker();
      renderNotetaker();
      showToast('Regenerating transcript…');
      await call('retry_notetaker_upload', { itemId: id });
    }
  } catch (error) {
    if (kind !== 'meeting') {
      pendingNotetakerItemId = null;
      const found = notetakerFindItem(id);
      if (found.item) { found.item.status = 'error'; saveNotetaker(); renderNotetaker(); }
    }
    showToast(String(error).replace(/^Error:\s*/, ''), true);
  }
}
document.querySelector('#notetaker-upload')?.addEventListener('click', () => document.querySelector('#notetaker-file').click());
document.querySelector('#notetaker-file')?.addEventListener('change', event => {
  const file = event.target.files?.[0];
  event.target.value = '';
  if (file) notetakerUpload(file);
});
document.querySelector('#notetaker-back')?.addEventListener('click', () => { notetakerDetail = null; renderNotetaker(); });
document.querySelector('#notetaker-cleanup')?.addEventListener('click', notetakerCleanup);
document.querySelector('#notetaker-copy')?.addEventListener('click', async () => {
  const current = currentTranscript();
  if (!current) return;
  const isMeeting = current.kind === 'meeting';
  const tab = isMeeting ? meetingTabFor(current.item.id, Boolean(current.item.notes)) : 'transcript';
  const text = isMeeting && tab === 'notes' ? markdownToPlain(current.item.notes || '') : current.text;
  if (!text) { showToast('Nothing to copy yet', true); return; }
  try { await navigator.clipboard.writeText(text); showToast(tab === 'notes' ? 'Meeting notes copied' : 'Transcript copied'); }
  catch (_) { showToast('Copy failed in this window', true); }
});
renderNotetaker();

function formatMeetingDuration(seconds) {
  const minutes = Math.floor(seconds / 60); const rest = Math.floor(seconds % 60).toString().padStart(2, '0');
  return `${minutes}:${rest}`;
}

function renderMeetingStatus() {
  const button = document.querySelector('#meeting-toggle'); const status = document.querySelector('#meeting-live-status');
  button.classList.toggle('recording', meetingRecording);
  button.querySelector('b').textContent = meetingRecording ? 'Stop and create notes' : 'Start meeting';
  status.textContent = meetingRecording ? `Recording · ${formatMeetingDuration((Date.now() - meetingStartedAt) / 1000)} · saving locally` : '';
}

function renderMeetings() {
  renderNotetaker();
}

document.querySelector('#meeting-toggle')?.addEventListener('click', async event => {
  const button = event.currentTarget; button.disabled = true;
  try {
    if (meetingRecording) {
      const record = await call('stop_meeting_recording'); meetingRecording = false;
      meetings = [record, ...meetings.filter(item => item.id !== record.id)]; selectedMeetingId = record.id; showToast('Recording saved. Creating notes…');
    } else {
      const title = `Meeting ${new Date().toLocaleDateString()} ${new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}`;
      const record = await call('start_meeting_recording', { title }); meetingRecording = true; meetingStartedAt = Date.now();
      meetings = [record, ...meetings.filter(item => item.id !== record.id)]; selectedMeetingId = record.id; showToast('Meeting recording started');
    }
    renderMeetingStatus(); renderMeetings();
  } finally { button.disabled = false; }
});
setInterval(() => { if (meetingRecording) renderMeetingStatus(); }, 1000);

Promise.all([
  call('get_status'),
  call('get_model_status'),
  call('get_preferences'),
  call('get_history'),
  call('get_hotkey_status'),
  call('get_microphones'),
  call('get_meetings'),
  call('get_meeting_status')
]).then(([engine, model, prefs, items, shortcut, microphones, savedMeetings, currentMeeting]) => {
  preferences = prefs;
  microphoneStatus = microphones;
  history = items;
  meetings = savedMeetings;
  meetingRecording = currentMeeting.recording;
  if (meetingRecording) meetingStartedAt = Date.now() - Number(currentMeeting.elapsedSeconds || 0) * 1000;
  renderStatus(engine);
  renderModel(model);
  renderPreferences();
  renderHistory();
  renderHotkey(shortcut);
  renderMeetingStatus();
  renderMeetings();
});
