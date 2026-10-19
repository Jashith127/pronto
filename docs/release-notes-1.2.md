# Pronto 1.2

Pronto 1.2 gives the tray icon a menu of its own, lets you pick what the dictation pill shows, and fixes dictation pulling your browser to the front.

## Added

### A tray menu of Pronto's own

- **Click the tray icon for a menu in Pronto's style,** the same look as the rest of the app instead of the standard Windows menu.
- **Recent transcripts are in the menu.** Open **Transcript history** and pick a recent dictation to paste it straight into whatever you are typing.
- **Choose how many transcripts it lists.** Settings → General sets it to **3**, **5** or **10**.
- **Free up memory from the menu.** The speech model row offers to **offload the model** when it is loaded, or to load it again when it is not, so the row always shows what is actually happening right now.

The menu opens under the cursor, flips itself back onto the screen when the tray icon is in a corner, and reads its labels when it opens, so a row never shows a state Pronto has already left.

### Choose what the dictation pill shows

- **Settings → Overlay pill.** Switch off the search button, the waveform, the note taker button, or the ✕ and ✓ inside the pill, and keep only what you use. A preview shows the result as you change it.
- **The ✕ and ✓ follow your shortcut mode.** They only appear in Toggle mode, where you need them to stop or discard. With Hold, letting go of the shortcut already finishes, so they are hidden and explained in the settings.

Everything is on by default, so upgrading changes nothing until you switch something off.

## Fixed

- **Dictation no longer pulls another app to the front.** In 1.1, starting to dictate could switch your focus to a browser window you had used earlier, instead of the game or app you were in. Pronto now only pastes into a text box you selected in the window you are actually in, as it did in 1.0.

  If you click a text box in one app, switch to another app, and then dictate, the text now goes to the app you are in.

- **Known limitation, unchanged from 1.0.** Dictation pastes with the clipboard and Ctrl+V. Some games in exclusive fullscreen, or with anti-cheat input filtering, do not accept that. The transcript is still on the clipboard and in History. **Borderless windowed mode is the most reliable.**

## Requirements

Unchanged from 1.1: Windows 10 or 11 (64-bit), a microphone, and either an NVIDIA GPU (Parakeet) or any CPU with AVX2 (Phonon). An AI provider API key is optional.

## Download

`Pronto_Setup_1.2.0_x64.exe` (SIZE_PLACEHOLDER MB)
SHA-256: `HASH_PLACEHOLDER`