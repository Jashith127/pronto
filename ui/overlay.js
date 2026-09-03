const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;
const microphoneLabel = document.querySelector('#microphone-label');
const overlayRow = document.querySelector('#overlay-row');
let microphoneTimer;
let persistentNotice = false;
let meetingTitle = 'Untitled meeting';
let meetingPromptOpen = false;
let meetingRecording = false;
let meetingStartedAt = 0;
const meetingPrompt = document.querySelector('#meeting-prompt');
const meetingIdle = document.querySelector('#meeting-idle');
const meetingActive = document.querySelector('#meeting-active');
const meetingStartButton = document.querySelector('#meeting-start');
const meetingDismissButton = document.querySelector('#meeting-dismiss');
const meetingStopButton = document.querySelector('#meeting-stop');
const meetingPromptTitle = document.querySelector('#meeting-prompt-title');
const meetingPromptDesc = document.querySelector('#meeting-prompt-desc');
const meetingTime = document.querySelector('#meeting-time');
const notetakerButton = document.querySelector('#notetaker-open');

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

function formatTime(seconds) {
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60).toString().padStart(2, '0');
  const rest = Math.floor(seconds % 60).toString().padStart(2, '0');
  return hours ? `${hours}:${minutes}:${rest}` : `${minutes}:${rest}`;
}

function renderMeetingPrompt() {
  meetingIdle.hidden = meetingRecording;
  meetingActive.hidden = !meetingRecording;
  notetakerButton.classList.toggle('recording', meetingRecording);
}

async function showMeetingPrompt(title, question, desc) {
  meetingTitle = title || 'Untitled meeting';
  meetingPromptOpen = true;
  meetingPromptTitle.textContent = question || 'Take notes for this meeting?';
  meetingPromptDesc.textContent = desc || 'Your microphone and computer audio will be saved locally.';
  renderMeetingPrompt();
  meetingPrompt.hidden = false;
  overlayRow.hidden = true;
  await invoke('resize_overlay', { width: 300, height: 148 });
  (meetingRecording ? meetingStopButton : meetingStartButton).focus();
}

async function closeMeetingPrompt() {
  meetingPromptOpen = false;
  meetingPrompt.hidden = true;
  overlayRow.hidden = false;
  if (!meetingRecording) await invoke('dismiss_meeting_prompt');
}

function showMeetingError(message) {
  meetingPromptOpen = true;
  meetingRecording = false;
  renderMeetingPrompt();
  meetingPrompt.hidden = false;
  overlayRow.hidden = true;
  meetingPromptTitle.textContent = 'Could not start meeting notes';
  meetingPromptDesc.textContent = String(message).replace(/^Error:\s*/, '');
  invoke('resize_overlay', { width: 300, height: 148 });
}

meetingStartButton.addEventListener('click', async () => {
  meetingStartButton.disabled = true;
  try {
    await invoke('start_meeting_recording', { title: meetingTitle });
    meetingPromptOpen = true;
    meetingRecording = true;
    meetingStartedAt = Date.now();
    renderMeetingPrompt();
    meetingPrompt.hidden = false;
    overlayRow.hidden = true;
    await invoke('resize_overlay', { width: 300, height: 132 });
  } catch (error) {
    showMeetingError(String(error));
  } finally {
    meetingStartButton.disabled = false;
  }
});

meetingStopButton.addEventListener('click', async () => {
  meetingStopButton.disabled = true;
  try {
    await invoke('stop_meeting_recording');
    meetingRecording = false;
    await invoke('dismiss_meeting_prompt');
  } finally {
    meetingStopButton.disabled = false;
  }
});

meetingDismissButton.addEventListener('click', closeMeetingPrompt);

document.querySelector('#cancel').addEventListener('click', () => invoke('cancel_recording'));
document.querySelector('#finish').addEventListener('click', () => invoke('stop_recording'));
notetakerButton.addEventListener('click', () => {
  if (meetingRecording) {
    renderMeetingPrompt();
    meetingPrompt.hidden = false;
    overlayRow.hidden = true;
    invoke('resize_overlay', { width: 300, height: 132 });
    meetingStopButton.focus();
    return;
  }
  showMeetingPrompt('Untitled meeting', 'Do you want to start the recording now?', 'Your microphone and computer audio will be saved locally.');
});

document.addEventListener('keydown', event => { if (event.key === 'Escape' && meetingPromptOpen && !meetingRecording) closeMeetingPrompt(); });
setInterval(() => { if (meetingRecording) meetingTime.textContent = formatTime((Date.now() - meetingStartedAt) / 1000); }, 1000);

listen('engine-status', event => {
  document.documentElement.classList.toggle('processing', event.payload.phase === 'processing');
});

listen('meeting-suggestion', event => showMeetingPrompt(event.payload.title));
listen('meeting-status', event => {
  const wasRecording = meetingRecording;
  meetingRecording = Boolean(event.payload.recording);
  if (meetingRecording && !wasRecording) meetingStartedAt = Date.now() - Number(event.payload.elapsedSeconds || 0) * 1000;
  notetakerButton.classList.toggle('recording', meetingRecording);
  if (meetingPromptOpen) {
    renderMeetingPrompt();
    meetingPrompt.hidden = false;
    overlayRow.hidden = true;
  }
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
