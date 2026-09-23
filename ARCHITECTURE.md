# Architecture

## Platform separation (Windows + Mac in one repo)

* **Shared:** Tauri commands/events, Rust pipeline, static `ui/` frontend, settings/history/dictionary, search schema, meeting detector core.
* **Windows path (`Pronto`):** `src-tauri/tauri.conf.json` (`nsis` + `installer-hooks.nsh`), `windows` crate behind `[target.'cfg(windows)'.dependencies]`, WH_KEYBOARD_LL hook, WASAPI/WinMM via CPAL, Win32 SendInput insertion, Core Audio ducking, tray + `%LOCALAPPDATA%` storage.
* **macOS path (`Pronto for Mac`):** `src-tauri/tauri.macos.conf.json` (`.app`/`.dmg`, hardened runtime, entitlements) merged by `scripts/build-macos.sh`, `src-tauri/src/platform/macos/` (hotkeys, insertion, audio, permissions, power, startup, navigation), `platform_paths.rs` + `model_provision.rs` (Application Support model download), Metal runtime under `Contents/Resources/runtime/nemo-speech/`.
* **Cargo separation:** `windows` is Windows-only; macOS deps (`objc2-*`, `core-graphics`, `tauri-plugin-autostart`, `tauri-plugin-single-instance`, Metal support) live under `[target.'cfg(target_os = "macos")'.dependencies]`. No `cfg` scattering through business logic where a trait/module is clearer.
* **CI:** `.github/workflows/desktop-ci.yml` builds both (`macos-15` ARM64 + `windows-2022` x64), so a Mac DMG refresh never requires a local Mac for the Windows-side upload (`scripts/publish-macos-release.ps1` + `docs/upload-macos-release-from-windows.md`).

## macOS platform path (port in progress)

The same Tauri commands, events, Rust pipeline, and static UI are used on
macOS. Target-specific modules replace the Windows keyboard hook with registered
macOS hotkeys for key-based chords and a listen-only `CGEventTap` for modifier-only
chords, Win32 insertion with Accessibility and a clipboard
fallback, WASAPI/WinMM with CPAL and CoreAudio, and loopback capture with an
in-process ScreenCaptureKit audio stream. `NSWorkspace` opens external search
URLs and provides application metadata. A LaunchAgent handles optional login
startup, while the Tauri single-instance plugin activates the existing app.

The native Apple Silicon Metal runtime is bundled under
`Contents/Resources/runtime/nemo-speech/`; its libraries resolve relative to
the executable. The Parakeet model is verified against `model.sha256` and
stored in Application Support, outside the app bundle. A dedicated download
worker streams it over HTTPS to a `.part` file, supports resume and cancel,
and atomically renames it only after SHA-256 verification. The engine only
opens a verified model. Its loopback server is terminated on controller drop,
normal process exit, or SIGTERM/SIGINT. Audio remains local; only text reaches configured
remote cleanup/search services.

This path has compiled and run local ASR on Apple Silicon. Packaged permission,
overlay, insertion, meeting, and relocation testing remains before release.
See `docs/macos-port-status.md` and `docs/macos-validation.md`.

## Windows platform path

```text
Configurable global shortcuts
  dictation (default Ctrl+Alt+Space)
  paste last (default Win+Shift+V)
  voice search (default Win+Space / super+Space)
      |
      v
Rust/Tauri coordinator ---------------------> overlay + dashboard + search overlay
      |
      +--> native WH_KEYBOARD_LL listener
      |      hold/toggle state machine + persisted canonical shortcuts
      |      (dictation / paste / search are mutually exclusive chords)
      |
      +--> optional Core Audio endpoint duck / exact restore
      |
      +--> prewarmed CPAL/WASAPI capture
      |      downmix -> 16 kHz -> silence trim
      |
      +--> persistent NeMo-Speech.cpp CUDA server
      |      Parakeet TDT 0.6B v3 Q8 -> punctuated transcript
      |
      +--> dictation path:
      |      deterministic local cleanup + dictionary correction
      |      optional DeepSeek V4 Flash rewrite (thinking disabled)
      |      Win32 SendInput Unicode insertion + local history
      |
      +--> voice search path (no intent router; search hotkey only):
             local cleanup only (no DeepSeek rewrite, no history insert)
             DuckDuckGo HTML retrieval via SearchProvider trait
             DeepSeek V4 Flash JSON UI synthesis (design://system/v1)
             search overlay (pill loading → popped centered result) renders
             allowlisted nodes (uPlot charts locally); blur/click-away dismisses
```

## Voice search notes

- Default search chord is Win + Space. Windows also reserves that chord for
  keyboard layout switching. Pronto’s hook always calls `CallNextHookEx`, so the
  OS may switch layouts in parallel. The Fn key cannot be bound (no VK code).
- Search uses a dedicated transparent always-on-top overlay (same class as the
  dictation pill): listening and searching share one bottom pill (green bars
  while listening, settled sweep while working); the result panel then pops
  in centered. The overlay is
  hidden when idle. Click-away / focus loss / Escape dismisses it (listening
  stays unfocused so Hold release on Win+Space remains reliable).
- Search is ignored while dictation is listening/processing or a meeting is
  recording; a tray toast explains why.
- Spoken queries and retrieved snippets are sent to the configured search
  provider (DuckDuckGo HTML by default) and to DeepSeek for UI synthesis.
  Microphone audio stays on-device.
- Result URLs open through the backend with ShellExecute into the default
  browser. The WebView never navigates to search results. YouTube links are
  rewritten to youtube-nocookie embeds.

## Why Parakeet

Parakeet TDT 0.6B v3 is a better fit than Whisper for this RTX 4050 laptop: it is
small enough to remain resident, is designed for very high transcription
throughput, returns punctuation and capitalization, and supports 25 European
languages. Pronto uses NVIDIA's native Windows CUDA runtime rather than a Python
service. A warmed local request transcribed the 11-second validation sample in
74 ms (86 ms through the local pipeline); a cold process completed it in about 892 ms.

Parakeet does not expose word boosting in this runtime, so dictionary behavior is
implemented explicitly after ASR and reinforced in the optional cleanup prompt.
That makes corrections predictable and independent of cloud availability.

## Latency design

1. The model process is started once and kept warm.
2. The microphone device is opened and negotiated once during app startup;
   hotkey activation only clears a buffer and resumes the prepared stream.
3. Audio stays in Rust; JavaScript receives only status and result events.
4. Capture callbacks only collect samples. Resampling and inference run off the
   audio callback thread.
5. One HTTP client is reused for DeepSeek requests and reasoning is disabled.
6. Local cleanup is always available; cloud cleanup is optional and separately
   timed.
7. Every completed dictation reports capture, ASR, cleanup, and total timings.

The sub-one-second target is realistic for short and medium phrases after warm-up
when local cleanup is used. DeepSeek cleanup is network-bound, so it is measured
but cannot be guaranteed below one second on every connection.

## Storage and privacy

- Audio is kept in memory and sent only to the local loopback transcription server.
- The DeepSeek API receives transcript text only when cloud cleanup is enabled.
- The API key is stored by Windows Credential Manager.
- Non-secret settings and the last 100 history entries live under
  `%LOCALAPPDATA%\Pronto`.

## Windows lifecycle

Tauri owns one opaque main WebView, one opaque non-activating dictation
overlay, and one transparent always-on-top search overlay (skip-taskbar). All
are undecorated and shadowless, and their web content fills the exact native
window bounds. The dictation overlay uses a native rounded window region, so
the compact pill has no larger transparent parent. The search overlay starts as
a bottom pill, expands to a full-monitor transparent stage for orb flight and
results, and hides when idle. Application state is registered on the Tauri
builder before either WebView is created, preventing early IPC or global-hotkey
events from racing setup.

Pronto is compiled as a Windows GUI-subsystem executable in every profile. The
bundled NeMo Speech console executable is spawned with `CREATE_NO_WINDOW`, so
neither development nor packaged launches create a console host.
