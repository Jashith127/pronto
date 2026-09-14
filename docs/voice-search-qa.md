# Voice search — manual QA

Dedicated Pronto voice search is triggered only by the global search hotkey (default **Win + Space**, canonical `super+Space`). It does not use the dictation intent path, does not insert text, and does not write dictation history. Activation follows Settings Hold / Toggle exactly like dictation.

Search UI is a **transparent always-on-top overlay** (same class as the dictation pill), not a normal Pronto window. It is hidden when idle.

## Checklist

### Overlay chrome
1. While listening, confirm a bottom-centered **pill** appears (dark, slightly rounded) with a **green** waveform and cancel / finish controls — **no** meeting-notes button on the right.
2. Confirm the overlay does **not** show in the taskbar and does not look like a separate Pronto app window.
3. After release / Search now, the pill should **liquid-morph into a circular orb** with the searching animation arranged on a ring (not a straight bar row).
4. When results arrive, the orb should **travel to the center of the screen** and expand into the dark result panel.
5. Click the dimmed backdrop, click outside / move focus away while the overlay is focused, press **Escape**, or use the panel × — the overlay should dismiss and stay gone until the next search.
6. Confirm the overlay is **not** present at rest (before any search).

### Hold / toggle
1. Settings → Activation = **Hold**. Press and hold the search shortcut, speak, release — listening should end on release (not after several seconds).
2. Settings → Activation = **Toggle**. Press once to start, press again to finish. A second press within ~300ms of start is ignored (Win+Space bounce).
3. While listening, **finish (✓)** forces finish; **cancel (×)** aborts and hides the overlay.

### Stuck listening / latency
1. Confirm the search overlay appears **without stealing keyboard focus** during listen.
2. If a key-up is missed, listening auto-finishes within **8 seconds**.
3. After stop: status should move Transcribing → Searching web → Writing answer. The query appears as soon as ASR completes; sources appear before the final LLM UI (after the orb-to-center transition).

### Win + Space layout-switch warning
1. With the default `super+Space` shortcut, press the chord.
2. Confirm Pronto starts voice search **and** that Windows may still switch keyboard layouts (WH_KEYBOARD_LL calls `CallNextHookEx`).
3. Immediate synthetic key-ups from layout switching should not strand Pronto in Listening.
4. Remap voice search in Settings if layout switching is disruptive. The Fn key cannot be bound (no VK code).

### Mutual exclusion
1. Start dictation; press the search hotkey — expect a toast and no search recording.
2. Start a meeting recording; press search — expect a toast and no search recording.
3. While search is listening, dictation should refuse to start.

### Offline / provider errors
1. Disconnect the network (or point Search provider URL at an unreachable host).
2. Run a voice search — expect a clear search-error / Error status without crashing.
3. Restore network; confirm a normal search succeeds.

### Browser click-through
1. Complete a search that includes a `source_list` or `button` with `open_url`.
2. Click a result — it must open in the **default system browser**, not navigate the Pronto WebView.
3. Invented / non-result URLs must be rejected by the backend.

### Privacy
1. Confirm Settings and the Search empty / privacy copy note that query text and snippets go to DuckDuckGo and DeepSeek.
2. Confirm microphone audio stays local (Parakeet) and is not uploaded for search synthesis.

### Charts
1. When the model returns a `chart` node, confirm uPlot renders from the vendored `ui/vendor/uPlot.iife.min.js` (no CDN).
2. If chart data is invalid, confirm the renderer falls back to a table.
