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
