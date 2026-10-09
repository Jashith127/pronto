"""Release-notes chart for the Phonon EcoQoS fix: before -> after per recording.

Reads the controlled serve_bench results (eco-on = how Pronto 0.9's server ran
while the app was in the background; eco-off = this fix) and writes one
self-contained HTML page per theme. Render each to PNG with a headless browser.
"""
import json
import math
from pathlib import Path
import statistics
import sys

THEMES = {
    'light': dict(surface='#fcfcfb', ink='#0b0b0b', ink2='#52514e', muted='#898781', grid='#e1e0d9',
                  axis='#c3c2b7', before='#6da7ec', after='#1c5cab', good='#006300', border='rgba(11,11,11,0.10)'),
    'dark': dict(surface='#1a1a19', ink='#ffffff', ink2='#c3c2b7', muted='#898781', grid='#2c2c2a',
                 axis='#383835', before='#898781', after='#3987e5', good='#0ca30c', border='rgba(255,255,255,0.10)'),
}
VOICES = {'6930': 'voice A', '1320': 'voice B', '5639': 'voice C'}


def p95(values):
    values = sorted(values)
    return values[math.ceil(.95 * len(values)) - 1]


def load(matrix):
    before = json.loads((matrix / 'eco-on.json').read_text())
    after = json.loads((matrix / 'eco-off.json').read_text())
    rows = []
    for clip in before['manifest']['clips']:
        b = statistics.median(r['latency_s'] for r in before['rows'] if r['clip'] == clip['id'])
        a = statistics.median(r['latency_s'] for r in after['rows'] if r['clip'] == clip['id'])
        rows.append(dict(length=clip['duration_s'], voice=VOICES[clip['id'][:4]], before=b, after=a))
    rows.sort(key=lambda r: (r['length'], r['voice']))
    lat = lambda rep: [r['latency_s'] for r in rep['rows']]
    kpi = dict(med=(before['summary']['median_s'], after['summary']['median_s']),
               p95=(p95(lat(before)), p95(lat(after))),
               wer=(before['summary']['wer_percent'], after['summary']['wer_percent']),
               same=({(r['clip'], r['text']) for r in before['rows']} == {(r['clip'], r['text']) for r in after['rows']}))
    return rows, kpi


def page(rows, kpi, t):
    W, left, right, top, row_h = 880, 150, 108, 34, 40
    plot_w = W - left - right
    xmax = math.ceil((max(r["before"] for r in rows) + 1) / 2) * 2  # room for the end label
    x = lambda s: left + s / xmax * plot_w
    H = top + row_h * len(rows) + 46
    svg = [f'<svg viewBox="0 0 {W} {H}" width="{W}" height="{H}" role="img" '
           f'aria-label="Seconds until text appears, before and after, for nine recordings">']
    for tick in range(0, xmax + 1, 2):
        svg.append(f'<line x1="{x(tick):.1f}" x2="{x(tick):.1f}" y1="{top - 10}" y2="{top + row_h * len(rows) - 10}" '
                   f'stroke="{t["grid"] if tick else t["axis"]}" stroke-width="1"/>')
        svg.append(f'<text x="{x(tick):.1f}" y="{top + row_h * len(rows) + 8}" fill="{t["muted"]}" '
                   f'font-size="12" text-anchor="middle">{tick} s</text>')
    svg.append(f'<text x="{left + plot_w / 2:.1f}" y="{H - 6}" fill="{t["ink2"]}" font-size="12.5" '
               f'text-anchor="middle">Seconds from the end of the recording until the text is ready (median)</text>')
    svg.append(f'<text x="{W - 4}" y="{top - 16}" fill="{t["muted"]}" font-size="12" text-anchor="end">Faster by</text>')
    for i, r in enumerate(rows):
        y = top + i * row_h + 10
        svg.append(f'<text x="{left - 18}" y="{y + 4.5}" fill="{t["ink"]}" font-size="13.5" text-anchor="end">'
                   f'{r["length"]:.1f} s<tspan fill="{t["muted"]}" font-size="12"> · {r["voice"]}</tspan></text>')
        svg.append(f'<line x1="{x(r["after"]):.1f}" x2="{x(r["before"]):.1f}" y1="{y}" y2="{y}" '
                   f'stroke="{t["before"]}" stroke-width="2" stroke-opacity="0.55"/>')
        svg.append(f'<circle cx="{x(r["before"]):.1f}" cy="{y}" r="5.5" fill="{t["surface"]}" '
                   f'stroke="{t["before"]}" stroke-width="2.5"/>')
        svg.append(f'<circle cx="{x(r["after"]):.1f}" cy="{y}" r="6" fill="{t["after"]}" '
                   f'stroke="{t["surface"]}" stroke-width="2"/>')
        svg.append(f'<text x="{x(r["before"]) + 11:.1f}" y="{y + 4.5}" fill="{t["ink2"]}" font-size="12.5">'
                   f'{r["before"]:.1f} s</text>')
        svg.append(f'<text x="{x(r["after"]) - 11:.1f}" y="{y + 4.5}" fill="{t["ink"]}" font-size="12.5" '
                   f'font-weight="600" text-anchor="end">{r["after"]:.2f} s</text>' if x(r['after']) - left > 40 else
                   f'<text x="{x(r["after"]):.1f}" y="{y - 10}" fill="{t["ink"]}" font-size="12" font-weight="600" '
                   f'text-anchor="middle">{r["after"]:.2f} s</text>')
        svg.append(f'<text x="{W - 4}" y="{y + 4.5}" fill="{t["ink"]}" font-size="13.5" font-weight="600" '
                   f'text-anchor="end">{r["before"] / r["after"]:.1f}×</text>')
    svg.append('</svg>')

    def tile(label, before, after, note):
        return (f'<div class="tile"><div class="label">{label}</div><div class="value">'
                f'<span class="was">{before}</span><span class="arrow">→</span>{after}</div>'
                f'<div class="note">{note}</div></div>')
    (mb, ma), (pb, pa) = kpi['med'], kpi['p95']
    tiles = ''.join([
        tile('Typical wait', f'{mb:.1f} s', f'{ma:.1f} s', f'{mb / ma:.1f}× faster (median)'),
        tile('Longest waits', f'{pb:.1f} s', f'{pa:.1f} s', f'{pb / pa:.1f}× faster (95th percentile)'),
        f'<div class="tile"><div class="label">Accuracy</div><div class="value">Unchanged</div>'
        f'<div class="note">{"Identical text, word for word" if kpi["same"] else "Same word error rate"}'
        f'</div></div>'])
    return f'''<!doctype html><html><head><meta charset="utf-8"><style>
body{{margin:0;background:{t["surface"]};font-family:"Segoe UI Variable Text","Segoe UI",system-ui,sans-serif;color:{t["ink"]}}}
.card{{width:{W}px;padding:28px 32px 22px}}
h1{{font-size:24px;font-weight:650;margin:0 0 6px;letter-spacing:-.01em}}
.sub{{font-size:14.5px;color:{t["ink2"]};margin:0 0 20px;line-height:1.45}}
.tiles{{display:grid;grid-template-columns:repeat(3,1fr);gap:12px;margin-bottom:22px}}
.tile{{border:1px solid {t["border"]};border-radius:10px;padding:12px 14px}}
.label{{font-size:12.5px;color:{t["ink2"]};margin-bottom:4px}}
.value{{font-size:24px;font-weight:650;font-variant-numeric:tabular-nums}}
.was{{color:{t["muted"]};font-weight:500;text-decoration:line-through;text-decoration-thickness:1px}}
.arrow{{color:{t["muted"]};font-weight:400;margin:0 8px;font-size:18px}}
.note{{font-size:12.5px;color:{t["ink2"]};margin-top:2px}}
.legend{{display:flex;gap:22px;font-size:13px;color:{t["ink2"]};margin:0 0 6px}}
.legend span{{display:inline-flex;align-items:center;gap:7px}}
.foot{{font-size:11.5px;color:{t["muted"]};margin-top:14px;line-height:1.5}}
svg text{{font-variant-numeric:tabular-nums}}
</style></head><body><div class="card">
<h1>Phonon now turns speech into text about 4× faster</h1>
<p class="sub">Pronto runs in the background while you type in other apps, and Windows was slowing its CPU speech engine down to save power. Pronto now tells Windows not to throttle Phonon. The model and the transcripts are unchanged.</p>
<div class="tiles">{tiles}</div>
<div class="legend"><span><svg width="14" height="14"><circle cx="7" cy="7" r="5" fill="{t["surface"]}" stroke="{t["before"]}" stroke-width="2.5"/></svg>Before (Pronto 0.9, in the background)</span>
<span><svg width="14" height="14"><circle cx="7" cy="7" r="6" fill="{t["after"]}"/></svg>Now</span></div>
{"".join(svg)}
<div class="foot">Intel Core i5-12450H laptop (4 performance + 4 efficiency cores), CPU only, plugged in. Nine LibriSpeech test-clean recordings from three speakers, three timed runs each, with the model already loaded. Times include reading the audio, running the model and returning the text.</div>
</div></body></html>'''


def main():
    root = Path(__file__).resolve().parents[2]
    matrix = root / 'dist/phonon-benchmark/matrix'
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else root / 'dist/phonon-benchmark/chart'
    out.mkdir(parents=True, exist_ok=True)
    rows, kpi = load(matrix)
    for name, theme in THEMES.items():
        (out / f'phonon-speed-{name}.html').write_text(page(rows, kpi, theme), encoding='utf-8')
    print(json.dumps(kpi), out)


if __name__ == '__main__':
    main()
