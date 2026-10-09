"""Fetch a small deterministic, multi-speaker slice of OpenSLR test-clean.

Streams the official archive; saves only three utterances per speaker from
the first three speakers (shortest, middle, longest), plus reference text.
Corpus: https://www.openslr.org/12/ (CC BY 4.0).
"""
import hashlib
import io
import json
from pathlib import Path
import tarfile
import urllib.request

import soundfile as sf

out = Path(__file__).resolve().parents[2] / 'dist/phonon-benchmark/clips'
out.mkdir(parents=True, exist_ok=True)
url = 'https://www.openslr.org/resources/12/test-clean.tar.gz'
speakers, refs = {}, {}
with urllib.request.urlopen(url, timeout=120) as response:
    with tarfile.open(fileobj=response, mode='r|gz') as archive:
        for member in archive:
            parts = member.name.split('/')
            if len(parts) != 5 or not member.isfile():
                continue
            speaker = parts[2]
            if speaker not in speakers and len(speakers) >= 3:
                break
            data = archive.extractfile(member).read()
            if member.name.endswith('.trans.txt'):
                for line in data.decode().splitlines():
                    key, text = line.split(' ', 1)
                    refs[key] = text
            elif member.name.endswith('.flac'):
                info = sf.info(io.BytesIO(data))
                speakers.setdefault(speaker, []).append((info.duration, Path(member.name).stem, data))
rows = []
for speaker, clips in speakers.items():
    clips.sort()
    for duration, key, data in (clips[0], clips[len(clips)//2], clips[-1]):
        audio, rate = sf.read(io.BytesIO(data), dtype='int16')
        path = out / (key + '.wav')
        sf.write(path, audio, rate, subtype='PCM_16')
        rows.append(dict(id=key, path=path.name, duration_s=duration,
                         reference=refs[key], sha256=hashlib.sha256(path.read_bytes()).hexdigest()))
(out / 'manifest.json').write_text(json.dumps(dict(source=url, license='CC BY 4.0', clips=rows), indent=2))
print(json.dumps(rows, indent=2))
