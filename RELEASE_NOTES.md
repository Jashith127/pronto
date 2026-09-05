# Pronto 0.7.3

Pronto 0.7.3 shrinks the installer from ~743 MB to ~101 MB by downloading the speech model during setup, polishes the Note Taker and overlay experience, and makes the tray meeting item follow recording state.

## What's new

- Slim installer (~101 MB): the 681 MB speech model is no longer bundled. The installer downloads it with progress, verifies its SHA256, and retries on failure. Internet is needed once at install time; afterwards transcription stays fully offline.
- Updates and reinstalls skip the download: if the model is already present and its hash matches, setup reuses it instead of fetching ~700 MB again.
- Uninstall removes the downloaded model on real uninstalls (updates keep it). Add/Remove Programs reports a truthful size.
- Note Taker reading view uses flexible heights: meeting notes and transcript share the window proportionally with independent scrolling instead of fixed caps. The explorer grows with tall windows.
- Whole-app UX pass: visible keyboard focus on recording rows, hover-reveal row menus with press feedback, inline Upload action in empty folders, delete blocked while an item is still recording or processing, and reduced-motion support for new transitions.
- Meeting overlay reworked: explicit pill-on-top layering, the circle fades out smoothly in place (no more zoom-out), the pill holds ~3.5s while static and centered, then the row fades. Fixed the red recording style leaking into and after the fade.
- System tray meeting item now reads "Stop meeting recording" while recording and stops the meeting when clicked, with toast feedback. It reverts to "Take meeting notes" afterwards.
- New global "paste last transcript" shortcut (default Win + Shift + V): pastes your most recent transcript wherever you are typing, without opening Pronto. Fully configurable in Settings; the two shortcuts can never collide.

## Validation

- 23 automated tests pass.
- 10 hardware or interactive tests remain opt-in because they require a microphone, NVIDIA GPU, Windows audio endpoint, desktop input, audio output, or a live DeepSeek key.
- Installer fetch-script hash and failure paths verified locally; full installer compile clean.

## Requirements

- Windows 10 or 11, 64-bit.
- A supported NVIDIA GPU and current NVIDIA driver for local CUDA transcription.
- Internet connection during installation (one-time ~681 MB model download).
- DeepSeek API key is optional and is used only for advanced cleanup and meeting-note generation.

---

# Pronto 0.7.2

Pronto 0.7.2 fixes freezing on long-file transcription, separates Note Taker and meeting uploads from Dictation History, reworks the meeting overlay flow, and upgrades the Note Taker workspace.

## What's new

- Long audio/video files no longer freeze the app. WAV conversion runs in slices with progress, and audio uploads travel to the backend in small chunks instead of one giant payload.
- Note Taker file uploads and meeting recordings no longer appear in the Dictation History clipboard. Note Taker transcripts arrive on a dedicated channel; Dictation History only holds live dictations and Dictate-screen imports.
- Meeting overlay after confirmation: the right-side circle animates merging into the pill, the waveform flashes red only for the animation, and a microphone-style label reads "Meeting recording has started. You can end it from the tray." The second overlay box above the pill is gone, X/✓ stay hidden in meeting state, and the pill fully disappears after a few seconds. Recording continues and is stopped from the tray. Normal dictation pill behavior is unchanged.
- Note Taker reading view is now responsive: transcript text fills the window width instead of a fixed column. The Back button is an icon-only button with a bolder arrow.
- Meeting notes now render Markdown (headings, bold, lists) instead of raw `#`/`**` markers, in a scrollable notes pane with **Meeting notes** / **Transcript** tabs. Copy follows the active tab.
- Each recording row has a `⋮` menu: **Edit name** and **Delete** (with confirmation) for everything, plus **Try again** for anything not ready. Meeting delete removes its audio and record from disk.
- Failed Note Taker uploads keep their saved audio on disk, so they can be retried even after restart; meetings retry from their saved `meeting.wav`.
- Upload audio button now shows an upload (up-arrow) icon instead of a download arrow.

## Validation

- 23 automated tests pass.
- 10 hardware or interactive tests remain opt-in because they require a microphone, NVIDIA GPU, Windows audio endpoint, desktop input, audio output, or a live DeepSeek key.

## Requirements

- Windows 10 or 11, 64-bit.
- A supported NVIDIA GPU and current NVIDIA driver for local CUDA transcription.
- DeepSeek API key is optional and is used only for advanced cleanup and meeting-note generation.

---

# Pronto 0.7.1

Pronto 0.7.1 cleans up the dictation pill, gives meeting notes their own space, and adds quick access to Note Taker plus a standard maximize control.

## What's new

- Dictation pill restored to its compact centered shape (cancel, waveform, finish) with the liquid waveform color scheme kept. Each waveform bar now dances at its own pace like a live waveform.
- Meeting notes moved out of the pill into a dedicated prompt bubble with **Start meeting notes** / **Not now** actions and a recording state with timer and **Stop and create notes**. The pill stays visible beside the prompt.
- New round recording button (thick ring with a center dot, turning red while recording) sits close to the pill. Clicking it asks "Do you want to start the recording now?" and starts recording directly from the overlay without opening the main Pronto window.
- Starting a meeting now automatically stops any live dictation first, so a running transcript can no longer block recording.
- Finished meetings open themselves in Note Taker with duration, word count, and a "Notes ready" state instead of waiting in the list.
- Hovering the meeting button no longer shows a loading cursor.
- Note Taker header simplified: the extra **New folder** button is removed (folder creation stays in the sidebar) and **Start meeting** / **Upload audio** are larger and clearer, with a more neutral, higher-end label typeface.
- Main window now has minimize, maximize/restore, and close controls with matching line-icon styling, including double-click on the title bar to toggle maximize.
- Release builds now strip symbols to trim the executable.

## Package size

- The installer remains large (~740 MB) because it bundles the ~714 MB Parakeet speech model and ~121 MB CUDA runtime so transcription works fully offline. The executable trim above saves only a few megabytes; a substantially smaller installer would require downloading the model on first launch instead of bundling it.

---

# Pronto 0.7.0

Pronto 0.7 adds local meeting recording and turns Note Taker into a focused workspace for recorded meetings, imported audio, transcripts, and notes—without requiring a calendar connection or meeting bot.

## Meeting recording

- Record the microphone and Windows system audio together during Google Meet, presentations, and other desktop calls.
- Audio is written continuously to local WAV files while the meeting is happening, limiting memory use and protecting completed audio if Pronto closes unexpectedly.
- Meeting detection is local and title-based. A small pinned prompt offers to take notes when Pronto recognizes a supported meeting window; no Google Calendar connection or account sync is required.
- The compact meeting pill uses a dedicated hollow-circle recording control. Its waveform keeps a static silhouette while a grainy indigo, magenta, coral, and amber liquid texture drifts subtly inside it.
- Recording does not run speech recognition or note generation during the call. After stopping, Pronto mixes the captured sources, transcribes the meeting in chunks with local Parakeet, and creates notes in the background.
- DeepSeek creates structured notes when configured; a local fallback still produces a usable meeting summary without an API key.
- Interrupted recordings are recovered and surfaced in Note Taker instead of being silently discarded.

## Note Taker

- Note Taker now opens as a two-pane file explorer with folders on the left and transcript files on the right.
- A permanent **My recordings** folder is created automatically for meeting recordings and quick uploads.
- Use either **+ New folder** control to organize imported recordings before uploading them.
- Uploads appear in the selected folder immediately and report preparation and transcription status while processing continues in the background.
- Recorded meetings and imported audio share the same explorer, with clear ready, processing, and attention states.
- Only completed transcripts can open. A ready transcript gets a dedicated reading view with **Back**, **Copy**, and **Clean Up Speech** controls.
- Recorded-meeting notes appear above the full transcript. Manual cleanup preserves the original transcript.

## Performance and privacy

- Live recording is limited to lightweight native audio capture and buffered disk writes; GPU transcription and note generation begin only after recording stops.
- Microphone and system-audio capture remain on the computer. Only optional DeepSeek cleanup or note generation sends transcript text to the configured service.
- The generated liquid waveform texture is optimized to approximately 142 KB and respects the system reduced-motion preference.

## Validation

- 23 automated tests pass.
- 10 hardware or interactive tests remain opt-in because they require a microphone, NVIDIA GPU, Windows audio endpoint, desktop input, audio output, or a live DeepSeek key.

## Requirements

- Windows 10 or 11, 64-bit.
- A supported NVIDIA GPU and current NVIDIA driver for local CUDA transcription.
- Microphone permission; system-audio recording uses the active Windows output endpoint.
- DeepSeek API key is optional and is used only for advanced cleanup and meeting-note generation.

---

# Pronto 0.6.1

Pronto 0.6.1 adds the Note Taker workspace, makes file transcripts verbatim by default, and gives the recording pill an acid-visual treatment.

## What's new

- New **Note Taker** tab: create folders, upload audio/video per folder, and keep multiple transcript files in each folder.
- Click any file to read the full transcript in the viewer; in-session audio playback is included via the upload's local audio URL.
- File imports (Dictate screen and Note Taker) no longer run automatic speech cleanup, even when "Clean up speech" is enabled for live dictation. Live dictation behavior is unchanged.
- New **Clean Up Speech** button in the Note Taker viewer: manually cleans the verbatim transcript with a dedicated long-form interview prompt (preserves all content and speaker order, removes fillers/false starts, scales output budget up to 8192 tokens). Original verbatim text is kept alongside the cleaned version.
- Recording pill now uses a saturated acid-style animation (rapid hue cycling, wobble, flicker, stronger grain). Processing-state animation is unchanged.

## Validation

- 21 automated tests pass.
- 10 hardware or interactive tests remain opt-in because they require a microphone, NVIDIA GPU, desktop input, audio output, or a live DeepSeek key.

## Requirements

- Windows 10 or 11, 64-bit.
- A supported NVIDIA GPU and current NVIDIA driver for local CUDA transcription.
- DeepSeek API key required only for manual Clean Up Speech.

---

# Pronto 0.6.0

Pronto 0.6.0 expands local transcription beyond live dictation and makes the CUDA speech-engine lifecycle significantly more reliable.

## What’s new

- Transcribe audio and video files directly from the Dictate screen.
- Supports browser-decodable formats such as MP3, WAV, M4A, MP4, MOV, and WebM.
- Media audio is decoded, downmixed, and resampled locally before being sent to Parakeet; files never leave the computer.
- Imported transcripts use the same language, dictionary, and cleanup settings as live dictation and are saved in History.
- Imported transcripts are not automatically inserted into the active application.
- Added clear errors for unsupported media, missing audio tracks, concurrent transcription, and files longer than 90 minutes.

## Reliability fixes

- GPU-memory model release is now opt-in and is disabled once for users upgrading from earlier releases, preventing unload/reload loops on constrained GPUs.
- A failed Parakeet startup now enters a retry cooldown instead of immediately stacking another long warm-up attempt.
- The bundled speech process now writes a rotating `engine.log` under Pronto’s local data folder.
- Startup detects an early `nemo-speech.exe` exit and reports its exit status plus the latest engine diagnostic.
- Transcription requests have bounded, duration-aware timeouts so the UI cannot remain in Processing indefinitely.
- Model warm-up, GPU-memory waits, retry cooldowns, and failures are shown in the recording overlay.

## Visual update

- The recording waveform now carries a moving coral, magenta, violet, and navy gradient inspired by analog light.
- Added a restrained moving film-grain texture inside the waveform.
- Listening and processing use distinct motion states.
- Added a static reduced-motion treatment for accessibility.

## Validation

- 21 automated tests pass.
- 10 hardware or interactive tests remain opt-in because they require a microphone, NVIDIA GPU, desktop input, audio output, or a live DeepSeek key.

## Requirements

- Windows 10 or 11, 64-bit.
- A supported NVIDIA GPU and current NVIDIA driver for local CUDA transcription.
