# macOS port status

The [phase plan](macos-port-plan.md) tracks the full product and release gates.
The [acceptance record](macos-validation.md) lists observed checks and exact
interactive tests still required.

This is engineering work in progress. The current `main` release remains Windows
only. Do not distribute a macOS bundle from this checkout as a working Pronto
release.

## Implemented foundations

- Tauri macOS bundle metadata targets macOS 13+ on Apple Silicon, with microphone
  and screen recording privacy descriptions, hardened runtime metadata, and
  generated `.icns` / `.png` icons.
- `scripts/prepare-macos-assets.sh` downloads the pinned NeMo-Speech.cpp 0.1.0
  Apple Silicon Metal archive, verifies its SHA-256, and stages its executable,
  loader-relative dynamic libraries, and license files for bundling. The model
  is separate from the bundle.
- User data, logs, and recordings use `~/Library/Application Support` and
  `~/Library/Logs` on macOS. Windows storage paths remain intact.
- The engine selects Metal on macOS and resolves the bundled native executable.
  Search URLs use `NSWorkspace` to open the default browser.
- macOS enables Tauri's transparency feature so the dictation and search
  windows render without an opaque white WebView rectangle. The redundant
  Start Dictation card has been removed from the main window; the shortcut
  and menu-bar action remain available.
- Apple unified-memory availability is read through public Darwin process,
  Mach host-statistics, and sysctl APIs for the existing sustained-pressure model-unload policy;
  the macOS UI uses memory wording rather than CUDA/VRAM wording.
- CPAL now captures the microphone and plays cues on macOS. Registered macOS
  hotkeys handle key-based shortcuts without Input Monitoring; CGEventTap handles
  modifier-only chords when Input Monitoring is granted. CoreAudio controls optional output ducking, and the
  official Tauri autostart plugin registers a silent login launch.
- Accessibility can insert into a focused text field, with a temporary
  pasteboard attempt when the target rejects direct insertion. The prior
  pasteboard contents are restored after the attempt; failed or unverifiable
  insertion keeps the transcript in History and leaves the clipboard unchanged.
  The Mac target tracker now retains the
  selected editable Accessibility element at dictation start, updates it when
  another editable field is clicked, and restores its focus before insertion;
  this change still needs a packaged app test. CoreGraphics window titles feed the shared
  meeting detector, and search placement uses the visible macOS screen frame.
- ScreenCaptureKit computer-audio capture is implemented through an in-process
  Objective-C bridge and writes only audio samples. A wake notification triggers
  the existing dictation/overlay recovery routine. The single-instance plugin
  shows the existing window on a second launch.
- Meeting Stop now returns after capture is halted and the record is marked
  processing; audio-writer joins and ScreenCaptureKit shutdown run on the
  finalization worker. A crash during this stage is recovered on next launch,
  including repair of an incomplete WAV header. The computer-audio warning is
  retained after successful microphone transcription and notes generation.
- The macOS model installer checks free space, resumes a partial HTTPS download,
  verifies the pinned size and SHA-256, atomically installs the model, and exposes
  download progress, cancel, and retry in Settings. The engine only warms a
  verified model. A full-size `.part` left by a crash is now verified locally
  and installed, or removed and downloaded again if corrupt. The downloader
  now uses a per-read stall timeout and an interruptible request, so a slow
  ongoing transfer is not cut off by a two-minute total deadline. Settings also
  shows native permission status and recovery links.
- Settings now shows an explicit microphone enumeration failure with Retry and
  makes one automatic retry after a transient timeout; it no longer leaves the
  selector on "Loading…" indefinitely.
- Accessibility onboarding retains the macOS prompt at launch. The app no
  longer requests Accessibility on every dictation start; its Settings action
  opens the relevant pane and checks the grant while that pane is open. If a
  rebuilt ad hoc app has a stale permission entry, Settings explains how to
  remove and re-add the installed app, then restart Pronto.
  A denied Input Monitoring grant for a modifier-only shortcut no longer
  unregisters independent key-based shortcuts.
- A macOS Tauri navigation guard keeps the three app WebViews on local pages,
  with a narrow exception for the schema's YouTube no-cookie embed. Search
  result buttons still hand links to the default browser. The installed app's
  launch log confirms all three local pages loaded under this guard.
- Failed settings and history writes now leave their in-memory values unchanged.
  Removing an API key also removes any migrated legacy Keychain copy. The
  dictation result reports an insertion failure to the main window when
  automatic insertion cannot be verified; the transcript remains in History
  and the clipboard is restored. The overlay retains the Windows source's limited notices for
  microphone activation, model status, and meeting actions.
- Note Taker imports now fail visibly if their retry audio cannot be saved.
  The saved WAV is written to a temporary file and renamed only after a
  successful disk write, preserving an existing recording on failure.
- Delayed dictation overlay hides now carry a generation check, so finishing
  or cancelling one dictation cannot hide a newer dictation or meeting prompt.
  A microphone-start failure also clears the active-dictation flag.
- Meeting transcription now reads each two-minute audio chunk on demand in
  three bounded workers instead of retaining the full recording in memory.
  Truncated or missing chunks fail explicitly, and progress/order behavior is
  preserved. This still needs a packaged hour-scale run.

## Build prerequisites and current result

On an Apple Silicon Mac with Xcode Command Line Tools, Rust, and network access:

```sh
scripts/prepare-macos-assets.sh
cd src-tauri
cargo check --target aarch64-apple-darwin
```

The Apple Silicon `cargo check` passes. The Rust suite passed 72 tests with 7
hardware-dependent tests ignored after the meeting Stop change.
The published 713,975,456-byte Parakeet model
was downloaded on this Mac and independently matched `model.sha256`.
An ad hoc signed local `.app` and `.dmg` now exist at
`src-tauri/target/aarch64-apple-darwin/release/bundle/macos/Pronto.app` and
`src-tauri/target/aarch64-apple-darwin/release/bundle/dmg/Pronto_0.8.2_aarch64.dmg`.
`codesign --verify --deep --strict` and `hdiutil verify` pass. The runtime in a
relocated copy detects the Apple M2 Metal accelerator with no runtime warning.
The latest local package was built with official Rust 1.98.1 and declares a
macOS 13.0 minimum. The installed app currently reports Accessibility denied
despite an enabled Pronto row in System Settings. The ad hoc designated
requirement is a code hash that changes across builds; the stale entry must be
reset and the exact final app granted before insertion testing.
Dictation started from Pronto's own window remembers the last external
foreground app as its insertion target. Future ad hoc rebuilds may need a
fresh Accessibility grant after replacement.
The bundled app started its local server and transcribed a 2.83-second
synthetic phrase accurately in 0.47 seconds through the loopback API. A live
5.237-second microphone recording reached History in 585 ms and left its text
on the clipboard; Accessibility was denied, so automatic insertion was not
verified. The dictation overlay was confirmed on screen through the native
window list. A SIGTERM test confirmed the current speech server exits with
Pronto. A fake local speech-server test covers multipart WAV upload and
transcript parsing. These results are not full product verification. Interactive permission, microphone,
insertion, meeting, and overlay tests are still needed. This local build is
neither Developer ID signed nor notarized.

## Required implementation before release

- Accessibility insertion into browser/rich-text controls, Unicode and
  multiline verification, clipboard preservation, and permission recovery;
- packaged-app ScreenCaptureKit meeting capture and sleep/wake validation,
  plus non-activating overlay windows;
- packaged-app model installation, cancellation, and warmup validation;
- platform wording and keycaps throughout the shared UI;
- automated contract/integration tests and packaged app hardware validation.

The model specification remains `src-tauri/model.sha256`. No Mac performance
measurements or release claims have been made.
