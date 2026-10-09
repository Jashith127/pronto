"""Release-notes chart for the Phonon EcoQoS fix, in the style of the Parakeet one.

Reads the controlled serve_bench results (eco-on = how Pronto 0.9's server ran
while the app was in the background; eco-off = this fix) and writes a
self-contained HTML page. Render it to PNG with a headless browser.
"""
import json
import math
from pathlib import Path
import statistics
import sys

BEFORE, AFTER = '#cdd3d9', '#2a78d6'  # blue for Phonon; the Parakeet chart is rust
INK, INK2, MUTED, GRID = '#1f1f1f', '#555555', '#777777', '#e6e6e6'
CLIPS = ['6930-75918-0008', '6930-75918-0003']  # a 4.8 s and a 23.3 s recording


def p95(values):
    values = sorted(values)
    return values[math.ceil(.95 * len(values)) - 1]


def load(matrix):
    before = json.loads((matrix / 'eco-on.json').read_text())
    after = json.loads((matrix / 'eco-off.json').read_text())
    lat = lambda rep, clip=None: [r['latency_s'] for r in rep['rows'] if clip in (None, r['clip'])]
    rows = [('Median dictation', statistics.median(lat(before)), statistics.median(lat(after))),
            ('p95 dictation', p95(lat(before)), p95(lat(after)))]
    for clip in sorted(before['manifest']['clips'], key=lambda c: c['duration_s']):
        if clip['id'] in CLIPS:
            rows.append((f"{clip['duration_s']:.0f} s clip", statistics.median(lat(before, clip['id'])),
                         statistics.median(lat(after, clip['id']))))
    return rows


def page(rows):
    W, left, right, top, group_h, bar_h = 650, 140, 52, 64, 44, 12
    plot_w = W - left - right
    xmax = math.ceil((max(b for _, b, _ in rows) + 1.5) / 3) * 3  # room for the end label
    x = lambda s: left + s / xmax * plot_w
    H = top + group_h * len(rows) + 34
    out = [f'<svg viewBox="0 0 {W} {H}" width="{W}" height="{H}" xmlns="http://www.w3.org/2000/svg">',
           f'<text x="10" y="22" font-size="15" font-weight="650" fill="{INK}">'
           f'Time from end of recording to text (Phonon)</text>']
    for i, (label, color) in enumerate([('Pronto 0.9', BEFORE), ('Pronto 1.0', AFTER)]):
        lx = 150 + i * 150
        out.append(f'<rect x="{lx}" y="34" width="12" height="12" rx="2" fill="{color}"/>'
                   f'<text x="{lx + 19}" y="44.5" font-size="12.5" fill="{INK2}">{label}</text>')
    for tick in range(0, xmax + 1, 3):
        out.append(f'<line x1="{x(tick):.1f}" x2="{x(tick):.1f}" y1="{top - 6}" y2="{H - 30}" stroke="{GRID}"/>'
                   f'<text x="{x(tick):.1f}" y="{H - 12}" font-size="12" fill="{MUTED}" text-anchor="middle">{tick} s</text>')
    for i, (label, before, after) in enumerate(rows):
        y = top + i * group_h
        out.append(f'<text x="{left - 12}" y="{y + bar_h + 5}" font-size="13" fill="{INK}" text-anchor="end">{label}</text>')
        for j, (value, color, weight) in enumerate([(before, BEFORE, 400), (after, AFTER, 650)]):
            by = y + j * (bar_h + 3)
            out.append(f'<rect x="{left}" y="{by}" width="{x(value) - left:.1f}" height="{bar_h}" rx="2" fill="{color}"/>'
                       f'<text x="{x(value) + 7:.1f}" y="{by + bar_h - 2}" font-size="11.5" font-weight="{weight}" '
                       f'fill="{INK if j else MUTED}">{value:.2f} s</text>')
    out.append('</svg>')
    return (f'<!doctype html><html><head><meta charset="utf-8"><style>body{{margin:0;background:#fff;'
            f'font-family:"Segoe UI Variable Text","Segoe UI",system-ui,sans-serif}}'
            f'svg text{{font-variant-numeric:tabular-nums}}</style></head><body>{"".join(out)}</body></html>')


def main():
    root = Path(__file__).resolve().parents[2]
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else root / 'dist/phonon-benchmark/chart'
    out.mkdir(parents=True, exist_ok=True)
    rows = load(root / 'dist/phonon-benchmark/matrix')
    (out / 'phonon-speed.html').write_text(page(rows), encoding='utf-8')
    print(rows)


if __name__ == '__main__':
    main()
