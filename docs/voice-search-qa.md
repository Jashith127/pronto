# Voice search — manual QA

Dedicated Pronto voice search is triggered only by the global search hotkey (default **Win + Space**, canonical `super+Space`). It does not use the dictation intent path, does not insert text, and does not write dictation history.

## Checklist

### Hold / toggle
1. In Settings, set activation to **Hold**. Press and hold the search shortcut, speak a short query, release.
2. Confirm the Search window opens, status moves Listening → Searching → Complete, and an answer (or fallback source list) appears.
3. Switch activation to **Toggle**. Press once to start listening, press again to finish. Cancel mid-listen with the Cancel button.

### Win + Space layout-switch warning
1. With the default `super+Space` shortcut, press the chord.
2. Confirm Pronto starts voice search **and** that Windows may still switch keyboard layouts (WH_KEYBOARD_LL calls `CallNextHookEx`).
3. Remap voice search to another chord in Settings if layout switching is disruptive.
4. Confirm the Fn key cannot be captured as a shortcut (no virtual-key code).

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
1. Confirm Settings and the Search empty state note that query text and snippets go to DuckDuckGo and DeepSeek.
2. Confirm microphone audio stays local (Parakeet) and is not uploaded for search synthesis.

### Charts
1. When the model returns a `chart` node, confirm uPlot renders from the vendored `ui/vendor/uPlot.iife.min.js` (no CDN).
2. If chart data is invalid, confirm the renderer falls back to a table.
