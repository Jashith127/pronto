const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;
const microphoneLabel = document.querySelector('#microphone-label');
let microphoneTimer;
let persistentNotice = false;
let meetingTitle = 'Untitled meeting';
let meetingPromptOpen = false;
let meetingRecording = false;
let meetingStartedAt = 0;
const meetingPrompt = document.querySelector('#meeting-prompt');
const meetingButton = document.querySelector('#meeting-record');
const meetingTime = document.querySelector('#meeting-time');

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

function formatTime(seconds) {
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60).toString().padStart(2, '0');
  const rest = Math.floor(seconds % 60).toString().padStart(2, '0');
  return hours ? `${hours}:${minutes}:${rest}` : `${minutes}:${rest}`;
}

async function showMeetingPrompt(title) {
  meetingTitle = title || 'Untitled meeting'; meetingPromptOpen = true; meetingPrompt.hidden = false;
  await invoke('resize_overlay', { width: 300, height: 104 }); meetingButton.focus();
}

async function closeMeetingPrompt() {
  meetingPromptOpen = false; meetingPrompt.hidden = true;
  if (!meetingRecording) await invoke('dismiss_meeting_prompt');
}

meetingButton.addEventListener('click', async () => {
  try {
    if (meetingRecording) {
      meetingButton.disabled = true; await invoke('stop_meeting_recording'); meetingRecording = false;
      meetingButton.classList.remove('recording'); meetingButton.setAttribute('aria-label', 'Start meeting notes'); meetingTime.hidden = true;
      await invoke('dismiss_meeting_prompt'); return;
    }
    if (!meetingPromptOpen) { await showMeetingPrompt('Untitled meeting'); return; }
    meetingButton.disabled = true; await invoke('start_meeting_recording', { title: meetingTitle });
    meetingPromptOpen = false; meetingPrompt.hidden = true; meetingRecording = true; meetingStartedAt = Date.now();
    meetingButton.classList.add('recording'); meetingButton.setAttribute('aria-label', 'Stop meeting and create notes'); meetingTime.hidden = false;
    await invoke('resize_overlay', { width: 148, height: 30 });
  } catch (error) {
    meetingPromptOpen = true; meetingPrompt.hidden = false;
    meetingPrompt.querySelector('strong').textContent = 'Could not start meeting notes';
    meetingPrompt.querySelector('span').textContent = String(error).replace(/^Error:\s*/, '');
    await invoke('resize_overlay', { width: 300, height: 104 });
  } finally { meetingButton.disabled = false; }
});

document.addEventListener('keydown', event => { if (event.key === 'Escape' && meetingPromptOpen) closeMeetingPrompt(); });
setInterval(() => { if (meetingRecording) meetingTime.textContent = formatTime((Date.now() - meetingStartedAt) / 1000); }, 1000);

listen('engine-status', event => {
  document.documentElement.classList.toggle('processing', event.payload.phase === 'processing');
  meetingButton.disabled = event.payload.phase === 'listening' || event.payload.phase === 'processing';
});

listen('meeting-suggestion', event => showMeetingPrompt(event.payload.title));
listen('meeting-status', event => {
  meetingRecording = Boolean(event.payload.recording);
  meetingButton.classList.toggle('recording', meetingRecording);
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
