# Pronto

Pronto is a Windows push-to-talk dictation application. It processes audio locally using an NVIDIA GPU and inserts text into the active application.

## Core Features

* **Local Transcription:** Uses NVIDIA Parakeet TDT 0.6B v3 via CUDA. The model stays loaded in memory to reduce delay.
* **Audio and Video Import:** Transcribes the audio track from common media files locally, including MP3, WAV, M4A, MP4, MOV, and WebM.
* **Meeting Note Taker:** Records microphone and Windows system audio directly to disk, then transcribes the meeting and creates notes after recording stops. Local window-title detection can offer the recorder without a calendar connection or meeting bot.
* **Recording Library:** Organizes recorded meetings and imported audio into local folders with background processing status and focused transcript views.
* **Offline Operation:** Runs completely offline without an internet connection using local models.
* **Text Processing/Cleanup:** Clears corrections to make text clear. Handles automatic punctuation, automatic bullet points, and automatic formatting. Optional DeepSeek V4 integration provides advanced text rewrite.
* **Text Insertion:** Inserts Unicode text into the active target application.
* **Audio Ducking:** Reduces system audio level automatically when recording starts and restores it when recording stops.
* **System Integration:** Operates in the system tray. Can start automatically at Windows boot.
* **Security:** Stores the DeepSeek API key in Windows Credential Manager.

## Performance

On an NVIDIA RTX 4050 Laptop GPU, local transcription of an 11-second audio file takes 86 ms.

## Requirements

* Windows 10 or 11 (64-bit)
* NVIDIA GPU with current display driver
* Microphone
* Internet connection (required only for DeepSeek rewriting)

## Install

1. Build the application or locate the installer file at:
`src-tauri/target/release/bundle/nsis`
2. Run the installer or execute `pronto.exe` directly.
3. Open **Settings** in Pronto.
4. Optional: Enter a DeepSeek API key. Local transcription works without a key.

## Build

Run these commands in PowerShell to test and build:

```powershell
cd src-tauri
cmd.exe /d /s /c '"C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\Common7\Tools\VsDevCmd.bat" -arch=x64 && cargo test --offline'
cargo tauri build

```

*Note: Frontend files are static and embedded by Tauri. Do not run Node.js or npm.*

## Windows Behavior

* Pronto runs without a console window.
* Closing the main window keeps background dictation active in the system tray.
* The application keeps the microphone active to avoid device initialization delay.
* Third-party software licenses are available in `THIRD_PARTY_NOTICES.md`.
