"""Warm latency of a RUNNING speech server (Phonon or Parakeet), exactly as Pronto calls it.

Posts each benchmark WAV as the same multipart form Pronto sends (file +
response_format=json; Parakeet also gets model=parakeet, and no language
field because Pronto omits it for "auto") and times submission -> parsed
text. Use it against the server the installed app launched to see what the
app really gets (background scheduling included), or against any test server.
Server CPU is read with GetProcessTimes on --pid when given; system CPU,
GPU clocks/P-state/utilisation and the power source come from telemetry.py.

--gap N idles N seconds before each request (dictation is bursty: the GPU
drops to P8 within seconds). --concurrency N sends each repeat's clips over
N parallel connections, as meeting chunks are, and also records batch wall time.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import ctypes
import http.client
import json
import math
import os
from pathlib import Path
import re
import statistics
import sys
import time
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
import telemetry  # noqa: E402


def words(text):
    return re.findall(r"[a-z0-9]+(?:'[a-z0-9]+)*", text.lower())


def distance(a, b):
    row = list(range(len(b) + 1))
    for i, x in enumerate(a, 1):
        nxt = [i]
        for j, y in enumerate(b, 1):
            nxt.append(min(nxt[-1] + 1, row[j] + 1, row[j-1] + (x != y)))
        row = nxt
    return row[-1]


def process_cpu_seconds(pid):
    if not pid:
        return 0.0
    k = ctypes.windll.kernel32
    handle = k.OpenProcess(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION
    if not handle:
        raise ctypes.WinError()
    times = [ctypes.c_ulonglong() for _ in range(4)]
    k.GetProcessTimes(handle, *map(ctypes.byref, times))
    k.CloseHandle(handle)
    return (times[2].value + times[3].value) / 1e7


def transcribe(conn, blob, backend):
    boundary = uuid.uuid4().hex
    fields = [('response_format', 'json')] + ([('model', 'parakeet')] if backend == 'parakeet' else [])
    body = (f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="audio.wav"\r\n'
            f'Content-Type: audio/wav\r\n\r\n').encode() + blob + b''.join(
            f'\r\n--{boundary}\r\nContent-Disposition: form-data; name="{k}"\r\n\r\n{v}'.encode()
            for k, v in fields) + f'\r\n--{boundary}--\r\n'.encode()
    conn.request('POST', '/v1/audio/transcriptions', body,
                 {'Content-Type': f'multipart/form-data; boundary={boundary}'})
    response = conn.getresponse()
    data = response.read()
    if response.status != 200:
        raise RuntimeError(f'{response.status}: {data[:300]!r}')
    return json.loads(data)['text'].strip()


def stats(values):
    values = sorted(values)
    return statistics.median(values), values[math.ceil(.95 * len(values)) - 1]


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--port', type=int, required=True)
    p.add_argument('--pid', type=int, default=0)
    p.add_argument('--backend', choices=['phonon', 'parakeet'], default='phonon')
    p.add_argument('--manifest', type=Path, default=Path('dist/phonon-benchmark/clips/manifest.json'))
    p.add_argument('--repeats', type=int, default=5)
    p.add_argument('--warmups', type=int, default=1)
    p.add_argument('--gap', type=float, default=0.0, help='idle seconds before each request (dictation is bursty)')
    p.add_argument('--concurrency', type=int, default=1, help='parallel requests per repeat (meeting chunks)')
    p.add_argument('--label', default='')
    p.add_argument('--output', type=Path, required=True)
    args = p.parse_args()
    manifest = json.loads(args.manifest.read_text())
    clips = manifest['clips']
    blobs = {c['id']: (args.manifest.parent / c['path']).read_bytes() for c in clips}
    gpu = telemetry.GpuSampler()
    conn = http.client.HTTPConnection('127.0.0.1', args.port, timeout=600)
    conn.request('GET', '/health')
    health = json.loads(conn.getresponse().read())
    report = dict(label=args.label, port=args.port, backend=args.backend, gap_s=args.gap,
                  concurrency=args.concurrency, logical_cpus=os.cpu_count(), power=telemetry.power_source(),
                  decode=health.get('decode'), health=health, manifest=manifest, warmup_rows=[], rows=[], batches=[])

    def timed(clip, repeat, connection):
        cpu = process_cpu_seconds(args.pid)
        sys_cpu = telemetry.system_cpu_seconds()
        with gpu.window() as window:
            start = time.perf_counter()
            text = transcribe(connection, blobs[clip['id']], args.backend)
            elapsed = time.perf_counter() - start
        cpu_s = process_cpu_seconds(args.pid) - cpu
        sys_cpu_s = telemetry.system_cpu_seconds() - sys_cpu
        return dict(
            repeat=repeat, clip=clip['id'], duration_s=clip['duration_s'], latency_s=elapsed,
            server_cpu_s=cpu_s, server_cpu_percent=100 * cpu_s / elapsed / os.cpu_count(),
            system_cpu_percent=100 * sys_cpu_s / elapsed / os.cpu_count(), gpu=window.summary(),
            text=text, word_errors=distance(words(clip['reference']), words(text)),
            reference_words=len(words(clip['reference'])))

    def show(row, prefix=''):
        g = row['gpu'] or {}
        print(f"{prefix}{row['clip']} {row['duration_s']:5.1f}s audio -> {row['latency_s']:6.3f}s "
              f"cpu {row['server_cpu_percent']:4.0f}% sys {row['system_cpu_percent']:4.0f}% "
              f"gpu P{g.get('start_pstate')}->{g.get('min_pstate')} {g.get('start_gr_mhz')}->{g.get('max_gr_mhz')} MHz "
              f"util {g.get('max_util')}%", flush=True)

    # Warmups run in order, so the first row is the first request after load.
    for _ in range(args.warmups):
        for clip in clips:
            report['warmup_rows'].append(timed(clip, -1, conn))
            show(report['warmup_rows'][-1], 'warmup ')
    connections = [conn] + [http.client.HTTPConnection('127.0.0.1', args.port, timeout=600)
                            for _ in range(args.concurrency - 1)]
    for repeat in range(args.repeats):
        order = clips[repeat % len(clips):] + clips[:repeat % len(clips)]
        if args.concurrency == 1:
            for clip in order:
                if args.gap:
                    time.sleep(args.gap)
                    # The server closes keep-alive connections after 5 s idle, so after a
                    # gap Pronto's client reconnects; connecting is inside the timed region.
                    conn.close()
                report['rows'].append(timed(clip, repeat, conn))
                show(report['rows'][-1])
            continue
        if args.gap:
            time.sleep(args.gap)
            for c in connections:
                c.close()
        # Each worker owns one keep-alive connection, like reqwest's pool under load.
        lanes = [order[i::args.concurrency] for i in range(args.concurrency)]
        start = time.perf_counter()
        with ThreadPoolExecutor(args.concurrency) as pool:
            results = pool.map(lambda lane: [timed(c, repeat, connections[lane[0]]) for c in lane[1]],
                               enumerate(lanes))
            rows = [row for lane in results for row in lane]
        wall = time.perf_counter() - start
        report['rows'].extend(rows)
        report['batches'].append(dict(repeat=repeat, wall_s=wall, audio_s=sum(c['duration_s'] for c in clips)))
        print(f'batch {repeat}: {wall:.3f}s wall for {len(rows)} requests', flush=True)
    rows = report['rows']
    med, p95 = stats([r['latency_s'] for r in rows])
    gmean = lambda key: statistics.mean(r['gpu'][key] for r in rows if r['gpu'] and r['gpu'][key] is not None)  # noqa: E731
    report['summary'] = dict(
        median_s=med, p95_s=p95,
        mean_server_cpu_percent=statistics.mean(r['server_cpu_percent'] for r in rows),
        mean_system_cpu_percent=statistics.mean(r['system_cpu_percent'] for r in rows),
        mean_gpu_util_percent=gmean('mean_util') if gpu.nvml else None,
        mean_gr_mhz=gmean('mean_gr_mhz') if gpu.nvml else None,
        wer_percent=100 * sum(r['word_errors'] for r in rows) / sum(r['reference_words'] for r in rows),
        first_after_load_s=report['warmup_rows'][0]['latency_s'] if report['warmup_rows'] else None,
        batch_wall_median_s=statistics.median(b['wall_s'] for b in report['batches']) if report['batches'] else None,
        clips={c['id']: dict(zip(('median_s', 'p95_s'), stats([r['latency_s'] for r in rows if r['clip'] == c['id']])))
               for c in clips})
    report['power_end'] = telemetry.power_source()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2))
    print(json.dumps({k: v for k, v in report['summary'].items() if k != 'clips'}))


if __name__ == '__main__':
    main()
