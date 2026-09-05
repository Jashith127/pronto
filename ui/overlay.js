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
const pill = document.querySelector('#dictation-pill');
let meetingHideTimer = null;

async function showNotice(text) {
  microphoneLabel.textContent = text;
  microphoneLabel.hidden = false;
  await new Promise(requestAnimationFrame);
  const responsiveWidth = Math.max(136, Math.ceil(microphoneLabel.getBoundingClientRect().width + 4));
  await invoke('resize_microphone_overlay', { width: responsiveWidth });
}

async function hideNotice() {
  microphoneLabel.hidden = true;
  await invoke('compact_overlay');
}

function renderMeetingPrompt() {
  meetingIdle.hidden = false;
  notetakerButton.classList.toggle('recording', meetingRecording);
}

async function showMeetingPrompt(title, question, desc) {
  meetingTitle = title || 'Untitled meeting';
  meetingPromptOpen = true;
  meetingPromptTitle.textContent = question || 'Take notes for this meeting?';
  meetingPromptDesc.textContent = desc || 'Your microphone and computer audio will be saved locally.';
  renderMeetingPrompt();
  meetingPrompt.hidden = false;
  await invoke('resize_overlay', { width: 300, height: 148 });
  meetingStartButton.focus();
}

async function closeMeetingPrompt() {
  meetingPromptOpen = false;
  meetingPrompt.hidden = true;
  if (!meetingRecording) await invoke('dismiss_meeting_prompt');
}

function showMeetingError(message) {
  meetingPromptOpen = true;
  meetingRecording = false;
  renderMeetingPrompt();
  meetingPrompt.hidden = false;
  meetingPromptTitle.textContent = 'Could not start meeting notes';
  meetingPromptDesc.textContent = String(message).replace(/^Error:\s*/, '');
  invoke('resize_overlay', { width: 300, height: 148 });
}

// After confirmation: circle merges into pill with a brief red waveform,
// a microphone-style status label is shown, then the whole pill disappears.
// Normal dictation pill UI/behavior is otherwise untouched.
async function playMeetingStartedSequence() {
  clearTimeout(meetingHideTimer);
  meetingPromptOpen = false;
  meetingPrompt.hidden = true;
  overlayRow.hidden = false;
  overlayRow.classList.remove('merging', 'meeting-flash', 'meeting-gone');
  // Force reflow so the merge animation restarts cleanly.
  void overlayRow.offsetWidth;
  overlayRow.classList.add('merging', 'meeting-flash');
  await showNotice('Meeting recording has started. You can end it from the tray.');
  clearTimeout(microphoneTimer);
  meetingHideTimer = setTimeout(async () => {
    overlayRow.classList.remove('meeting-flash');
    overlayRow.classList.add('meeting-gone');
    // Let the fade finish, then fully hide the pill and restore state.
    setTimeout(async () => {
      microphoneLabel.hidden = true;
      try { await invoke('dismiss_meeting_prompt'); } catch (_) {}
      overlayRow.classList.remove('merging', 'meeting-flash', 'meeting-gone');
      overlayRow.hidden = false;
    }, 450);
  }, 2200);
}

meetingStartButton.addEventListener('click', async () => {
  meetingStartButton.disabled = true;
  try {
    await invoke('start_meeting_recording', { title: meetingTitle });
    meetingRecording = true;
    notetakerButton.classList.add('recording');
    await playMeetingStartedSequence();
  } catch (error) {
    showMeetingError(String(error));
  } finally {
    meetingStartButton.disabled = false;
  }
});

meetingDismissButton.addEventListener('click', closeMeetingPrompt);

document.querySelector('#cancel').addEventListener('click', () => invoke('cancel_recording'));
document.querySelector('#finish').addEventListener('click', () => invoke('stop_recording'));
notetakerButton.addEventListener('click', () => {
  if (meetingRecording) {
    // Recording is controlled from the tray; surface the status label again.
    showNotice('Meeting recording has started. You can end it from the tray.');
    clearTimeout(microphoneTimer);
    microphoneTimer = setTimeout(async () => {
      if (!persistentNotice) await hideNotice();
    }, 2500);
    return;
  }
  showMeetingPrompt('Untitled meeting', 'Do you want to start the recording now?', 'Your microphone and computer audio will be saved locally.');
});

document.addEventListener('keydown', event => { if (event.key === 'Escape' && meetingPromptOpen && !meetingRecording) closeMeetingPrompt(); });

listen('engine-status', event => {
  document.documentElement.classList.toggle('processing', event.payload.phase === 'processing');
});

listen('meeting-suggestion', event => showMeetingPrompt(event.payload.title));
listen('meeting-status', event => {
  meetingRecording = Boolean(event.payload.recording);
  notetakerButton.classList.toggle('recording', meetingRecording);
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
