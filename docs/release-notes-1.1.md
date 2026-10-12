# Pronto 1.1

Pronto 1.1 makes Phonon ready sooner, pastes into the text box you actually chose, and tidies up a few screens.

## Faster

- **Phonon is ready about 12 seconds sooner every time it starts.** It now prepares its speech model on all of your processor's cores instead of one, and no longer starts a slow system query just to count them. On an 8-core laptop the model load dropped from about 42 s to 30 s, with identical transcripts.
- **No compile step on Phonon's first start.** The Phonon runtime now ships ready to run, so the first start after installing or updating skips compiling it. Updating with Pronto Setup downloads the new runtime once.

## Changed

### Pasting goes to the text box you chose
- **The last text box you selected gets the paste.** Clicking into a box selects it, and so does having the cursor in a box when you start recording.
- **Clicking a different text box while recording takes over.** Clicks on anything that isn't a text box don't change the target.
- **Pronto brings that window back before pasting,** un-minimising it if needed. If the cursor has left the text box, Pronto puts it back there first.
- **Win+Shift+V follows the same rule.**

### Smaller UI changes
- **Transcribe a file from Recent transcripts.** The large file card on Dictate is now a compact **Transcribe file** button beside *Recent transcripts*. It fills up to show progress while the file is prepared.
- **Saved API keys stay out of the way.** Settings → Online services shows **Key saved** once a key is stored, with an edit button to change it.

## Fixed

- **Show tour starts from Dictate.** Settings → General → Show tour now switches to the Dictate page first, then starts the tour there.

## Requirements

Unchanged from 1.0: Windows 10 or 11 (64-bit), a microphone, and either an NVIDIA GPU (Parakeet) or any CPU with AVX2 (Phonon). An AI provider API key is optional.

## Download

`Pronto_Setup_1.1.0_x64.exe` (SIZE)
SHA-256: `PENDING`
