"""Compare two production-pipeline JSON reports without third-party packages."""
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

def stats(rows):
    values = sorted(r['latency_s'] for r in rows)
    return statistics.median(values), values[math.ceil(.95*len(values))-1]

def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('before', type=Path)
    p.add_argument('after', type=Path)
    args = p.parse_args()
    before, after = [json.loads(f.read_text()) for f in (args.before, args.after)]
    assert before['manifest'] == after['manifest'], 'Inputs differ'
    print('| Clip | Audio s | Before median / p95 s | After median / p95 s | Speedup | Raw word errors before → after | Final text identical |')
    print('|---|---:|---:|---:|---:|---:|---|')
    total_errors = [0, 0]
    total_words = 0
    for clip in before['manifest']['clips']:
        groups = [[r for r in report['rows'] if r['clip']==clip['id']] for report in (before, after)]
        b, a = map(stats, groups)
        texts = [{r['raw_text'] for r in group} for group in groups]
        final = [{r['entry']['finalText'] for r in group} for group in groups]
        reference = words(clip['reference'])
        errors = [[edits(reference, words(t)) for t in group] for group in texts]
        # A variable transcript is made explicit, not hidden by choosing the best repetition.
        for i in (0, 1):
            total_errors[i] += max(errors[i])
        total_words += len(reference)
        print(f"| {clip['id']} | {clip['duration_s']:.3f} | {b[0]:.3f} / {b[1]:.3f} | {a[0]:.3f} / {a[1]:.3f} | {b[0]/a[0]:.2f}× | {min(errors[0])}–{max(errors[0])} → {min(errors[1])}–{max(errors[1])} | {final[0] == final[1] and len(final[0]) == 1} |")
    print('\nPooled requests (equal repetitions per clip):')
    for label, report in [('Before', before), ('After', after)]:
        print(label, 'median / p95', stats(report['rows']), 'n =', len(report['rows']))
        print('mean server CPU %', statistics.mean(r['server_cpu_percent'] for r in report['rows']))
        print('startup including warmup s', report['load_including_startup_warmup_s'], 'stop s', report['stop_s'])
    print('Worst-repeat corpus WER:', [100*e/total_words for e in total_errors], 'reference words:', total_words)

if __name__ == '__main__':
    main()
