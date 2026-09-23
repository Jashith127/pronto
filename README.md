# Pronto

> **Pronto for Mac (Apple Silicon):** the macOS port lives in this same
> repository alongside Windows. The `v0.8.2-macos` release holds the
> MAC-only `Pronto_0.8.2_aarch64.dmg` (ad hoc signed, not notarized).
> The bundle name stays `Pronto`; "Pronto for Mac" is the docs/release name
> for the Mac edition.
> See [the port status and build prerequisites](docs/macos-port-status.md).

Pronto is a push-to-talk dictation application. Hold a global shortcut, speak, and your words appear in whatever application you were typing in -- with local punctuation, cleanup, and history. It processes audio locally and inserts text into the active application.

Two editions share one codebase with clearly separated platform paths:

* **Pronto (Windows):** NVIDIA Parakeet TDT 0.6B v3 via CUDA, Win32 insertion via SendInput, NSIS installer.
* **Pronto for Mac (Apple Silicon, macOS 13+):** Metal speech runtime, Accessibility insertion with clipboard fallback, `.app`/`.dmg` bundle. Dictation uses Control + Option + Space, Paste Last uses Control + Option + V, voice search uses Control + Option + S by default.

![Pronto Dictate screen](docs/screenshot-dictate.png)

## For customers

### Dictate anywhere

* **Push-to-talk, globally.** A configurable shortcut (including modifier-only chords such as Win + Ctrl) starts and stops dictation from any application. Hold-to-talk or press-to-toggle, your choice.
* **Compact recording pill.** A small always-on-top overlay shows cancel, a live waveform, and finish controls without stealing focus from your work.
* **Paste last transcript.** A second global shortcut (default Win + Shift + V) pastes your most recent transcript wherever you are typing.
* **Voice search.** A third global shortcut (default Win + Space) opens a transparent search overlay (not a Pronto app window): speak a query, Pronto retrieves DuckDuckGo HTML results, and DeepSeek synthesizes a grounded on-the-fly UI with citations. The overlay reuses the dictation pill while working, then pops a minimal result panel with the answer first and sources behind a disclosure. Suggested "ask next" prompts run real follow-up searches, and opening any link hands off to your browser and clears the overlay. Clicking away parks the answer as a small tab above the taskbar (bottom-left) with a two-minute countdown on its close button -- click it to restore, or let it expire. Audio stays local; query text and snippets go to DuckDuckGo and DeepSeek. Win + Space may also switch Windows keyboard layouts because the low-level hook calls `CallNextHookEx`.

### Transcription that stays on your computer

* **Local speech engine.** Uses NVIDIA Parakeet TDT 0.6B v3 via CUDA. The model stays loaded in memory, so short phrases transcribe in well under a second with punctuation and capitalization included.
* **Fully offline after setup.** The microphone, transcription, and cleanup all run locally. The network is needed only during installation (one-time model download) and optionally for DeepSeek rewriting.
* **Private by design.** Audio never leaves the machine -- it travels only to a local loopback transcription server. The DeepSeek API receives transcript text only when cloud cleanup is enabled, and the API key is stored in Windows Credential Manager.

### Cleanup and rewriting

* **Local cleanup.** Removes fillers and false starts and repairs punctuation automatically -- no account or key required.
* **Optional DeepSeek rewrite.** Configure an API key to rewrite transcripts with DeepSeek V4 Flash, using an editable system prompt you control.
* **Personal dictionary.** Add names and specialist terms that Pronto must preserve; corrections apply deterministically after recognition.

### Meeting Note Taker

* **Record meetings locally.** Captures microphone plus Windows system audio directly to disk -- no calendar connection or meeting bot required. Local window-title detection offers the recorder when a call is detected.
* **Fast stop, background notes.** Stopping a recording -- even a one-to-two-hour meeting -- returns in about a second. Mixing and transcription continue in the background with live chunk progress, using parallel workers, so the app stays responsive throughout.
* **Background notes.** After recording stops, Pronto transcribes the meeting in chunks and generates structured notes (via DeepSeek when configured, with a local fallback otherwise).
* **Recording library.** Organizes recorded meetings and imported audio into folders with background processing status, word counts, durations, and focused transcript views with audio playback.
* **Audio and video import.** Transcribes the audio track from common media files (MP3, WAV, M4A, MP4, MOV, WebM) locally, from the Dictate screen or any Note Taker folder.

### Look and feel

* **Rounded main window.** The app window is heavily rounded when floating and snaps back to square when maximized.
* **Light and dark mode.** Choose System, Light, or Dark under Settings -- Appearance. Listening pills and dictation overlays keep their dark look so they stay readable on top of any app.
* **Note Taker in dark mode.** Recording libraries, transcripts, and meeting notes all follow the app theme at full panel width.

### Windows integration

* **System tray operation.** Lives in the tray with dictation status, meeting controls, and Paste Last Transcript. Closing the main window keeps background dictation active.
* **Starts at boot.** Optional silent launch into the tray.
* **Dictation sounds and audio ducking.** Short start/stop cues, plus optional ducking that lowers playback while dictating and restores its exact prior state.
* **GPU pressure relief.** Optionally frees the model's VRAM under sustained pressure; the next dictation briefly warms it back up.
* **Dashboard.** Speaking pace (WPM with recent-history chart), words captured, transcript count, and average response time across your locally saved history.

## Performance

On an NVIDIA RTX 4050 Laptop GPU, local transcription of an 11-second audio file takes 86 ms. The microphone device is pre-opened at startup and the model stays resident, so hotkey activation has no device or model-loading delay. Search answers paint as soon as synthesis finishes while photos resolve in the background.

## Requirements

### Windows (Pronto)

* Windows 10 or 11 (64-bit)
* NVIDIA GPU with current display driver
* Microphone
* Internet connection (during installation for the one-time model download, and only for DeepSeek rewriting afterwards)

### macOS (Pronto for Mac)

* Apple Silicon Mac, macOS 13 or newer
* Microphone (Microphone permission granted)
* Accessibility permission for automatic insertion (otherwise the transcript stays in History and the clipboard is left unchanged), Input Monitoring only for modifier-only shortcuts, Screen Recording only for computer-audio meeting capture
* Internet connection (first-launch model download with progress/cancel/retry and SHA-256 verification, plus DeepSeek rewriting afterwards)

## Install

### Windows

1. Download `Pronto_<version>_x64-setup.exe` from this repository's Releases page.
2. Run the installer (per-user, no admin needed). Setup downloads the speech model once with progress and verification.
3. Open **Settings** in Pronto.
4. Optional: Enter a DeepSeek API key. Local transcription works without a key.

### macOS (Pronto for Mac)

1. Download `Pronto_0.8.2_aarch64.dmg` from the MAC-only [`v0.8.2-macos` release](https://github.com/Jashith127/pronto/releases/tag/v0.8.2-macos).
2. Open the DMG and move `Pronto.app` to Applications, then launch it. The build is ad hoc signed and not Apple notarized, so macOS may ask you to allow it manually in System Settings.
3. On first launch the speech model downloads to `~/Library/Application Support/app.pronto.dictation/models/` with progress, cancellation, retry, and checksum verification. Settings also shows macOS permission status.
4. Optional: Enter a DeepSeek API key. Local transcription works without a key. New installations start with Clean up speech turned off; existing saved preferences are respected.

## For developers

### Repository layout

* `ui/` -- static frontend (HTML/CSS/JS) embedded by Tauri. No Node.js or npm involved. Shared by Windows and Mac; platform wording/shortcuts switch at runtime.
* `src-tauri/` -- Rust/Tauri backend: global hotkeys, audio capture, transcription pipeline, overlay windows, tray/menu-bar, installer hooks.
* `src-tauri/src/platform/macos/` -- macOS-only implementations: registered hotkeys + CGEventTap modifier chords, Accessibility insertion, CoreAudio ducking, ScreenCaptureKit computer audio, permissions, power, startup, navigation guard.
* `src-tauri/src/platform_paths.rs` + `src-tauri/src/model_provision.rs` -- standard macOS Application Support/Logs storage and first-launch model download with resume/cancel/SHA-256 verify. Windows storage paths remain intact.
* `src-tauri/tauri.conf.json` -- Windows bundle (`nsis`, `installer-hooks.nsh` model download at setup time).
* `src-tauri/tauri.macos.conf.json` + `Entitlements.plist` + `Info.plist` + `icons/icon.icns` -- macOS bundle (`.app`/`.dmg`, hardened runtime, privacy strings). Merged at build time by `scripts/build-macos.sh`.
* `src-tauri/installer-hooks.nsh` -- NSIS logic that downloads and verifies the speech model at install time (Windows only).
* `scripts/` -- `build-macos.sh` + `prepare-macos-assets.sh` (Mac build/asset staging), `publish-macos-release.ps1` (Windows-only DMG upload: checksum-verify `release-assets/` and `gh release create` -- no Mac needed).
* `.github/workflows/desktop-ci.yml` -- CI matrix: `macos-15` (Apple Silicon check/clippy/test + hotkey bridge) and `windows-2022` (check/test). Future Mac DMGs can be rebuilt from CI without a local Mac.
* `ARCHITECTURE.md` -- pipeline, latency design, storage, and Windows/macOS lifecycle details (see "Windows platform path" and "macOS platform path").
* `RELEASE_NOTES.md` -- per-version changelog.
* `docs/macos-*.md` -- Mac port plan/status/validation, Mac release notes used for the `v0.8.2-macos` GitHub release, and [Windows-only DMG upload](docs/upload-macos-release-from-windows.md).
* `release-assets/` -- local-only Mac DMG + `.sha256` (DMG is gitignored; uploaded as a release asset, never committed).

### How it fits together

A global shortcut wakes a Rust coordinator that captures prewarmed microphone audio, sends it to a persistent local Parakeet server, runs deterministic cleanup (plus optional DeepSeek rewrite), and inserts Unicode text into the previously focused window via SendInput. The frontend receives only status and result events over Tauri IPC -- audio never crosses into JavaScript. See `ARCHITECTURE.md` for the full pipeline.

### Build

The installer is slim (~100 MB): the ~681 MB speech model is excluded from bundle resources and fetched with hash verification during setup. Run these commands in PowerShell to test and build:

```powershell
cd src-tauri
cmd.exe /d /s /c '"C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\Tools\VsDevCmd.bat" -arch=x64 && cargo test --offline'
cargo tauri build

```

The installer lands at `src-tauri/target/release/bundle/nsis`.

### macOS Apple Silicon development build

On macOS 13 or newer with Xcode Command Line Tools and Rust installed:

```sh
scripts/prepare-macos-assets.sh
cd src-tauri
cargo fmt --check
cargo test --target aarch64-apple-darwin
cargo check --target aarch64-apple-darwin
cd ..
scripts/build-macos.sh local
```

The local `.app` and `.dmg` are written under
`src-tauri/target/aarch64-apple-darwin/release/bundle/`. The model is excluded
from the app and is downloaded on first launch to
`~/Library/Application Support/app.pronto.dictation/models/`. Settings shows
progress, cancellation, retry, and macOS permission status. Dictation uses
Control + Option + Space, Paste Last uses Control + Option + V, and voice search
uses Control + Option + S by default.
Key-based shortcuts use macOS registered hotkeys. Modifier-only shortcuts require
Input Monitoring. Automatic insertion into another app requires Accessibility;
without it, Pronto keeps the transcript in History and leaves the clipboard unchanged.
Paste Last inserts the most recent transcript into the focused field when Accessibility
is available. Copy Transcript in History still copies on request.

For a distribution build, install a Developer ID Application certificate and
provide `APPLE_SIGNING_IDENTITY` plus either the App Store Connect API key
variables (`APPLE_API_ISSUER`, `APPLE_API_KEY`, `APPLE_API_KEY_PATH`) or Apple ID
notarization variables (`APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`) through
the environment. Then run `scripts/build-macos.sh signed`. The script checks
the signed app and stapled notarization ticket. No credentials are stored in
the repository. See [the macOS port status](docs/macos-port-status.md) for
remaining acceptance work.

To refresh the Mac DMG later without touching a Mac, rebuild via CI or on a Mac once, copy the new `release-assets/Pronto_*_aarch64.dmg` + `.sha256` to this Windows checkout, and re-run the publish script for the new tag:

```powershell
.\scripts\publish-macos-release.ps1 -Repo Jashith127/pronto -Tag v0.8.2-macos
```

No macOS build tools are needed on the Windows laptop for the upload. The DMG itself stays out of Git (`release-assets/*.dmg` is gitignored); only code, docs, scripts, and the `.sha256` are committed. See
[Upload the macOS DMG from Windows](docs/upload-macos-release-from-windows.md).
The ready asset and SHA-256 checksum are in `release-assets/`. New installations start with Clean up speech
turned off; existing saved preferences are respected.

*Note: Frontend files are static and embedded by Tauri. Do not run Node.js or npm.*

### Windows Behavior

* Pronto runs without a console window.
* Closing the main window keeps background dictation active in the system tray.
* The application keeps the microphone active to avoid device initialization delay.
* Third-party software licenses are available in `THIRD_PARTY_NOTICES.md`.
