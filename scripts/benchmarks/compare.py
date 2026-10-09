"""Summarise one benchmark report, or compare two, without third-party packages.

Reads both formats: production-pipeline reports from the Rust tests
(`raw_text` + `entry.finalText`) and http_bench.py / serve_bench.py reports
(`text`, plus GPU telemetry). With one file, prints per-clip median / p95,
word errors, server CPU and GPU clocks. With two, prints before/after per clip,
speedup, word errors and whether raw and final text are identical.
"""
import argparse
import json
import math
import re
import statistics
from pathlib import Path


def words(text):
    return re.findall(r"[a-z0-9]+(?:'[a-z0-9]+)*", text.lower())


def edits(a, b):
    row = list(range(len(b)+1))
    for i, x in enumerate(a, 1):
        nxt = [i]
        for j, y in enumerate(b, 1):
            nxt.append(min(nxt[-1]+1, row[j]+1, row[j-1]+(x != y)))
        row = nxt
    return row[-1]


def stats(rows, key='latency_s'):
    values = sorted(r[key] for r in rows)
    return statistics.median(values), values[math.ceil(.95*len(values))-1]


def raw(row):
    return row.get('raw_text', row.get('text'))


def final(row):
    return row['entry']['finalText'] if 'entry' in row else raw(row)


def gpu(rows, key):
    values = [r['gpu'][key] for r in rows if r.get('gpu') and r['gpu'].get(key) is not None]
    return statistics.mean(values) if values else float('nan')


def describe(report):
    out = []
    for key in ('label', 'backend', 'gap_s', 'gap_ms', 'concurrency', 'cleanup', 'power'):
        if key in report:
            out.append(f'{key}={report[key]}')
    if 'server' in report:
        out.append(f"server={ {k: v for k, v in report['server'].items() if k != 'command'} }")
    return ' '.join(out)


def summarise(report):
    print(describe(report))
    print('| Clip | Audio s | Median / p95 s | Word errors | Server CPU % | GPU start P-state / MHz | GPU mean MHz | GPU util % |')
    print('|---|---:|---:|---:|---:|---:|---:|---:|')
    for clip in report['manifest']['clips']:
        rows = [r for r in report['rows'] if r['clip'] == clip['id']]
        med, p95 = stats(rows)
        errors = [edits(words(clip['reference']), words(raw(r))) for r in rows]
        print(f"| {clip['id']} | {clip['duration_s']:.1f} | {med:.3f} / {p95:.3f} | {min(errors)}–{max(errors)} "
              f"| {statistics.mean(r['server_cpu_percent'] for r in rows):.1f} "
              f"| P{gpu(rows, 'start_pstate'):.1f} / {gpu(rows, 'start_gr_mhz'):.0f} | {gpu(rows, 'mean_gr_mhz'):.0f} "
              f"| {gpu(rows, 'mean_util'):.0f} |")
    rows = report['rows']
    print('Pooled median / p95 s: %.3f / %.3f, n = %d' % (*stats(rows), len(rows)))
    for key in ('load_s',):
        if key in report.get('server', {}):
            print('load s', report['server'][key])
    if 'cleanup_ms' in rows[0]:
        cleanup = sorted(r['cleanup_ms'] for r in rows)
        print('cleanup ms median / p95', statistics.median(cleanup), cleanup[math.ceil(.95*len(cleanup))-1],
              'applied', sum(bool(r.get('cleanup_applied')) for r in rows), '/', len(rows))
    if report.get('summary'):
        print({k: v for k, v in report['summary'].items() if k != 'clips'})


def compare(before, after):
    assert [c['id'] for c in before['manifest']['clips']] == [c['id'] for c in after['manifest']['clips']], 'Inputs differ'
    print('Before:', describe(before))
    print('After: ', describe(after))
    print('| Clip | Audio s | Before median / p95 s | After median / p95 s | Speedup | Raw word errors before → after | Raw text identical | Final text identical |')
    print('|---|---:|---:|---:|---:|---:|---|---|')
    total_errors = [0, 0]
    total_words = 0
    for clip in before['manifest']['clips']:
        groups = [[r for r in report['rows'] if r['clip'] == clip['id']] for report in (before, after)]
        b, a = map(stats, groups)
        texts = [{raw(r) for r in group} for group in groups]
        finals = [{final(r) for r in group} for group in groups]
        reference = words(clip['reference'])
        errors = [[edits(reference, words(t)) for t in group] for group in texts]
        # A variable transcript is made explicit, not hidden by choosing the best repetition.
        for i in (0, 1):
            total_errors[i] += max(errors[i])
        total_words += len(reference)
        print(f"| {clip['id']} | {clip['duration_s']:.3f} | {b[0]:.3f} / {b[1]:.3f} | {a[0]:.3f} / {a[1]:.3f} "
              f"| {b[0]/a[0]:.2f}× | {min(errors[0])}–{max(errors[0])} → {min(errors[1])}–{max(errors[1])} "
              f"| {texts[0] == texts[1] and len(texts[0]) == 1} | {finals[0] == finals[1] and len(finals[0]) == 1} |")
    print('\nPooled requests (equal repetitions per clip):')
    for label, report in [('Before', before), ('After', after)]:
        print(label, 'median / p95', stats(report['rows']), 'n =', len(report['rows']))
        print('mean server CPU %', statistics.mean(r['server_cpu_percent'] for r in report['rows']))
        if 'load_including_startup_warmup_s' in report:
            print('startup including warmup s', report['load_including_startup_warmup_s'], 'stop s', report['stop_s'])
        elif 'server' in report:
            print('load s', report['server'].get('load_s'))
    print('Worst-repeat corpus WER:', [100*e/total_words for e in total_errors], 'reference words:', total_words)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('before', type=Path)
    p.add_argument('after', type=Path, nargs='?')
    args = p.parse_args()
    before = json.loads(args.before.read_text())
    if args.after is None:
        summarise(before)
    else:
        compare(before, json.loads(args.after.read_text()))


if __name__ == '__main__':
    main()
