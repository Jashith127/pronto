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
