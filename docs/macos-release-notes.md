# Pronto for Mac 0.8.2 (Apple Silicon, macOS 13+)

MAC-only release. The asset below is the only file in this release; Windows installers stay on their own tags (e.g. `v0.8.2`).

Pronto for Mac 0.8.2 for Apple Silicon Macs running macOS 13 or newer.

This build includes local speech transcription, a transparent dictation overlay, automatic insertion into the selected text field, History, Paste Last, and a first-use model download. Automatic insertion preserves the previous clipboard contents. Speech cleanup is off by default for new installations and can be enabled in Settings.

The DMG is ad hoc signed and is not Apple notarized. macOS may require the person installing it to allow the app manually in System Settings. A Developer ID signed and notarized build is still required for normal public distribution without that warning.

## Requirements

- Apple Silicon Mac (M1/M2/M3/M4), macOS 13 Ventura or newer
- ~1 GB free for the app plus ~714 MB for the speech model on first launch
- Microphone
- Internet on first launch (model download) and only for DeepSeek features afterwards

## Install — step by step

1. Download `Pronto_0.8.2_aarch64.dmg` from this release (below).
2. Verify the download (optional but recommended):
   ```sh
   shasum -a 256 ~/Downloads/Pronto_0.8.2_aarch64.dmg
   # must print: ec8f2c190df1ce63af414ae1b1f4a450b13ed6c4253f609dc5a7e5435ed8cfb1
   ```
3. Open the DMG (double-click), drag `Pronto.app` to `Applications`.
4. Eject the DMG, then launch from `Applications` (first launch, not from the DMG).
5. Ad hoc signature Gatekeeper pass (only because this build is not notarized):
   - If macOS says the app is damaged or cannot be opened, open `System Settings > Privacy & Security`, allow `Pronto`, then launch again.
   - Or remove the quarantine flag once:
     ```sh
     xattr -dr com.apple.quarantine /Applications/Pronto.app
     ```
6. First-launch model download: Pronto fetches the Parakeet model to `~/Library/Application Support/app.pronto.dictation/models/` with progress in Settings. Keep the app open until warm status appears. Cancel/retry is in Settings if needed.
7. Grant permissions when prompted (Settings shows status + recovery links):
   - Microphone: needed for dictation and meeting notes.
   - Accessibility: needed for automatic insertion into the app you were using. Without it, transcripts stay in History and the clipboard is left unchanged.
   - Input Monitoring: only for modifier-only shortcut chords.
   - Screen Recording: only for computer-audio meeting capture (no video is saved).
8. Defaults: dictation `Control + Option + Space`, Paste Last `Control + Option + V`, voice search `Control + Option + S`. Test in TextEdit: focus a text field, hold the dictation shortcut, speak, release.
9. Optional: enter a DeepSeek API key in Settings for rewrite/cleanup and search synthesis. Local transcription works without a key.

## Verify it worked

- Settings shows model ready/warm, no permission errors.
- A short dictation appears in History and is inserted into the focused field (or held in History with a notice if Accessibility is off).
- The dictation pill is dark, always on top, and never steals focus.

## Troubleshooting

- "Cannot be opened / damaged": apply step 5 above, then relaunch from `/Applications/Pronto.app`.
- No insertion: grant Accessibility, restart Pronto, retest. Clipboard contents are preserved/restored by design.
- Modifier-only shortcut dead: grant Input Monitoring, re-register the shortcut in Settings.
- No computer audio: grant Screen Recording, restart Pronto, restart the meeting app if needed.
- Model stuck: Settings cancel/retry; check free space (>2 GB safe); relaunch resumes.

---

## Copy-paste to an AI (install guide prompt)

Copy everything between the lines into any AI to have it walk you through the install:

```
You are helping me install Pronto for Mac 0.8.2 (Apple Silicon, macOS 13+).
DMG SHA-256 must be ec8f2c190df1ce63af414ae1b1f4a450b13ed6c4253f609dc5a7e5435ed8cfb1.
Guide me step by step and confirm each step before moving on:
1. Confirm Apple Silicon (apple silicon mac, macOS 13+) and free space.
2. Download Pronto_0.8.2_aarch64.dmg from the v0.8.2-macos release, verify shasum -a 256.
3. Open the DMG, drag Pronto.app to /Applications, eject DMG, launch from /Applications.
4. If Gatekeeper blocks the ad hoc signed app, walk me through System Settings > Privacy & Security allow, or xattr -dr com.apple.quarantine /Applications/Pronto.app, then relaunch.
5. Keep the app open for the first-launch model download to ~/Library/Application Support/app.pronto.dictation/models/ until Settings shows warm/ready. Offer cancel/retry if stalled.
6. Grant Microphone, then Accessibility (required for auto-insert; without it transcripts stay in History), then Input Monitoring only for modifier-only chords, then Screen Recording only for meeting computer audio. Use Settings status and restart Pronto after grants.
7. Test: Control+Option+Space dictation into TextEdit, Control+Option+V paste last, Control+Option+S voice search. If insertion fails, keep History and check Accessibility.
8. Optional DeepSeek key. New installs start with Clean up speech off.
Ask me for the exact error text and macOS version at any failure, and give only the next fix, not the whole list.
```
