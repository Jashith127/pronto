# Phonon CPU latency benchmark

Use the Python executable shipped with the installed Pronto Phonon pack, not
an unrelated Python environment. Run on AC power with other heavy work idle.
Do not compile the app while taking timings.

```powershell
$python = "$env:LOCALAPPDATA\Pronto\runtimes\phonon-cpu\python\python.exe"
& $python -I scripts/benchmarks/fetch_librispeech.py
& $python -I scripts/benchmarks/phonon_cpu.py --repeats 5
```

The fetcher streams the official [LibriSpeech test-clean archive](https://www.openslr.org/12/)
(CC BY 4.0), selecting the shortest, median-duration, and longest utterance
from each of its first three speakers. `dist/phonon-benchmark/clips/manifest.json`
contains the original reference transcripts, durations, and SHA-256 hashes of
the PCM16 WAVs. Audio stays in the ignored `dist` directory. These are clean
read-speech samples, not an evaluation of noisy microphones or all accents.

The Python harness starts a fresh process and loads the model once for each
thread configuration, setting `FERMION_CPU_THREADS` before loading. This matches
production startup and avoids resizing active native pools. It does not change model
weights, quantization, decoding rules, or precision. Two complete warmup passes
per configuration are discarded by default. File reads and model loading occur
outside the timed region. Each measurement starts with resident WAV bytes and
ends with raw transcript text. Feature extraction, encoder, projector, native
TDT decoding, text assembly, audio parsing, and residual overhead are measured
separately. CPU usage is process CPU seconds / wall seconds / logical CPUs,
expressed as a percentage of whole-machine capacity. Median and nearest-rank
p95 are calculated from individual requests; short sweeps are exploratory.
WER uses word-level Levenshtein distance after lowercasing and removing
punctuation (internal apostrophes retained); references are never model-generated.

## Actual application pipeline

The ignored Rust test runs the production `process_job` path: original audio
preparation, multipart HTTP, server transcription, local cleanup and final text
formatting. Optional cloud rewriting and text insertion are disabled. It loads
each recording outside the timed region and discards two complete passes.
`preparation_probe_s` is a separate probe; the full timed pipeline still performs
and includes its own preparation. Windows server CPU time comes from
`GetProcessTimes`. Startup (including the application's startup warmup) and
server teardown are separate fields, never included in warm latency.

Build the test in release mode from a Visual Studio developer shell:

```powershell
$env:PRONTO_BENCH_MANIFEST = (Resolve-Path dist/phonon-benchmark/clips/manifest.json).Path
$env:PRONTO_BENCH_OUTPUT = "$PWD/dist/phonon-benchmark/pipeline.json"
$env:PRONTO_BENCH_REPEATS = '10'
# Explicit thread count permits a controlled baseline or candidate run.
$env:FERMION_CPU_THREADS = '8'
cargo test --manifest-path src-tauri/Cargo.toml --release --lib benchmark_phonon_warm_pipeline -- --ignored --nocapture --test-threads=1
```

Run the baseline and selected candidate on the same clips, alternating their
order to detect thermal drift. Preserve raw JSON results as well as summaries.
Compare both raw and final text, not just WER, to catch punctuation/format changes.
The Python stage profile does not include Rust trimming, HTTP, or cleanup; do not
present its time as the complete application latency. The Rust test excludes
microphone capture, UI event delivery, text insertion and busy-worker queueing.

## Scheduling (EcoQoS) — the measured bottleneck

Pronto dictates into other apps, so it and its windowless Phonon server are
background processes, and Windows 11 applies EcoQoS (power throttling) to
them: low clocks, E-core preference. On the i5-12450H (4P+4E, 12 logical)
that, not the model, explained the 5–10 s waits. `engine.rs` now opts the
Phonon server out right after spawning it (`opt_out_of_power_throttling`).

`serve_bench.py` launches `fermion serve` exactly as Pronto does
(arguments, environment, `CREATE_NO_WINDOW`), applies `--eco on|off|default`,
`--threads`, `--affinity` or `--priority` to the child from outside, reports
model load separately, then runs `http_bench.py` (Pronto's multipart request,
submission → text). `http_bench.py --port N --pid P` can also time the server
a running Pronto launched. `winqos.py` prints the P/E core topology.

```powershell
& $python -I scripts/benchmarks/serve_bench.py --eco on  --repeats 3 --output dist/phonon-benchmark/matrix/eco-on.json
& $python -I scripts/benchmarks/serve_bench.py --eco off --repeats 3 --output dist/phonon-benchmark/matrix/eco-off.json
```

Measured 2026-10-09 (9 clips, 27 requests each, default 8 threads, AC power):
EcoQoS on 3.25 s median / 11.5 s p95 at 31% server CPU; OS default 1.16 / 5.8;
opted out 0.74 / 3.0 at 74%. Transcripts identical. With the opt-out, 4, 6,
8 or 12 threads, P-core affinity and above-normal priority all landed within
0.73–0.86 s median, so the thread default is unchanged.

## Parakeet (CUDA) — measured 2026-10-09

RTX 4050 Laptop, i5-12450H, **on battery** (the GPU stayed at P3/P4, ~1.0–1.6 GHz of
3.1 GHz; expect lower absolute numbers on AC). The same harness covers Parakeet:

```powershell
& $python -I scripts/benchmarks/make_long_clips.py            # 55/60/120/200 s + meeting chunks
& $python -I scripts/benchmarks/serve_bench.py --backend parakeet --repeats 5 --output dist/parakeet-benchmark/b2b.json
& $python -I scripts/benchmarks/serve_bench.py --backend parakeet --gap 10 --repeats 3 --output dist/parakeet-benchmark/gap10.json
& $python -I scripts/benchmarks/serve_bench.py --backend parakeet --lifecycle 5 --output dist/parakeet-benchmark/lifecycle.json
& $python -I scripts/benchmarks/gpu_wake_probe.py --output dist/parakeet-benchmark/wake.json
& $python -I scripts/benchmarks/compare.py dist/parakeet-benchmark/b2b.json   # one report, or two to compare
```

`telemetry.py` adds GPU P-state/clocks/utilisation (NVML), system CPU and AC/battery
to every row. Rust benchmarks (release, `--ignored`): `benchmark_warm_pipeline`
(`PRONTO_BENCH_BACKEND=parakeet`, `PRONTO_BENCH_CLEANUP=1` for DeepSeek,
`PRONTO_BENCH_GAP_MS`), `benchmark_model_lifecycle`, `benchmark_meeting_chunks`
(`PRONTO_BENCH_MEETING_WAV`), `benchmark_deepseek_connection` and, in insert.rs,
`benchmark_insertion_latency`.

Warm ASR, model loaded, back-to-back (submission → text, 5 repeats, 0% WER on the 9 clips):
1.8 s audio 23 ms, 4.8 s 31 ms, 11.5 s 76 ms, 23 s 178 ms, 28 s 230 ms; long clips
55 s 0.51 s, 60 s 0.61 s, 121 s 1.60 s, 205 s 3.73 s (full-context attention grows faster
than linearly). The 55 s clip loses its first sentence (8 words) in every run though that
sentence alone is transcribed correctly: a long-form model behaviour, unchanged here.

* **Idle downclocking dominates real dictation.** After 10 s idle the GPU is in P8
  (210 MHz) and ramps only near the end of a request: 2.2 s clip 185 ms vs 27 ms
  back-to-back, 7.6 s 265 vs 49 ms, 28 s 504 vs 230 ms. After a request the GPU holds
  P4 ~4 s and reaches P8 at ~6 s. Small "wake" requests while recording
  (`gpu_wake_probe.py`) helped only with a ~1 s lead; across 5–10 s leads the driver
  still stepped clocks down and results were inconsistent, so no wake was shipped.
* **EcoQoS does not affect nemo-speech by default:** median 0.049 s (OS default),
  0.052 (opted out), 0.054 (forced on). Its CPU side is ~1 thread (7–8% of the machine).
* **`--threads` is the HTTP worker count**, not compute. 1 vs 4 workers: identical for
  dictation. The server's keep-alive timeout is 5 s and one worker stays on an idle
  keep-alive connection, so 3 parallel meeting chunks on 1 worker took 16 s instead of
  ~5 s and a connection was aborted; with 3 workers concurrent 130 s chunks oversubscribed
  VRAM (batches of 188 s and 1,445 s). Meeting chunks now go one at a time
  (~1.6 s per 130 s chunk).
* **Load / release / reload:** load 1.03 s (1.59 s cold), first request +15–20 ms, stop
  35–50 ms, model ~805 MiB VRAM. Recording start already sends `warm()`, so a reload after
  a GPU-pressure release overlaps speech.
* **Total − ASR in history is DeepSeek cleanup.** `totalMs − asrMs − cleanupMs` is 0–1 ms in
  every history entry. A new TLS connection to api.deepseek.com costs ~110 ms, and after
  2–5 idle minutes the DNS lookup alone took 2.15–2.56 s (curl, 4 of 12 probes), above
  the client's 2 s connect timeout, which matches a history entry with cleanup 2,008 ms
  and not applied. Dictation start now opens the DeepSeek connection (`preconnect_cleanup`).
* History's slowest "ASR" entries (5.3 s → 1,461 ms, 54.8 s → 5,708 ms) are 10–40× slower
  than Parakeet ever measured here and coincide with Phonon being active.
