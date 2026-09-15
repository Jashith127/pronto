# Pronto 0.8.1

Pronto 0.8.1 polishes the 0.8.0 voice search experience end to end: results that arrive faster and linger smarter, a search panel that feels calmer and rounder, a fixed shortcut editor, a full dark mode for the Note Taker, a brand-new app icon, and a heavily rounded main window. No behavior you rely on changes — answers, transcripts, and sounds are all identical, just quicker to reach.

## Search results that keep up

- **Answers paint first, photos follow** — the result panel now appears as soon as the answer is synthesized, with the banner image sliding in right after instead of holding everything up. Image lookups (Wikipedia, OpenGraph, DuckDuckGo) now run concurrently.
- **“Ask next” actually searches** — follow-up chips used to show a “say it out loud” toast; they now run a real text search through the full retrieval + synthesis pipeline, with follow-up-aware query expansion.
- **Click-away parks instead of losing your answer** — dismissing a finished result (backdrop click, Escape, focus loss) tucks it into a small folder-edge tab peeking out above the taskbar at the bottom-left, with a live countdown ring draining on its close button over the two-minute linger. Click the tab to restore the full panel, × to dismiss, or let it auto-dismiss when the ring runs out. The panel × button still closes fully.
- **Links hand off to the browser** — opening a source, image, or the DuckDuckGo button now drops the always-on-top overlay so the browser comes forward instead of opening underneath it.

## Calmer, rounder search panel

- **Official DuckDuckGo mark** — the footer uses the standard DuckDuckGo logo image (crisp at small sizes) with readable text, right-aligned, shortened to “Results by DuckDuckGo · Answer by DeepSeek”.
- **Quieter type label** — Profile / Recipe / How-to / etc. moved from a pill above the title to subtle divider text beside it.
- **Matching pills** — dictation and voice search pills share sizing and waveform motion; dictation keeps its colorful wave with a fixed white finish button (black check), voice search gets its own subtle green.
- **Rounder everything** — larger corner radii on the panel and all inner cards; the layout label, key-fact chips (now an even grid), and bio image/answer heights all line up.

## Fixed + faster under the hood

- **Shortcut editor no longer hangs** — saving a dictation, paste, or search shortcut froze the app on a locking bug; fixed in all three editors.
- **Long meetings no longer freeze the app** — stopping a 1–2 hour recording mixed the microphone and computer captures on the spot, wedging every meeting request behind minutes of disk IO. Stopping now returns in about a second with the meeting marked processing; mixing and transcription continue in the background with live chunk progress, and transcription itself runs on parallel workers.
- **Snappier dictation** — the pipeline lock is held only for the state flip (insertion, history writes, and overlay hide run outside it), and cancel returns immediately.
- **Less waiting on credentials** — the DeepSeek key is cached for 60 seconds instead of hitting Credential Manager on every search and cleanup.
- **Lighter overlay** — no global per-element theme transitions, no fullscreen backdrop blur, cheaper loading shimmer, deferred scripts, and one less blocking stylesheet, so the pill and panel paint sooner.

## Note Taker in dark mode

- The recording detail view (title card, tabs, transcript, meeting notes) and remaining explorer bits now follow the app theme instead of rendering light-on-dark.
- Transcripts use the full responsive panel width, just like meeting notes.

## New look

- **New app icon** — flat dark-grey circle with the orange accent waveform, no more rounded square. Applies to the desktop, taskbar, tray, and installer.
- **Rounded main window** — the app window is now heavily rounded when floating and snaps back to square when maximized.

## Requirements

- Same as 0.8.0: Windows 10 or 11 (64-bit), NVIDIA GPU with current driver, microphone; internet during installation for the one-time model download, plus a DeepSeek API key for full synthesized answers (DuckDuckGo sources still work without one).

---

# Pronto 0.8.0

Pronto 0.8.0 is built around two headline features: **voice search** and **light/dark mode**. Ask a question out loud and get a readable answer in a centered panel — or run the main app and search results in the theme that suits you. Your microphone audio stays on this PC; only the question text and web snippets leave the device for search.

<img width="850" height="529" alt="Pronto 0.8 voice search and themed UI" src="https://github.com/user-attachments/assets/8bdfc3c7-7bdf-4286-bc9b-76267a5ba093" />

## Voice search

Press **Win + Space** (remappable in Settings), speak your question, and Pronto listens from a compact pill at the bottom of the screen. When you finish, a results panel opens in the center with a direct answer, supporting detail, and links back to the web.

<img width="850" height="531" alt="Pronto voice search listening pill and results" src="https://github.com/user-attachments/assets/87585b1a-5156-428a-8a77-9f899858533b" />

- **Answers that fit the question** — profiles, definitions, comparisons, step-by-step guides, timelines, ranked lists, yes/no calls, locations, recipes, and stat-heavy answers each get a layout chosen for that kind of query. Comparisons and data-heavy topics can include tables when they help.
- **Quick facts and follow-ups** — key details appear as scannable chips above the answer; suggested “ask next” prompts help you keep going by voice.
- **Images and sources** — relevant photos when available; a collapsible source list with site icons, titles, and snippets. Tap a source to open it in your browser. The DuckDuckGo footer opens the same query on duckduckgo.com.
- **Grounded when it matters** — answers are synthesized from retrieved web results (DeepSeek). Without an API key, you still get the source list from DuckDuckGo.
- **From dictation** — while dictating, tap the search button on the left of the pill to send what you just said into voice search instead of pasting it.
- **Same listening chrome** — dictation and search share the same dark pill design (waveform, cancel, white finish button). Listening overlays stay dark even when the rest of the app is in light mode.
- **Duck other audio** — the existing setting now lowers playback during voice search as well as dictation, and restores volume when the pill closes.

Dismiss with Escape, click-away, or the panel close button. The overlay hides when idle.

## Light and dark mode

Choose **System**, **Light**, or **Dark** under Settings → **Appearance**. The main Pronto window and the voice search results panel follow your choice; listening pills and dictation overlays keep their original dark look so they stay readable on top of any app.

## Also in 0.8.0

- Voice search shortcut editor and configurable search provider URL in Settings.
- Win + Space may also switch Windows keyboard layouts (low-level hotkey behavior). The Fn key cannot be bound.

## Requirements

- Same as 0.7.5, plus a **DeepSeek API key** for full synthesized answers (DuckDuckGo sources still work without one).

---

# Pronto 0.7.5

Pronto 0.7.5 is a UI polish release: a full spacing/typography/icon pass over the app, overlay clipping and centering fixes, custom dialogs replacing every native browser popup, and a calmer Transcribe-a-File card.

## What's new

- App-wide polish: consistent spacing rhythm and design tokens, readable type scale (no more 8–9px micro-text), unified stroke-style SVG icon set (settings gear, copy, upload, back, plus, close, and row-menu glyphs replace text characters), focus-visible rings, and matching scrollbars.
- Overlay no longer boxes out: drop shadows removed from pill, circle button, mic notice, and meeting cards (transparent windows clip shadows into hard rectangles), and all surfaces are opaque.
- Meeting prompt never cuts off: the window now sizes itself to the card's measured height instead of a fixed 300×148, and the prompt narrowed to fit with slack.
- Prompt triangle repointed: the card stays centered while its pointer targets the note-taker circle's center.
- Meeting-started pill truly centered: the faded circle collapses to zero width instead of holding its 30px slot, and the exit reuses the regular pill's fade-and-settle instead of a long slide.
- Mic and recording notices fit: taller window, roomier label padding, full-text measurement (long names no longer truncate to the old window width), and smaller 11px toast type.
- Pill survives display scaling: the row floats 1px inside a 32px window so fractional scaling can't shave its bottom edge.
- No more native popups: confirmations (clear history, deletes) and renames use custom Pronto-styled modals with danger variants, keyboard support, and backdrop dismiss.
- Meeting notes / Transcript is a real segmented switcher instead of floating pills.
- Transcribe-a-File card simplified: solid card, plain file icon, single formats line, icon button, and a status line that only appears while working.

## Requirements

- Windows 10 or 11, 64-bit.
- A supported NVIDIA GPU and current NVIDIA driver for local CUDA transcription.
- Internet connection during installation (one-time ~681 MB model download).
- DeepSeek API key is optional and is used only for advanced cleanup and meeting-note generation.

---

# Pronto 0.7.4

Pronto 0.7.4 overhauls meeting detection and the meeting pill, fixes Note Taker recordings not opening, enlarges the meeting notes view, reworks overlay motion and the waveform states, redesigns the dictation cues with Bluetooth-aware timing, and makes pasting follow the most recently selected textbox.

## What's new

- Meeting detection rewritten: presence-edge triggering on vendor identity with expanded coverage (Zoom, Teams, Webex, Skype, Jitsi, Chime, and bare "Meet" titles such as Zen browser tabs). Fixes meetings never being detected and the prompt only appearing the first time.
- Repeat meetings always re-prompt: per-session generations mean a meeting that leaves and returns asks again, while Esc/X dismissal stays quiet for that session only (new `dismiss_meeting_suggestion` path, 2-minute blip cooldown).
- Dedicated meeting pill: detection now shows its own pill — red-dot-free capsule with the meeting title, a Start-only button, and an X dismiss — while the normal dictation pill and circle stay hidden. Includes per-app icons (extracted exe icons, bundled service glyphs for browser tabs) on the pill's left.
- Detection never fights dictation: the detector freezes while Pronto dictates or takes notes, and a new "Suggest meeting notes" Settings toggle (on by default) disables pop-ups entirely.
- Note Taker recordings open reliably again: rows are real buttons (no more `display:contents` click/focus bugs), the explorer fits the viewport with internal scrolling, the row menu layer can't swallow clicks, and headers align with rows.
- Meeting notes view fills the detail area: notes and transcript panes share the full height with independent scrolling, and notes text is larger.
- Overlay motion pass: fast pill enter/exit micro-animations (160/130 ms, zero felt latency), smooth circle fade, slide-down exit that keeps its size, layered pill-over-status, and a fix for the red recording style leaking through fades.
- Waveform states: refined idle dance while listening; processing now shows a cool low sweep that reads as working instead of a faster dance that felt like listening.
- Dictation cues redesigned: low discrete A3↔E4 two-tone blips replace the high bubbly glides, with exact normalized loudness (start slightly louder).
- Bluetooth-aware cue timing: the app detects Bluetooth routes and delays the start cue past the hands-free switch gap (capture still starts instantly, so no speech is lost; quick taps skip the late cue). The stop cue plays after endpoint restore on the settled route. Fixes cues being swallowed on Bluetooth headsets.
- Pasting follows the most recently selected textbox: every click is tracked (not just during dictation), so alt-tabbing without clicking pastes into the original box, while clicking a new box redirects there. Dead targets fall back to the clipboard with a toast.
- Meeting UX no longer says "recording": pill, labels, tray ("Stop taking notes"), statuses, and toasts use notes-first wording.

## Validation

- 30 automated tests pass (new coverage: detector generations and vendor keys, icon pixel math, cue direction and loudness, suggestions migration default).
- 10 hardware or interactive tests remain opt-in because they require a microphone, NVIDIA GPU, Windows audio endpoint, desktop input, audio output, or a live DeepSeek key.
- Installer fetch-script hash and failure paths verified locally; full installer compile clean.

## Requirements

- Windows 10 or 11, 64-bit.
- A supported NVIDIA GPU and current NVIDIA driver for local CUDA transcription.
- Internet connection during installation (one-time ~681 MB model download).
- DeepSeek API key is optional and is used only for advanced cleanup and meeting-note generation.

---

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
