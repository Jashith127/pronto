# Pronto

Pronto is a Windows-first push-to-talk dictation app. It keeps NVIDIA Parakeet
loaded on the local GPU, optionally cleans the transcript with DeepSeek, applies
a personal dictionary, and types the result into the app you were using.

## Core features

- Choose hold-to-talk or toggle activation (press once to start, again to finish)
- Native Windows-key combinations, including modifier-only `Win+Ctrl`
- Local NVIDIA Parakeet TDT 0.6B v3 transcription through NeMo-Speech.cpp/CUDA
- Persistent model server for low latency (no model reload between dictations)
- Local filler/repetition cleanup plus optional DeepSeek V4 Flash rewriting
- Personal dictionary with deterministic correction before insertion
- Unicode text insertion into the previously focused Windows application
- Secure DeepSeek key storage in Windows Credential Manager
- History, configurable auto-insert, tray operation, and a tiny cancel/waveform/finish pill
- Tray-level **Paste Last Transcript** with automatic paste and clipboard fallback
- Optional reversible system-audio ducking while the microphone is active
- Optional launch-at-startup mode that starts silently in the system tray
- Opaque, shadowless native window bounds with no transparent outer container

On the target RTX 4050 Laptop GPU, the warmed engine transcribed the included
11-second validation clip in 74 ms (86 ms for the local pipeline). End-to-end time also depends
on phrase length, DeepSeek/network latency, and the target application.

## Install

Run the installer from `src-tauri/target/release/bundle/nsis` after building, or
launch `src-tauri/target/release/pronto.exe` directly from this project directory.
The installer includes the CUDA transcription runtime and Parakeet model.

Open Settings in Pronto to save a DeepSeek API key. The key is never written to the
settings JSON file. Cleanup remains usable without a key through the local cleanup
pipeline.

## Build and test

```powershell
cd src-tauri
cmd.exe /d /s /c '"C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\Common7\Tools\VsDevCmd.bat" -arch=x64 && cargo test --offline'
cargo tauri build
```

No Node/npm step is required; the frontend is static and embedded by Tauri.

## Windows behavior

- Pronto and its Parakeet child process are launched without console windows.
- Closing the main window keeps global dictation and tray actions available.
- Startup launch uses a background flag and never shows or flashes the main window.
- A narrowly scoped low-level keyboard listener supports modifier-only chords,
  rejects unsafe/reserved combinations, and passes unrelated input through.
- The microphone is opened and format-negotiated at startup. Each activation
  resumes the prewarmed stream instead of reopening the device.
- Audio ducking snapshots the playback endpoint's volume and mute state and
  restores both when recording stops, fails, resets, or Pronto exits normally.

## Requirements

- Windows 10/11 x64
- NVIDIA GPU with a compatible current driver (optimized for the RTX 4050 6 GB)
- Microphone permission for desktop apps
- Internet access only for optional DeepSeek cleanup

Third-party licenses and model attribution are recorded in
`THIRD_PARTY_NOTICES.md`.
