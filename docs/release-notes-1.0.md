# Pronto 1.0

Pronto 1.0 is the first stable release. It adds a choice of AI providers, much faster dictation on Phonon and Parakeet, a welcome tour for new users, and a simpler way to reinstall.

## Added

- **Choose your AI provider.** Cleanup, meeting notes and voice search answers now work with DeepSeek (recommended), OpenAI, Anthropic, Google Gemini, Groq, OpenRouter, or any OpenAI-compatible endpoint such as Ollama or LM Studio. Each provider keeps its own API key, and your existing DeepSeek key carries over.
- **Pick a model from a list.** Settings → Online services loads the provider's available models, with the default first and **Other...** to type a name.
- **Welcome tour.** New users get a short, skippable tour: shortcut, microphone and startup, then an all-set screen that shows how to dictate in any app. Replay it from Settings → General → Show tour.
- **Update or Reinstall in Pronto Setup.** Running Setup on a PC that already has Pronto now offers to update in place or reinstall a clean copy, with an option to also erase history and settings. Silent installs accept `--reinstall` and `--fresh`.

## Faster

Dictation is faster on both engines, and Phonon is several times faster than in 0.9.

- **Phonon is about 4× faster.** Windows was throttling the CPU speech engine as a background process. Pronto now opts it out, so the median wait after you stop dropped from 3.25 s to 0.74 s, and the slowest dictations from 11.5 s to 3 s.
- **Parakeet finishes sooner.** Pronto transcribes what you have already said at each pause while you keep talking, so stopping leaves only the last few words. The median wait dropped from 0.66 s to 0.34 s, and long dictations improve the most (a 63-second clip: 0.94 s to 0.37 s).
- **AI cleanup no longer falls back after idle time.** Pronto opens the provider connection when dictation starts, so a slow first lookup no longer times out and drops to local cleanup.

<img alt="Time from end of recording to text on Phonon: median 3.25 s to 0.74 s, p95 11.48 s to 3.03 s, 5 s clip 2.27 s to 0.49 s, 23 s clip 9.15 s to 2.15 s" src="https://raw.githubusercontent.com/Jashith127/pronto/a71830b9078c6610d8b8b5e952969bea4cdce048/docs/images/latency-phonon.png" width="640">

<img alt="Time from releasing the shortcut to text on Parakeet: median 0.66 s to 0.34 s, p95 0.94 s to 0.50 s, 44 s clip 0.67 s to 0.23 s, 63 s clip 0.94 s to 0.37 s" src="https://raw.githubusercontent.com/Jashith127/pronto/a71830b9078c6610d8b8b5e952969bea4cdce048/docs/images/latency-parakeet.svg" width="640">

## Fixed

- **Meetings on Parakeet no longer fail mid-transcription.** Meeting chunks are now sent one at a time, which avoids stalls and dropped connections.
- **Window controls stay responsive** while a meeting retry, file re-import or reset is running.

## Requirements

Unchanged from 0.9: Windows 10 or 11 (64-bit), a microphone, and either an NVIDIA GPU (Parakeet) or any CPU with AVX2 (Phonon). An AI provider API key is optional.

## Download

`Pronto_Setup_1.0.0_x64.exe` (13.4 MB)
SHA-256: `d99e8844ef2b8a63a6a67275aa8706aa4000f3af264ea91f3f6934865e80460b`
