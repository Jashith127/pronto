# Architecture

```text
Configurable global shortcut (including modifier-only Win + Ctrl)
      |
      v
Rust/Tauri coordinator ---------------------> overlay + dashboard
      |
      +--> native WH_KEYBOARD_LL listener
      |      hold/toggle state machine + persisted canonical shortcut
      |
      +--> optional Core Audio endpoint duck / exact restore
      |
      +--> prewarmed CPAL/WASAPI capture
      |      downmix -> 16 kHz -> silence trim
      |
      +--> persistent NeMo-Speech.cpp CUDA server
      |      Parakeet TDT 0.6B v3 Q8 -> punctuated transcript
      |
      +--> deterministic local cleanup + dictionary correction
      |
      +--> optional DeepSeek V4 Flash rewrite (thinking disabled)
      |
      +--> Win32 SendInput Unicode insertion into captured foreground window
      |
      +--> local history --> tray Paste Last Transcript
             CF_UNICODETEXT clipboard + Ctrl+V fallback semantics
```

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

Tauri owns one opaque main WebView and one opaque, non-activating recording
overlay. Both are undecorated and shadowless, and their web content fills the
exact native window bounds. The overlay uses a native rounded window region, so
the compact pill has no larger transparent parent. Application state is registered on the Tauri builder
before either WebView is created, preventing early IPC or global-hotkey events
from racing setup.

Pronto is compiled as a Windows GUI-subsystem executable in every profile. The
bundled NeMo Speech console executable is spawned with `CREATE_NO_WINDOW`, so
neither development nor packaged launches create a console host.
