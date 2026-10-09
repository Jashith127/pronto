"""Warm latency of a RUNNING Phonon server, exactly as Pronto calls it.

Posts each benchmark WAV as the same multipart form Pronto sends (file +
response_format=json, no model/language) and times submission -> parsed text.
Use it against the server the installed app launched to see what the app
really gets (background scheduling included), or against any test server.
Server CPU is read with GetProcessTimes on --pid when given.
"""
import argparse
import ctypes
import http.client
import json
import math
import os
from pathlib import Path
import re
import statistics
import time
import uuid


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


def transcribe(conn, blob):
    boundary = uuid.uuid4().hex
    body = (f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="audio.wav"\r\n'
            f'Content-Type: audio/wav\r\n\r\n').encode() + blob + (
            f'\r\n--{boundary}\r\nContent-Disposition: form-data; name="response_format"\r\n\r\n'
            f'json\r\n--{boundary}--\r\n').encode()
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
    p.add_argument('--manifest', type=Path, default=Path('dist/phonon-benchmark/clips/manifest.json'))
    p.add_argument('--repeats', type=int, default=5)
    p.add_argument('--warmups', type=int, default=1)
    p.add_argument('--gap', type=float, default=0.0, help='idle seconds before each request (dictation is bursty)')
    p.add_argument('--label', default='')
    p.add_argument('--output', type=Path, required=True)
    args = p.parse_args()
    manifest = json.loads(args.manifest.read_text())
    clips = manifest['clips']
    blobs = {c['id']: (args.manifest.parent / c['path']).read_bytes() for c in clips}
    conn = http.client.HTTPConnection('127.0.0.1', args.port, timeout=600)
    conn.request('GET', '/health')
    health = json.loads(conn.getresponse().read())
    report = dict(label=args.label, port=args.port, gap_s=args.gap, logical_cpus=os.cpu_count(),
                  decode=health.get('decode'), manifest=manifest, rows=[])
    for _ in range(args.warmups):
        for clip in clips:
            transcribe(conn, blobs[clip['id']])
    for repeat in range(args.repeats):
        for clip in clips[repeat % len(clips):] + clips[:repeat % len(clips)]:
            if args.gap:
                time.sleep(args.gap)
            cpu = process_cpu_seconds(args.pid)
            start = time.perf_counter()
            text = transcribe(conn, blobs[clip['id']])
            elapsed = time.perf_counter() - start
            cpu_s = process_cpu_seconds(args.pid) - cpu
            report['rows'].append(dict(
                repeat=repeat, clip=clip['id'], duration_s=clip['duration_s'], latency_s=elapsed,
                server_cpu_s=cpu_s, server_cpu_percent=100 * cpu_s / elapsed / os.cpu_count(),
                text=text, word_errors=distance(words(clip['reference']), words(text)),
                reference_words=len(words(clip['reference']))))
            print(f"{clip['id']} {clip['duration_s']:5.1f}s audio -> {elapsed:6.3f}s "
                  f"cpu {report['rows'][-1]['server_cpu_percent']:4.0f}%", flush=True)
    rows = report['rows']
    med, p95 = stats([r['latency_s'] for r in rows])
    report['summary'] = dict(
        median_s=med, p95_s=p95,
        mean_server_cpu_percent=statistics.mean(r['server_cpu_percent'] for r in rows),
        wer_percent=100 * sum(r['word_errors'] for r in rows) / sum(r['reference_words'] for r in rows),
        clips={c['id']: dict(zip(('median_s', 'p95_s'), stats([r['latency_s'] for r in rows if r['clip'] == c['id']])))
               for c in clips})
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2))
    print(json.dumps({k: v for k, v in report['summary'].items() if k != 'clips'}))


if __name__ == '__main__':
    main()
