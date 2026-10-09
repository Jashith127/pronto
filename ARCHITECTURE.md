# Architecture

This document describes how Pronto 1.0 works. It covers the dictation pipeline, the speech engines, voice search, the Note Taker, the installer, and the Windows and macOS platform paths.

## Overview

Pronto is a Tauri app. The backend is Rust. The frontend is static HTML, CSS and JavaScript in `ui/`, with no build step.

The backend owns all audio. Audio never goes into the WebView. The frontend sends commands and gets status and result events through Tauri IPC.

Pronto has three windows:

* **Main window.** Dictate, History, Note Taker, Dictionary and Settings.
* **Dictation overlay.** The listening pill, meeting prompts and microphone notices. It does not take the focus.
* **Search overlay.** The voice search pill and the result panel. It is transparent and always on top.

## Dictation pipeline (Windows)

```text
Global shortcuts (WH_KEYBOARD_LL hook)
  dictation     default Ctrl+Alt+Space
  paste last    default Win+Shift+V
  voice search  default Win+Space
      |
      v
Rust coordinator --------------------------> overlays + main window (events only)
      |
      +--> optional Core Audio ducking (exact restore after)
      |
      +--> prewarmed CPAL/WASAPI capture
      |      downmix -> 16 kHz -> silence trim
      |
      +--> live segments (Parakeet only)
      |      cut at pauses of 0.8 s or more, after 3 s of audio
      |      finished segments transcribe while the user speaks
      |
      +--> local speech server on loopback (/v1/audio/transcriptions)
      |      Parakeet TDT 0.6B v3 on CUDA, or Phonon-2 on the CPU
      |
      +--> dictation path:
      |      local cleanup + dictionary corrections
      |      optional AI rewrite through the selected provider
      |      Win32 SendInput Unicode insertion + local history
      |
      +--> voice search path (search shortcut only):
             local cleanup only (no AI rewrite, no history entry)
             DuckDuckGo HTML retrieval through the SearchProvider trait
             JSON UI synthesis through the selected provider (design://system/v1)
             search overlay renders allowlisted nodes
```

The shortcut hook uses a hold/toggle state machine. The three shortcuts must be different chords. Pronto saves them in a canonical form.

## Latency design

1. Pronto starts the speech server one time and keeps it loaded.
2. Pronto opens the microphone at startup. A shortcut only clears a buffer and resumes the stream.
3. Audio stays in Rust. JavaScript gets only status and result events.
4. The capture callback only collects samples. Resampling and inference run on other threads.
5. **Live segments.** With Parakeet, Pronto transcribes finished segments while the user speaks. At stop, only the audio after the last pause is left. If a segment fails, or the engine is Phonon, or the capture path is WaveIn, Pronto transcribes the full recording.
6. **AI pre-connect.** When AI cleanup is on, dictation start opens the provider connection. This removes DNS and TLS time from the cleanup call.
7. Pronto uses one HTTP client again for all provider requests. Reasoning is off for cleanup.
8. Local cleanup is always available. AI cleanup is optional, and it has its own timeout.
9. Each dictation records capture, ASR, cleanup and total times.

Measured on Parakeet (RTX 4050 laptop, on battery): the median time from stop to text went from 0.66 s to 0.34 s with live segments. The p95 went from 0.94 s to 0.50 s. A 63-second clip went from 0.94 s to 0.37 s. See `scripts/benchmarks/README.md` for the method and all results.

AI cleanup depends on the network. Pronto measures it, but cannot make it less than one second on every connection.

## Speech engines (Windows)

Pronto runs one of two local engines. Both use the same loopback `/v1/audio/transcriptions` contract. The user selects the engine in Pronto Setup, and can change it in **Settings → Speech engine**.

| Engine | Runtime | Device | Languages | Notes |
|---|---|---|---|---|
| Parakeet TDT 0.6B v3 (Q8 GGUF) | NeMo-Speech.cpp `nemo-speech.exe` | CUDA | 25 | Default with an NVIDIA GPU. Approximately 128x real time on an RTX 4050 laptop. |
| Phonon-2 (Fermion Research) | Embedded Python + `fermion-research` (`python -m fermion.cli serve <dir>`) | CPU | English | For PCs without an NVIDIA GPU. WER 5.21, against 4.96 for Parakeet. |

Phonon-2 has a CUDA path only as a Linux container. On NVIDIA hardware, Parakeet is faster and more accurate. Thus Pronto uses Phonon only on the CPU.

The engine thread owns one server at a time. To change engines, it stops the server, saves the choice, and starts the other engine.

Engine-specific behavior:

* Phonon does not accept the `model` and `language` form fields. It can take up to 180 s to load.
* Pronto opts the Phonon server out of Windows power throttling (EcoQoS) after spawn. Without this, warm transcription was 3 to 4 times slower on an i5-12450H.
* GPU-pressure release applies only to Parakeet.
* Both engines transcribe meeting chunks one at a time. `nemo-speech` serves one request at a time, and parallel requests caused stalls and aborted connections.

### Why Parakeet

Parakeet TDT 0.6B v3 is a better fit than Whisper for this use. It is small enough to stay loaded. It has very high throughput. It returns punctuation and capitalization. It supports 25 European languages. Pronto uses the native Windows CUDA runtime, not a Python service.

A warm request transcribed the 11-second validation sample in 74 ms (86 ms through the full pipeline). A cold process took approximately 892 ms.

This runtime does not support word boosting. Thus Pronto applies dictionary corrections after ASR, and the AI cleanup prompt repeats them. The corrections are predictable and do not need the cloud.

### Speech packs

Both engines come as *packs*, listed in `crates/speech-packs/speech-packs.manifest`:

* Each pack is a pinned HTTPS release asset.
* Pronto verifies the size and the SHA-256 of each pack.
* Downloads continue after a cancel or a connection failure.
* Pronto unpacks to a staging folder, and moves it into place only when it is complete.

Packs are in `%LOCALAPPDATA%\Pronto` (`models/`, `runtimes/`). A CUDA runtime from the legacy NSIS installer also satisfies the CUDA pack.

## AI providers

`src-tauri/src/cleanup_provider.rs` sends all AI requests through the provider that the user selects:

* DeepSeek (recommended)
* OpenAI
* Anthropic
* Google Gemini
* Groq
* OpenRouter
* Custom: any endpoint compatible with OpenAI, for example Ollama or LM Studio

The same provider does dictation cleanup, Note Taker cleanup, meeting notes and voice search answers.

* Each provider has its own key in Windows Credential Manager. DeepSeek uses the same keyring account as before, so old keys continue to work. Pronto caches a key for 60 seconds.
* Settings gets the live model list from the provider (`/models`, or `/v1/models` for Anthropic). It removes embedding, speech, image and moderation models. The provider default is first. **Other...** lets the user type a model name.
* Search answers have an 8 s timeout on hosted chat providers, and 20 s on Anthropic and custom endpoints.

## Voice search

* The default shortcut is **Win + Space**. Windows also uses this chord to change the keyboard layout. The Pronto hook always calls `CallNextHookEx`, so the layout can change at the same time. Pronto cannot bind the Fn key, because it has no virtual-key code.
* Listening and searching use one pill at the bottom of the screen. Green bars show while Pronto listens. A sweep shows while it works. The result panel then opens in the center.
* The overlay hides when it is idle. Click-away, focus loss or Escape puts the answer in a small tab at the bottom-left for two minutes. The overlay does not take the focus while it listens, so Hold release on Win + Space stays reliable.
* Pronto ignores the search shortcut during dictation or a meeting recording. A tray toast tells the user why.
* The answer paints first. Images load after it. Wikipedia, OpenGraph and DuckDuckGo image lookups run at the same time.
* The question text and the result snippets go to DuckDuckGo and to the AI provider. Audio stays on the device.
* Links open in the default browser through the backend (`ShellExecute`). The WebView never goes to a result page. Pronto changes YouTube links to `youtube-nocookie` embeds.
* `design://system/v1` is a local design-system catalog in `src-tauri/resources/design-system.json`. The provider builds the answer UI from its allowlisted nodes. There is no remote MCP server.

## Note Taker

* Pronto records the microphone and the Windows system audio (loopback) to the disk.
* **Meeting detection** reads the Windows microphone consent store (`CapabilityAccessManager\ConsentStore\microphone`). Pronto suggests a recording only when a known meeting app uses the microphone for approximately 10 seconds. Window titles alone do not start a suggestion.
* **Stop** returns in approximately one second, also for a long meeting. Pronto marks the meeting as processing. Mixing and transcription continue in the background, with progress for each chunk.
* After transcription, the AI provider writes structured notes. If there is no provider, Pronto makes local notes.
* Imported audio and video files use the same path. Pronto extracts the audio track locally.

## Pronto Setup

`installer/` is a separate Tauri app. Its UI is `ui/setup.html`, with the same tokens and fonts as Pronto.

1. Setup checks for WebView2 before it opens a window. If WebView2 is missing, Setup installs it when the user agrees.
2. Setup finds the display adapters through DXGI. With an NVIDIA GPU, it recommends Parakeet. With all other GPUs, it recommends Phonon.
3. If Pronto is already installed, the user selects **Update** or **Reinstall**. Reinstall removes the app and engines. An option also erases history and settings.
4. Setup downloads the selected packs. This is the only slow step, and the user can cancel it.
5. Setup closes Pronto, writes the app files from its embedded zip, and saves `asrModel` in `settings.json`.
6. Setup makes Start menu and desktop shortcuts, and registers `HKCU\…\Uninstall\Pronto` with `uninstall.exe` (a copy of Setup).

The uninstaller runs from a temporary copy, so it can remove its own folder. It can keep history and settings.

## First-run tour

On the first start, the main window shows a short tour: welcome, shortcut and hold/toggle mode, microphone with startup and sound settings, a try-it box, and a summary. Each step has **Skip setup** and Escape.

The tour uses the same commands as Settings, so the two always agree. Finish or skip calls `complete_onboarding`, which saves `onboardingCompleted` in `settings.json`. When a user upgrades with saved history, Pronto marks the tour as done. **Settings → General → Show tour** shows it again.

## Storage and privacy

* Audio stays in memory. Pronto sends it only to the local loopback speech server.
* The AI provider gets transcript text only when the user turns on AI cleanup, meeting notes or search answers.
* API keys are in Windows Credential Manager.
* Settings and the last 100 history entries are in `%LOCALAPPDATA%\Pronto`.

## Windows lifecycle

Tauri owns one opaque main WebView, one opaque dictation overlay that does not activate, and one transparent always-on-top search overlay (skip-taskbar). All windows have no decorations and no shadows. Their web content fills the native window bounds exactly.

* The dictation overlay uses a native rounded window region, so the pill has no larger transparent parent.
* The search overlay starts as a bottom pill. It expands to a full-monitor transparent stage for the results, and hides when idle.
* Pronto registers application state on the Tauri builder before it makes the WebViews. Thus early IPC or shortcut events cannot race setup.
* After sleep, Pronto reloads a quiet overlay page, releases a stuck dictation, restores ducked audio, and prepares the microphone again if Windows reset the device.

Pronto is a Windows GUI-subsystem executable in all profiles. Pronto starts the engine servers with `CREATE_NO_WINDOW`, so no console window opens.

## Platform separation

Windows and macOS share one repository.

* **Shared:** Tauri commands and events, the Rust pipeline, the static `ui/` frontend, settings, history, dictionary, the search schema and the meeting detector core.
* **Windows (`Pronto`):** Pronto Setup (`installer/`), `src-tauri/tauri.conf.json`, the `windows` crate, the WH_KEYBOARD_LL hook, WASAPI/WinMM through CPAL, `SendInput` insertion, Core Audio ducking, the tray and `%LOCALAPPDATA%` storage.
* **macOS (`Pronto for Mac`):** `src-tauri/tauri.macos.conf.json` (merged by `scripts/build-macos.sh`), `src-tauri/src/platform/macos/`, `platform_paths.rs` and `model_provision.rs`, and the Metal runtime in `Contents/Resources/runtime/nemo-speech/`.
* **Cargo:** `windows` is a Windows-only dependency. macOS dependencies (`objc2-*`, `core-graphics`, `tauri-plugin-autostart`, `tauri-plugin-single-instance`, Metal support) are under `[target.'cfg(target_os = "macos")'.dependencies]`. Use a trait or a module, not `cfg` in business logic.
* **CI:** `.github/workflows/desktop-ci.yml` builds both platforms (`macos-15` ARM64 and `windows-2022` x64).

## macOS platform path (preview)

macOS uses the same Tauri commands, events, Rust pipeline and UI. Platform modules replace the Windows parts:

| Windows | macOS |
|---|---|
| WH_KEYBOARD_LL hook | Registered hotkeys for key chords; a listen-only `CGEventTap` for modifier-only chords |
| `SendInput` insertion | Accessibility insertion, with a clipboard fallback |
| WASAPI/WinMM | CPAL and CoreAudio |
| WASAPI loopback | In-process ScreenCaptureKit audio stream |
| `ShellExecute` | `NSWorkspace` |
| Startup registry entry | LaunchAgent |

The Tauri single-instance plugin brings the running app forward.

The Apple Silicon Metal runtime is in `Contents/Resources/runtime/nemo-speech/`. Its libraries resolve relative to the executable. The Parakeet model is in Application Support, not in the app bundle. A download worker streams it over HTTPS to a `.part` file, with resume and cancel. The worker renames the file only after SHA-256 verification against `model.sha256`. The engine opens only a verified model. Pronto stops the loopback server on controller drop, normal exit, SIGTERM or SIGINT.

This path compiles and runs local ASR on Apple Silicon. Packaged tests for permissions, overlays, insertion, meetings and relocation are not complete. See `docs/macos-port-status.md` and `docs/macos-validation.md`.
