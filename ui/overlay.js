const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;
const microphoneLabel = document.querySelector('#microphone-label');
let microphoneTimer;
let persistentNotice = false;
let meetingTitle = 'Untitled meeting';
let meetingPromptOpen = false;
let meetingRecording = false;
const meetingPrompt = document.querySelector('#meeting-prompt');
const meetingIdle = document.querySelector('#meeting-idle');
const meetingStartButton = document.querySelector('#meeting-start');
const meetingDismissButton = document.querySelector('#meeting-dismiss');
const meetingPromptTitle = document.querySelector('#meeting-prompt-title');
const meetingPromptDesc = document.querySelector('#meeting-prompt-desc');
const notetakerButton = document.querySelector('#notetaker-open');
const overlayRow = document.querySelector('#overlay-row');
let lastPhase = 'idle';
const meetingPill = document.querySelector('#meeting-pill');
const meetingPillTitle = document.querySelector('#meeting-pill-title');
const meetingPillStart = document.querySelector('#meeting-pill-start');
const meetingPillDismiss = document.querySelector('#meeting-pill-dismiss');
const meetingPillIcon = document.querySelector('#meeting-pill-icon');
const meetingPillCanvas = document.querySelector('#meeting-pill-canvas');

// Bundled glyphs for known meeting services (browser tabs expose the
// browser's exe icon, never the site favicon, so these cover the web case).
const VENDOR_ICONS = {
  gmeet: '<svg viewBox="0 0 24 24" aria-hidden="true"><path fill="#4caf50" d="M10 8.5A1.5 1.5 0 0 1 11.5 7h3A1.5 1.5 0 0 1 16 8.5v7a1.5 1.5 0 0 1-1.5 1.5h-3a1.5 1.5 0 0 1-1.5-1.5z"/><path fill="#1a73e8" d="M16 10.2l4-2.7v9l-4-2.7z"/><path fill="#ea4335" d="M7 9.5A1.5 1.5 0 0 1 8.5 8H10v8H8.5A1.5 1.5 0 0 1 7 14.5z"/></svg>',
  zoom: '<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="2.5" y="5" width="13" height="14" rx="3" fill="#2d8cff"/><path fill="#2d8cff" d="M15.5 10.3l6-3.6v10.6l-6-3.6z"/></svg>',
  teams: '<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="9" cy="8" r="3.4" fill="#6264a7"/><path fill="#6264a7" d="M2.8 19.2c.7-3.3 3.2-5 6.2-5s5.5 1.7 6.2 5z"/><rect x="15" y="3.5" width="7" height="7" rx="1.6" fill="#6264a7"/><path stroke="#fff" stroke-width="1.3" d="M16.6 7h3.8M18.5 5.1v3.8"/></svg>',
  webex: '<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="9" fill="none" stroke="#00bceb" stroke-width="2.4"/><circle cx="12" cy="12" r="3.4" fill="#00bceb"/></svg>',
  slack: '<svg viewBox="0 0 24 24" aria-hidden="true"><path fill="#36c5f0" d="M9.5 3.5a2 2 0 0 1 2 2v4h-4a2 2 0 0 1 0-4zM9.5 3.5h.01M14.5 3.5a2 2 0 0 1 0 4H10v-4zM20.5 9.5a2 2 0 0 1-2 2h-4v-4a2 2 0 0 1 4 0zM20.5 14.5a2 2 0 0 1-4 0V10h4zM14.5 20.5a2 2 0 0 1-2-2v-4h4a2 2 0 0 1 0 4zM9.5 20.5a2 2 0 0 1 0-4H14v4zM3.5 14.5a2 2 0 0 1 2-2h4v4a2 2 0 0 1-4 0zM3.5 9.5a2 2 0 0 1 4 0V14h-4z"/></svg>',
  discord: '<svg viewBox="0 0 24 24" aria-hidden="true"><path fill="#5865f2" d="M19.6 5.1A16.4 16.4 0 0 0 15.5 4l-.5 1a15 15 0 0 0-6 0L8.5 4a16.4 16.4 0 0 0-4.1 1.1C1.8 9 1.2 12.8 1.5 16.5A16.1 16.1 0 0 0 6.4 19l1-1.7a8.6 8.6 0 0 1-1.4-.7l.3-.3a11.4 11.4 0 0 0 9.4 0l.3.3c-.4.3-.9.5-1.4.7l1 1.7a16.1 16.1 0 0 0 4.9-2.5c.4-4.3-.7-8-2.9-11.4zM8.7 14.3c-.8 0-1.5-.8-1.5-1.7s.7-1.7 1.5-1.7 1.5.8 1.5 1.7-.7 1.7-1.5 1.7zm6.6 0c-.8 0-1.5-.8-1.5-1.7s.7-1.7 1.5-1.7 1.5.8 1.5 1.7-.7 1.7-1.5 1.7z"/></svg>',
  skype: '<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="9" fill="#00aff0"/><path fill="#fff" d="M16.6 14.4c.2-4-2.3-6.4-5.6-6.4-2.8 0-4.9 1.9-4.9 4.2 0 2.5 2.1 3.9 5 4.3l.7 1.2c.1.2.4.2.5 0l.6-1.5c2 .1 3.4-.6 3.7-1.8z"/></svg>'
};

function renderMeetingPillIcon(icon, vendor) {
  if (icon && Number(icon.width) > 0 && Number(icon.height) > 0 && Array.isArray(icon.rgba) && icon.rgba.length >= icon.width * icon.height * 4) {
    try {
      // Render at devicePixelRatio so tray/window icons stay crisp on 125-200% scaling.
      const dpr = Math.min(3, Math.max(1, Number(window.devicePixelRatio) || 1));
      const context = meetingPillCanvas.getContext('2d');
      meetingPillCanvas.width = icon.width * dpr;
      meetingPillCanvas.height = icon.height * dpr;
      meetingPillCanvas.style.width = '20px';
      meetingPillCanvas.style.height = '20px';
      context.setTransform(dpr, 0, 0, dpr, 0, 0);
      context.imageSmoothingEnabled = true;
      context.imageSmoothingQuality = 'high';
      const image = context.createImageData(icon.width, icon.height);
      image.data.set(icon.rgba.slice(0, icon.width * icon.height * 4));
      const offscreen = document.createElement('canvas');
      offscreen.width = icon.width;
      offscreen.height = icon.height;
      offscreen.getContext('2d').putImageData(image, 0, 0);
      context.clearRect(0, 0, icon.width, icon.height);
      context.drawImage(offscreen, 0, 0, icon.width, icon.height);
      meetingPillCanvas.hidden = false;
      meetingPillIcon.querySelectorAll('svg').forEach(node => node.remove());
      return;
    } catch (_) {}
  }
  meetingPillCanvas.hidden = true;
  meetingPillIcon.querySelectorAll('svg').forEach(node => node.remove());
  const glyph = vendor && VENDOR_ICONS[vendor];
  if (glyph) meetingPillIcon.insertAdjacentHTML('beforeend', glyph);
}
let meetingHideTimer = null;
let meetingSequenceActive = false;
let dictating = false;

async function showNotice(text) {
  microphoneLabel.textContent = text;
  microphoneLabel.hidden = false;
  await new Promise(requestAnimationFrame);
  // Measure scrollWidth, not the rendered box: the CSS max-width cap uses
  // 100vw (the still-small window), so long notices render ellipsized and
  // measuring the box would size the window to the truncated text.
  // scrollWidth reports the full text; the backend clamps to the monitor.
  const fullWidth = Math.ceil(microphoneLabel.scrollWidth + 6);
  const responsiveWidth = Math.max(136, Math.min(480, fullWidth));
  await invoke('resize_microphone_overlay', { width: responsiveWidth });
}

async function hideNotice() {
  microphoneLabel.hidden = true;
  await invoke('compact_overlay');
}

function renderMeetingPrompt() {
  meetingIdle.hidden = false;
}

// Size the window to the prompt's real footprint (prompt height + the
// 38px it floats above the pill row + slack) so the card is never cut
// off by a fixed-size window. Width stays fixed; only height is measured.
async function fitMeetingPrompt() {
  await new Promise(requestAnimationFrame);
  const promptHeight = Math.ceil(meetingPrompt.getBoundingClientRect().height);
  const height = Math.min(180, Math.max(30, promptHeight + 38 + 8));
  await invoke('resize_overlay', { width: 300, height });
}

async function showMeetingPrompt(title, question, desc) {
  meetingTitle = title || 'Untitled meeting';
  meetingPromptOpen = true;
  meetingPromptTitle.textContent = question || 'Take notes for this meeting?';
  meetingPromptDesc.textContent = desc || 'Your microphone and computer audio will be saved locally.';
  renderMeetingPrompt();
  meetingPrompt.hidden = false;
  await fitMeetingPrompt();
  meetingStartButton.focus();
}

async function closeMeetingPrompt() {
  meetingPromptOpen = false;
  meetingPrompt.hidden = true;
  if (!meetingRecording) await invoke('dismiss_meeting_prompt');
}

// Dedicated detection pill: the normal recording pill and circle stay
// hidden while this is visible. Start-only by design.
async function showMeetingPill(title, vendor, icon) {
  meetingTitle = title || 'Untitled meeting';
  meetingPromptOpen = true;
  overlayRow.hidden = true;
  meetingPrompt.hidden = true;
  microphoneLabel.hidden = true;
  meetingPillTitle.textContent = title || 'A meeting was detected';
  renderMeetingPillIcon(icon, vendor);
  meetingPill.hidden = false;
  await invoke('resize_overlay', { width: 360, height: 72 });
  meetingPillStart.focus();
}

async function hideMeetingPill({ dismissBackend = false } = {}) {
  meetingPill.hidden = true;
  if (dismissBackend) {
    try { await invoke('dismiss_meeting_suggestion'); } catch (_) {}
  }
}

// Dictation owns the overlay row; make sure a stale detection pill can
// never cover it and the window is back to pill size.
async function showDictationRow() {
  if (meetingPill.hidden && !overlayRow.hidden) return;
  meetingPill.hidden = true;
  meetingPrompt.hidden = true;
  meetingPromptOpen = false;
  overlayRow.hidden = false;
  try { await invoke('compact_overlay'); } catch (_) {}
}

function showMeetingError(message) {
  meetingPromptOpen = true;
  meetingRecording = false;
  renderMeetingPrompt();
  meetingPrompt.hidden = false;
  meetingPromptTitle.textContent = 'Could not start meeting notes';
  meetingPromptDesc.textContent = String(message).replace(/^Error:\s*/, '');
  fitMeetingPrompt();
}

// After confirmation: the circle fades out smoothly in place behind the
// pill (top layer), the status label sits on the bottom layer, the pill
// holds static and centered, then the whole row fades. The tray owns the
// recording from here, so the circle never shows recording state.
// Normal dictation pill UI/behavior is otherwise untouched.
async function playMeetingStartedSequence() {
  clearTimeout(meetingHideTimer);
  meetingSequenceActive = true;
  meetingPromptOpen = false;
  meetingPrompt.hidden = true;
  meetingPill.hidden = true;
  notetakerButton.classList.remove('recording');
  overlayRow.hidden = false;
  overlayRow.classList.remove('meeting-start', 'meeting-flash', 'meeting-gone', 'pill-enter', 'pill-exit');
  // Force reflow so the fade transition restarts cleanly.
  void overlayRow.offsetWidth;
  overlayRow.classList.add('meeting-start', 'meeting-flash');
  await showNotice('Meeting notes started. You can end it from the tray.');
  clearTimeout(microphoneTimer);
  meetingHideTimer = setTimeout(async () => {
    // Same exit as the regular transcription pill (pill-out). meeting-flash
    // stays on through the exit so the X/✓ buttons never pop back in.
    overlayRow.classList.add('meeting-gone');
    // Let the exit finish, then fully hide the pill and restore state.
    setTimeout(async () => {
      microphoneLabel.hidden = true;
      try { await invoke('dismiss_meeting_prompt'); } catch (_) {}
      overlayRow.classList.remove('meeting-start', 'meeting-flash', 'meeting-gone');
      overlayRow.hidden = false;
      meetingSequenceActive = false;
    }, 220);
  }, 3500);
}

async function confirmMeetingStart(button) {
  button.disabled = true;
  try {
    await invoke('start_meeting_recording', { title: meetingTitle });
    meetingRecording = true;
    await playMeetingStartedSequence();
  } catch (error) {
    meetingPill.hidden = true;
    showMeetingError(String(error));
  } finally {
    button.disabled = false;
  }
}

meetingStartButton.addEventListener('click', () => confirmMeetingStart(meetingStartButton));
meetingPillStart.addEventListener('click', () => confirmMeetingStart(meetingPillStart));

meetingDismissButton.addEventListener('click', closeMeetingPrompt);

document.querySelector('#cancel').addEventListener('click', () => invoke('cancel_recording'));
document.querySelector('#finish').addEventListener('click', () => invoke('stop_recording'));
notetakerButton.addEventListener('click', () => {
  if (meetingRecording) {
    // Note taking is controlled from the tray; surface the status label again.
    showNotice('Meeting notes started. You can end it from the tray.');
    clearTimeout(microphoneTimer);
    microphoneTimer = setTimeout(async () => {
      if (!persistentNotice) await hideNotice();
    }, 2500);
    return;
  }
  showMeetingPrompt('Untitled meeting', 'Do you want to start taking notes now?', 'Your microphone and computer audio will be saved locally.');
});

function dismissMeetingPill() {
  if (meetingPill.hidden || meetingRecording) return;
  meetingPromptOpen = false;
  hideMeetingPill({ dismissBackend: true }).finally(() => invoke('dismiss_meeting_prompt').catch(() => {}));
}

document.addEventListener('keydown', event => {
  if (event.key !== 'Escape' || !meetingPromptOpen || meetingRecording) return;
  if (!meetingPill.hidden) {
    dismissMeetingPill();
    return;
  }
  closeMeetingPrompt();
});

meetingPillDismiss.addEventListener('click', dismissMeetingPill);

function playPillEnter() {
  overlayRow.classList.remove('pill-exit');
  void overlayRow.offsetWidth;
  overlayRow.classList.add('pill-enter');
}

function playPillExit() {
  if (meetingSequenceActive) return;
  overlayRow.classList.remove('pill-enter');
  void overlayRow.offsetWidth;
  overlayRow.classList.add('pill-exit');
}

listen('engine-status', event => {
  const phase = event.payload.phase;
  document.documentElement.classList.toggle('processing', phase === 'processing');
  const wasActive = lastPhase === 'listening' || lastPhase === 'processing';
  dictating = phase === 'listening';
  if (dictating) {
    showDictationRow();
    playPillEnter();
  } else {
    // The pill stays up through processing (sweep state); the exit only
    // plays when the window is actually about to hide. The backend waits
    // ~140ms before hiding, which is exactly the exit animation's window:
    // results never wait on it.
    if (wasActive && (phase === 'complete' || phase === 'error' || phase === 'idle')) playPillExit();
  }
  lastPhase = phase;
});

listen('meeting-suggestion', event => {
  if (meetingRecording || meetingSequenceActive || dictating) return;
  const payload = event.payload || {};
  showMeetingPill(payload.title, payload.vendor, payload.icon);
});
listen('meeting-status', event => {
  meetingRecording = Boolean(event.payload.recording);
  // Never re-apply recording styling while the started-sequence owns the UI.
  if (!meetingSequenceActive) notetakerButton.classList.toggle('recording', meetingRecording);
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
