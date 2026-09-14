# Voice search — manual QA

Dedicated Pronto voice search is triggered only by the global search hotkey (default **Win + Space**, canonical `super+Space`). It does not use the dictation intent path, does not insert text, and does not write dictation history. Activation follows Settings Hold / Toggle exactly like dictation.

## Checklist

### Hold / toggle
1. Settings → Activation = **Hold**. Press and hold the search shortcut, speak, release — listening should end on release (not after several seconds).
2. Settings → Activation = **Toggle**. Press once to start, press again to finish. A second press within ~300ms of start is ignored (Win+Space bounce).
3. While listening, **Search now** forces finish; **Cancel** aborts.

### Stuck listening / latency
1. Confirm the search window appears without stealing keyboard focus during listen.
2. If a key-up is missed, listening auto-finishes within **8 seconds**.
3. After stop: status should move Transcribing → Searching web → Writing answer. The query appears as soon as ASR completes; sources appear before the final LLM UI.

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
1. Confirm Settings and the Search empty state note that query text and snippets go to DuckDuckGo and DeepSeek.
2. Confirm microphone audio stays local (Parakeet) and is not uploaded for search synthesis.

### Charts
1. When the model returns a `chart` node, confirm uPlot renders from the vendored `ui/vendor/uPlot.iife.min.js` (no CDN).
2. If chart data is invalid, confirm the renderer falls back to a table.
