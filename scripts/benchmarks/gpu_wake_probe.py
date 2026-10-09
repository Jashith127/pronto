"""How the laptop GPU's idle downclocking affects Parakeet, and what wakes it.

Starts nemo-speech exactly as Pronto does (serve_bench.server_command), then:

1. decay: after a request, samples the P-state / graphics clock every 50 ms
   for --decay-s seconds: how soon the GPU falls back to P8.
2. wake: for each idle period, lets the GPU idle, optionally sends small
   "wake" requests (a quiet tone, like engine.rs warm_up_clip) every
   --wake-interval seconds for the lead time a dictation gives (recording
   while the user speaks), then times a real clip. Lead 0 = no wake request,
   i.e. today's behaviour.

The wake requests are timed too: they would compete with a dictation that
ends while one is in flight.
"""
import argparse
import http.client
import io
import json
import math
from pathlib import Path
import statistics
import struct
import sys
import time
import wave

sys.path.insert(0, str(Path(__file__).resolve().parent))
import http_bench  # noqa: E402
import serve_bench  # noqa: E402
import telemetry  # noqa: E402


def tone(seconds, rate=16_000):
    """A quiet 220 Hz tone as PCM16 WAV bytes (engine.rs warm_up_clip)."""
    frames = b''.join(struct.pack('<h', int(math.sin(i * 220 * math.tau / rate) * 0.05 * 32767))
                      for i in range(int(seconds * rate)))
    buffer = io.BytesIO()
    with wave.open(buffer, 'wb') as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(frames)
    return buffer.getvalue()


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--port', type=int, default=51902)
    p.add_argument('--manifest', type=Path, default=Path('dist/phonon-benchmark/clips/manifest.json'))
    p.add_argument('--clips', default='5639-40744-0008,1320-122617-0012,5639-40744-0031')
    p.add_argument('--idle-s', type=float, default=12, help='idle before each trial (GPU reaches P8)')
    p.add_argument('--leads', default='0,0.5,1,2,4,8', help='seconds of wake requests before the clip')
    p.add_argument('--wake-interval', type=float, default=1.0)
    p.add_argument('--wake-tone-s', type=float, default=1.0)
    p.add_argument('--repeats', type=int, default=3)
    p.add_argument('--decay-s', type=float, default=15)
    p.add_argument('--output', type=Path, required=True)
    args = p.parse_args()
    data = serve_bench.DATA
    server_args = argparse.Namespace(
        backend='parakeet', exe=data / 'runtimes/nemo-speech-cuda/bin/nemo-speech.exe',
        model=data / 'models/parakeet-tdt-0.6b-v3.q8_0.gguf', port=args.port, server_threads=1, extra='',
        eco='default', priority=None, affinity=0)
    manifest = json.loads(args.manifest.read_text())
    clips = [c for c in manifest['clips'] if c['id'] in args.clips.split(',')]
    blobs = {c['id']: (args.manifest.parent / c['path']).read_bytes() for c in clips}
    wake_blob = tone(args.wake_tone_s)
    gpu = telemetry.GpuSampler()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    log = open(args.output.with_suffix('.server.log'), 'w')
    child, load_s = serve_bench.start(server_args, log)
    report = dict(power=telemetry.power_source(), load_s=load_s, args={k: str(v) for k, v in vars(args).items()},
                  decay=[], trials=[])

    def post(blob):
        conn = http.client.HTTPConnection('127.0.0.1', args.port, timeout=600)
        start = time.perf_counter()
        text = http_bench.transcribe(conn, blob, 'parakeet')
        conn.close()
        return time.perf_counter() - start, text

    try:
        for clip in clips:  # warm the model's buffers for every clip length
            post(blobs[clip['id']])
        # 1. Decay after one request.
        post(blobs[clips[-1]['id']])
        t0 = time.perf_counter()
        while time.perf_counter() - t0 < args.decay_s:
            s = gpu.now()
            report['decay'].append(dict(t=time.perf_counter() - t0, pstate=s['pstate'], gr_mhz=s['gr_mhz'],
                                        power_w=s['power_w']))
            time.sleep(0.05)
        p8 = next((d['t'] for d in report['decay'] if d['pstate'] == 8), None)
        print(f'decay: P8 reached {p8}s after the last request', flush=True)
        # 2. Wake leads.
        leads = [float(v) for v in args.leads.split(',')]
        for repeat in range(args.repeats):
            for lead in leads:
                for clip in clips:
                    time.sleep(args.idle_s)
                    before = gpu.now()
                    wakes = []
                    started = time.perf_counter()
                    while lead and time.perf_counter() - started < lead:
                        wakes.append(post(wake_blob)[0])
                        remaining = lead - (time.perf_counter() - started)
                        time.sleep(max(0.0, min(args.wake_interval - wakes[-1], remaining)))
                    with gpu.window() as window:
                        latency, text = post(blobs[clip['id']])
                    row = dict(repeat=repeat, lead_s=lead, clip=clip['id'], duration_s=clip['duration_s'],
                               latency_s=latency, idle_pstate=before['pstate'] if before else None,
                               wake_requests=len(wakes), wake_latency_s=wakes, gpu=window.summary(),
                               word_errors=http_bench.distance(http_bench.words(clip['reference']),
                                                               http_bench.words(text)), text=text)
                    report['trials'].append(row)
                    g = row['gpu'] or {}
                    print(f"lead {lead:4.1f}s {clip['id']} {clip['duration_s']:5.1f}s -> {latency * 1000:6.1f} ms "
                          f"start P{g.get('start_pstate')} {g.get('start_gr_mhz')} MHz, "
                          f"wakes {len(wakes)} ({', '.join(f'{w * 1000:.0f}' for w in wakes[:6])} ms)", flush=True)
    finally:
        serve_bench.stop(child)
    summary = {}
    for lead in sorted({t['lead_s'] for t in report['trials']}):
        for clip in clips:
            values = sorted(t['latency_s'] for t in report['trials'] if t['lead_s'] == lead and t['clip'] == clip['id'])
            summary[f"lead {lead} {clip['id']}"] = dict(median_s=statistics.median(values), max_s=values[-1], n=len(values))
    report['summary'] = summary
    args.output.write_text(json.dumps(report, indent=2))
    for key, value in summary.items():
        print(key, value)


if __name__ == '__main__':
    main()
