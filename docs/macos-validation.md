# macOS acceptance record

Target: Apple Silicon, macOS 13 or newer. This record distinguishes observed
results from required interactive tests. A passing compile is not a passing
feature test.

## Observed on Apple M2, macOS 26.5

| Check | Result |
| --- | --- |
| Model release download | 713,975,456 bytes; SHA-256 `e3880d0aaaaf2c308ea2c35016b2b895c423eb3fda924c1b463d1c19b7f4d32e` matches `model.sha256`. This was an independent download, not an in-app first-use test. |
| Model recovery and cancellation | Unit tests verify a full-size crash partial is accepted only after checksum verification, resumed HTTP byte ranges are checked, and Cancel interrupts a stalled transfer while preserving its partial file. An in-app 714 MB first-use run remains due. |
| Meeting Stop worker | A deterministic test kept both capture writers busy for 500 ms; Stop returned in under 250 ms and finalization waited for them afterward. This does not measure real ScreenCaptureKit or a long recording. |
| Meeting chunk memory | The transcription worker now seeks and reads one two-minute chunk at a time instead of loading all chunks before processing. A fixture test covers chunk boundaries, order, and truncation. No hour-scale memory measurement has been made. |
| Meeting recovery | A fixture marked `processing` with an invalid WAV header was recovered on startup to a mixed audio file and `interrupted` state. A separate test confirms that successful notes keep a computer-audio failure warning. Real capture and interruption remain untested. |
| Note Taker retry audio | A disk-write failure test confirms that saving a replacement WAV leaves the prior file intact; a successful retry atomically replaces it. File-import code now reports a persistence error instead of starting transcription with no saved retry audio. A packaged UI import remains untested. |
| Bundled runtime | `nemo-speech doctor --json` from a relocated `.app` reports `backend_metal=true`, `accelerator_available=true`, Apple M2, and no runtime warnings. |
| Runtime linkage | Inspected 27 bundled Mach-O runtime files with `otool -L`; none referenced `/opt`, `/usr/local`, or a user path. |
| Local ASR | A 2.83-second system-synthesized WAV returned “Pronto records a short test sentence on this Mac.” over `127.0.0.1` in 0.47 seconds after warm-up. |
| Live microphone dictation | On this Mac, the in-window control captured 5.237 seconds of microphone audio and saved a transcript to History in 585 ms after recording stopped. The text was also copied to the clipboard. |
| Overlay | The in-window control showed the dictation overlay; `CGWindowListCopyWindowInfo` confirmed its 480 × 72 window was on the 1470 × 956 display. Earlier physical-coordinate logs were compared against logical screen bounds and were misleading. |
| Overlay transparency | Tauri's macOS transparency feature is enabled in the rebuilt app. The user confirms the white rectangle around the dictation pill is gone. |
| Overlay stale hides | A unit test covers a completion/cancel hide scheduled for an older overlay generation, a new dictation, and a meeting prompt. The packaged UI race remains unobserved. |
| Process teardown | On the latest installed build, terminating Pronto with SIGTERM removed both its GUI process and the bundled speech child. Relaunch produced one new server with Pronto as its parent. |
| Registered hotkey bridge | `scripts/check-macos-hotkey-bridge.sh` registered a Control-based shortcut and dispatched synthetic Carbon press/release events to its handler; both callbacks arrived. A physical global shortcut test is still required. |
| Permissions | The installed app's own UI and dictation log report Accessibility denied, while System Settings shows a Pronto row switched on. A shell-launched `--diagnose` reported allowed, but that process can inherit Terminal's TCC attribution and is not evidence for the app. The ad hoc designated requirement changed from code hash `d55a3aee…` to `d0c88c21…` across saved builds. Reset and re-grant the exact final installed build, then test insertion. Input Monitoring and Screen Recording still need in-app checks. |
| Insertion result reporting | The user reports that automatic insertion now works in one external app; the installed build logged multiple `insertion=verified` completions. The user also reports that switching text fields while speaking appears to work, but the exact target app and fields were not recorded. A later attempt returned clipboard fallback in an earlier build. Native AX/paste return codes distinguish verified writes, attempted but unreadable writes, and no event sent. The user confirms that the latest installed build preserves previous clipboard content after dictation. |
| WebView navigation | The packaged app logged completed loads for `main`, `overlay`, and `search` with the macOS navigation guard active. A URL-policy test allows local pages and the exact YouTube no-cookie embed, and rejects other external/file/script URLs. External-navigation attempts have not been exercised in the packaged UI. |
| Package | Official Rust 1.98.1 built the ad hoc signed `.app` and DMG with `MACOSX_DEPLOYMENT_TARGET=13.0`. The app passes `codesign --verify --deep --strict`; the DMG passes `hdiutil verify`; `LC_BUILD_VERSION` reports macOS 13.0 minimum. The bundle identifier is `app.pronto.dictation`; model is absent from app resources. The latest installed executable is SHA-256 `bd08e30930d80971b1e7077a434079a2c26b2210a885e7a487e64ff0f63bf625`. |
| Source gates | `cargo fmt --check`, Apple Silicon `cargo check`, `cargo clippy --all-targets -- -D warnings`, and Rust tests passed after the native port changes. |

The server process RSS was about 688 MiB during the local ASR check. This is a
single point sample, not an idle/warm memory benchmark. Activation latency,
cold ASR, and long-meeting stop latency remain unmeasured.

## Interactive acceptance still required

Computer Use permission was unavailable to this task. Apart from the observed
live microphone and overlay run above, the following UI and permission tests
have **not** been observed. Run them against a copy of the
packaged app moved out of the build tree, with a test account and disposable
content. Record the macOS version, app hash, timing, and result for each row.

| Area | Reproducible test | Observed |
| --- | --- | --- |
| First-use model | Move an existing model aside, launch the app, watch progress in Settings, cancel mid-download, relaunch, retry, verify resume/checksum and final Metal warm status; repeat with a corrupt model and low free space. | Not run |
| Microphone | Grant, deny, and re-grant Microphone; select two inputs, disconnect one while listening, then record a short hold and toggle dictation. | Not run |
| Shortcuts | Test all three defaults, configure distinct replacements and modifier-only chords, reject conflicts, hold key down/up, rapid repeats, sleep/wake, and tap disable/re-enable. | Not run |
| Insertion | Dictate Unicode and multiline text into TextEdit, Safari, Chrome, Slack, and a code editor. Start in field A, click editable field B while speaking, then stop: B must receive the transcript and remain focused. Repeat after clicking noneditable content: A must remain the target. Move focus across apps, close the original target, and repeat Paste Last. Check that clipboard bytes/types are restored after verified and unverified paste and that failure leaves the transcript in History without changing the clipboard. | Ordinary insertion works, the user reports field switching appears to work, and the latest installed build preserved previous clipboard content. The full app and rich-text matrix remains untested. |
| Accessibility/Input Monitoring | Deny each permission, verify only affected behavior degrades, use the Settings recovery action, then grant and retest without a blank window or prompt loop. | Not run |
| Meeting audio | Start a call or playback fixture, record microphone and computer audio, verify both WAV tracks and mixed audio, deny/revoke Screen Recording, switch output device, sleep/wake, and confirm the microphone track survives. | Not run |
| Long meeting | Record for at least one hour, measure Stop latency, verify the UI stays responsive during mixing/transcription, and check partial/error recovery. | Not run |
| Detection | Open known meeting vendors and transient title blips; check stability polls, two-minute suppression, generations, icons, and no prompt during dictation/recording. | Not run |
| Overlays | Test dictation pill focus safety, dark pill, cancel/finish/search reroute, search results, click-away/park/restore/two-minute timeout, two displays and Dock positions, sleep recovery, themes, and reduced motion. | Not run |
| Lifecycle | Close main window, invoke menu-bar actions, launch a second instance, enable/disable login startup, and confirm silent background launch and clean child-process exit. | Not run |
| Notes and history | Import WAV/MP3/M4A/MP4/MOV/WebM, test 90-minute rejection and cancellation, process a recording, rename/delete/retry, compare local history limits and dictionary cleanup. | Not run |
| Privacy and security | Inspect Keychain entry, network destinations, invalid search UI/URL rejection, no remote audio upload, no external WebView navigation, and logs without secrets. | Not run |
| Clean install | Install the DMG on a separate clean macOS 13+ Apple Silicon Mac without Homebrew or a repository checkout; test first launch, relocation, permissions, and update behavior. | Not run |

Signing and notarization require a Developer ID identity and Apple notarization
credentials supplied outside the repository. The current DMG is ad hoc signed
and is a local development artifact.
