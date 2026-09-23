# Pronto 0.8.2 macOS port plan

The Windows 0.8.2 application, its release notes, and the shared UI are the
parity baseline. A phase passes only when its behavior works in a relocated,
packaged Apple Silicon app. Source changes and unit tests alone do not close a
phase.

## Phase 0: baseline and contracts

- [x] Check out the 0.8.2 source and identify native Windows dependencies.
- [x] Pin and verify the published Apple Silicon Metal NeMo-Speech runtime.
- [ ] Record the Tauri commands, events, payloads, and all 0.8.2 regressions in
  a parity matrix before changing those contracts.
- [x] Add Windows build/test CI alongside Apple Silicon macOS CI (workflow added;
  first remote run still pending).

Exit evidence: contract inventory, CI matrix, and baseline test results.

## Phase 1: native platform core

- [x] Separate Windows-only Cargo dependencies and standardize macOS data paths.
- [x] Move the first native hooks behind platform modules with shared coordination.
- [ ] Validate the macOS CGEventTap shortcut listener, including modifier-only
  chords, key-up, tap recovery, permissions, and sleep/wake behavior.
- [ ] Implement AXUIElement focus tracking and Unicode insertion, with verified
  clipboard fallback and restoration.
- [ ] Validate cue playback, CoreAudio microphone capture, exact duck/restore,
  and launch at login; implement single-instance activation and child cleanup.
- [x] Make `cargo check --target aarch64-apple-darwin` pass.
- [x] Run the full macOS Rust test suite (72 passed, 7 hardware-dependent ignored).
- [x] Make macOS Clippy pass with `--all-targets -- -D warnings`.

Exit evidence: buildable app and native integration tests with actual external
text fields and keyboard events.

## Phase 2: local speech and first-use model

- [x] Stage the pinned Metal runtime and keep the model outside the app bundle.
- [x] Implement HTTPS first-use model provisioning with visible progress,
  cancellation, resume/retry, disk-space feedback, SHA-256 verification, and
  atomic installation.
- [ ] Verify warm-up, loopback health, timeouts, crash reporting, and process
  teardown. Replace CUDA/VRAM language and policies with honest macOS behavior.
- [ ] Test short dictation, file import, long transcription, and cancellation
  with fixtures and the real model.

Exit evidence: first launch on a clean Mac downloads the model once, rejects a
corrupt model, and reaches ready only after Metal warm-up.

## Phase 3: meetings and overlays

- [ ] Validate microphone plus ScreenCaptureKit system-audio recording,
  permission recovery, prompt stop, background mix, and hour-scale processing.
- [ ] Port meeting detection and app icons while preserving generations,
  suppression, stability polls, and cooldown.
- [ ] Configure a non-activating dictation NSPanel and the search stage/peek on
  the active display's visible frame. Preserve watchdog and race guards.

Exit evidence: a two-hour meeting capture and multi-display overlay tests in a
packaged app, including denial, revocation, and sleep/wake cases.

## Phase 4: frontend parity and privacy

- [ ] Keep the shared five-section UI and replace Windows text and keycaps with
  platform-specific copy. Verify themes, zoom state, reduced motion, and focus.
- [ ] Verify Note Taker import/library, history, dictionary, cleanup, settings,
  voice search, browser handoff, and schema/URL security.
- [ ] Audit Keychain storage, network destinations, audio locality, and logs.

Exit evidence: feature-by-feature UX comparison against Windows 0.8.2 with
screenshots, interaction recordings, and regression tests.

## Phase 5: release

- [x] Add `.icns`, macOS bundle metadata, usage descriptions, and entitlements.
- [x] Build and verify an ad hoc signed local `.app` and `.dmg`, including nested runtime.
- [ ] Developer ID sign, notarize, and staple from environment-provided credentials.
- [ ] Install the DMG on a clean macOS 13+ Apple Silicon Mac from a relocated
  Applications folder. Run the complete hardware/permission matrix.
- [ ] Measure activation latency, warm ASR, end-to-end short dictation, idle and
  warm memory, and long-meeting stop latency; investigate regressions.
- [ ] Update installation, architecture, privacy, notices, and release notes
  with observed results and genuine limitations.

Exit evidence: the DMG, the installed app, passing automated checks, observed
hardware results, and a requirement-by-requirement completion audit.
