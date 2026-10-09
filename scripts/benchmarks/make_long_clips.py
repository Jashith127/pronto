"""Build long benchmark clips by concatenating the fetched LibriSpeech clips.

Run after fetch_librispeech.py. Writes manifest-long.json beside the clips:
~55 s and ~60 s dictations (lengths seen in real history), a 120 s clip (one
meeting chunk, CHUNK_SAMPLES in engine.rs) and a ~200 s dictation; and
manifest-meeting.json: three different ~120 s chunks of one meeting, for the
parallel meeting path (http_bench.py --concurrency 3). Each is
the source utterances joined by 0.25 s of silence; the reference is their
references joined in the same order, so WER stays exact. Stdlib only.
"""
import hashlib
import json
from pathlib import Path
import wave

clips_dir = Path(__file__).resolve().parents[2] / 'dist/phonon-benchmark/clips'
manifest = json.loads((clips_dir / 'manifest.json').read_text())
by_id = {c['id']: c for c in manifest['clips']}
ALL = [c['id'] for c in manifest['clips']]
LONG = {
    'long-055': ['5639-40744-0008', '5639-40744-0036', '5639-40744-0031', '1320-122617-0012', '6930-75918-0008'],
    'long-060': ['6930-81414-0005', '6930-75918-0008', '6930-75918-0003',
                 '1320-122617-0038', '1320-122617-0012', '1320-122617-0007'],
    'long-120': ALL + ['1320-122617-0007'],
    'long-200': ALL + ALL,
}
MEETING = {}
for n in range(3):
    # Distinct ~120 s chunks: rotate the utterance order, then fill to >= 118 s.
    order = (ALL[3 * n:] + ALL[:3 * n]) * 2
    parts, total = [], 0.0
    while total < 118:
        parts.append(order[len(parts)])
        total += by_id[parts[-1]]['duration_s'] + 0.25
    MEETING[f'meeting-{n + 1}'] = parts
GAP_S = 0.25


def build(name, parts):
    frames, rate = [], None
    for part in parts:
        with wave.open(str(clips_dir / by_id[part]['path'])) as w:
            assert w.getnchannels() == 1 and w.getsampwidth() == 2
            rate = rate or w.getframerate()
            assert w.getframerate() == rate
            if frames:
                frames.append(b'\0\0' * int(GAP_S * rate))
            frames.append(w.readframes(w.getnframes()))
    data = b''.join(frames)
    path = clips_dir / f'{name}.wav'
    with wave.open(str(path), 'wb') as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(data)
    return dict(id=name, path=path.name, duration_s=len(data) / 2 / rate, parts=parts,
                reference=' '.join(by_id[p]['reference'] for p in parts),
                sha256=hashlib.sha256(path.read_bytes()).hexdigest())


for file, group in (('manifest-long.json', LONG), ('manifest-meeting.json', MEETING)):
    rows = [build(name, parts) for name, parts in group.items()]
    (clips_dir / file).write_text(json.dumps(
        dict(source=manifest['source'], license=manifest['license'], derived_from='manifest.json', clips=rows),
        indent=2))
    for row in rows:
        print(file, row['id'], f"{row['duration_s']:.2f}s", len(row['reference'].split()), 'words')
