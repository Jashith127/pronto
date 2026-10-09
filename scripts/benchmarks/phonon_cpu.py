"""Warm CPU benchmark of the installed Fermion backend, with stage profiling.

Run using the SAME embedded Python as Pronto. No GPU, downloads, or model
changes. Inputs are preloaded PCM WAV bytes; loading/warmups are excluded.
Each thread configuration runs in a fresh process with discarded warmup passes.
Native and torch thread counts agree; numerical/decoder settings stay fixed.
"""
import argparse
import hashlib
import importlib.metadata
import json
import os
from pathlib import Path
import platform
import re
import statistics
import sys
import time

def power_status():
    if os.name != 'nt':
        return None
    import ctypes
    class Status(ctypes.Structure):
        _fields_ = [('ac', ctypes.c_ubyte), ('flags', ctypes.c_ubyte),
                    ('percent', ctypes.c_ubyte), ('reserved', ctypes.c_ubyte),
                    ('remaining', ctypes.c_uint32), ('full', ctypes.c_uint32)]
    status = Status()
    if not ctypes.windll.kernel32.GetSystemPowerStatus(ctypes.byref(status)):
        raise ctypes.WinError()
    return dict(ac=status.ac, battery_percent=status.percent)

os.environ['FERMION_DEVICE'] = 'cpu'
os.environ['CUDA_VISIBLE_DEVICES'] = '-1'
os.environ['HF_HUB_OFFLINE'] = '1'
os.environ['TRANSFORMERS_OFFLINE'] = '1'

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

def summary(rows):
    vals = sorted(r['latency_s'] for r in rows)
    import math
    return dict(median_s=statistics.median(vals), p95_s=vals[math.ceil(.95*len(vals))-1],
                mean_cpu_percent=statistics.mean(r['cpu_percent'] for r in rows))

def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--model', type=Path, default=Path(os.environ['LOCALAPPDATA'])/'Pronto/models/phonon-2')
    p.add_argument('--manifest', type=Path, default=Path('dist/phonon-benchmark/clips/manifest.json'))
    p.add_argument('--threads', default='8,1,2,4,6,12,8')
    p.add_argument('--repeats', type=int, default=5)
    p.add_argument('--warmups', type=int, default=2)
    p.add_argument('--encoder', choices=['c', 'torch'], default='c')
    p.add_argument('--torch-threads', type=int)
    p.add_argument('--eco', choices=['default', 'on', 'off'], default='default',
                   help='EcoQoS state: on = what Windows applies to background processes')
    p.add_argument('--priority', choices=['normal', 'above', 'high'])
    p.add_argument('--affinity', type=lambda v: int(v, 0), default=0, help='process affinity mask, e.g. 0xff')
    p.add_argument('--output', type=Path, default=Path('dist/phonon-benchmark/sweep.json'))
    args = p.parse_args()
    configs = list(map(int, args.threads.split(',')))
    if len(configs) > 1:
        # Separate processes reproduce the production launch configuration and
        # avoid relying on resizing an already-active native thread pool.
        import subprocess
        import sys
        for index, count in enumerate(configs):
            output = args.output.with_name(f'{args.output.stem}-{index}-{count}.json')
            command = [sys.executable, '-I', '-X', 'utf8', str(Path(__file__).resolve()),
                            '--model', str(args.model), '--manifest', str(args.manifest),
                            '--threads', str(count), '--repeats', str(args.repeats),
                            '--warmups', str(args.warmups), '--encoder', args.encoder, '--output', str(output)]
            if args.torch_threads is not None:
                command += ['--torch-threads', str(args.torch_threads)]
            command += ['--eco', args.eco, '--affinity', hex(args.affinity)]
            if args.priority:
                command += ['--priority', args.priority]
            subprocess.run(command, check=True)
        return
    if configs[0]:
        os.environ['FERMION_CPU_THREADS'] = str(configs[0])
    if os.name == 'nt':
        sys.path.insert(0, str(Path(__file__).resolve().parent))
        import winqos
        winqos.set_power_throttling(args.eco)
        winqos.set_priority(args.priority)
        winqos.set_affinity(args.affinity)
    os.environ['FERMION_P2_CPU_ENC'] = args.encoder
    manifest = json.loads(args.manifest.read_text())
    clips = manifest['clips']
    blobs = {}
    for clip in clips:
        blob = (args.manifest.parent/clip['path']).read_bytes()
        assert hashlib.sha256(blob).hexdigest() == clip['sha256']
        blobs[clip['id']] = blob
    started = time.perf_counter()
    import torch
    torch_import_s = time.perf_counter()-started
    from fermion._speech import engine, engine_phonon2_cpu
    from fermion.transcribe import _resolve
    _, key, pin, _ = _resolve(str(args.model))
    imports_s = time.perf_counter()-started
    loading = time.perf_counter()
    model = engine_phonon2_cpu.load(args.model, profile=key, backend=pin['backend'])
    loader_s = time.perf_counter()-loading
    load_s = time.perf_counter()-started
    if args.torch_threads is not None:
        torch.set_num_threads(args.torch_threads)
    assert not torch.cuda.is_available(), 'Expected CPU-only installed torch'
    stages = {}
    def wrap(obj, name, stage):
        original = getattr(obj, name)
        def measured(*a, **kw):
            start = time.perf_counter()
            try:
                return original(*a, **kw)
            finally:
                stages[stage] = stages.get(stage, 0) + time.perf_counter()-start
        setattr(obj, name, measured)
    wrap(model, '_log_mel', 'features_s')
    wrap(model, '_finish', 'text_decode_s')
    wrap(model.model.encoder_projector, 'forward', 'projector_s')
    if model._cenc is not None:
        wrap(model._cenc, 'forward', 'encoder_s')
    else:
        wrap(model.model.encoder, 'forward', 'encoder_s')
    if model._ctdt is not None:
        wrap(model._ctdt, 'decode_timed', 'tdt_s')
    report = dict(hardware=platform.processor(), logical_cpus=os.cpu_count(),
                  power=power_status(), torch_interop_threads=torch.get_num_interop_threads(),
                  torch_threads=torch.get_num_threads(), encoder=args.encoder,
                  eco=args.eco, priority=args.priority, affinity=hex(args.affinity),
                  versions={n: importlib.metadata.version(n) for n in ['fermion-research','torch','transformers','numpy']},
                  model=model.describe(), model_load_s=load_s, manifest=manifest,
                  load_stages=dict(torch_import_s=torch_import_s, imports_s=imports_s, loader_s=loader_s),
                  measurement='in-process backend: uploaded PCM WAV bytes to final raw text; excludes Rust preparation and HTTP',
                  rows=[], summaries=[])
    args.output.parent.mkdir(parents=True, exist_ok=True)
    for block, threads in enumerate(map(int,args.threads.split(','))):
        threads = threads or model.describe()['threads']
        assert torch.get_num_threads() == (args.torch_threads or threads)
        warm = time.perf_counter()
        for _ in range(args.warmups):
            for clip in clips:
                model.transcribe_array_detailed(engine.read_audio_bytes(blobs[clip['id']], 'audio.wav'))
                print(f"  warmup {threads} threads {clip['id']} complete", flush=True)
        print(f'block {block}: {threads} threads warmed ({time.perf_counter()-warm:.1f}s)', flush=True)
        for repeat in range(args.repeats):
            # Rotate ordering to distribute thermal/order effects among clips.
            for clip in clips[repeat % len(clips):] + clips[:repeat % len(clips)]:
                stages.clear()
                cpu = time.process_time()
                start = time.perf_counter()
                audio = engine.read_audio_bytes(blobs[clip['id']], 'audio.wav')
                stages['audio_read_s'] = time.perf_counter()-start
                result = model.transcribe_array_detailed(audio)
                elapsed = time.perf_counter()-start
                cpu_s = time.process_time()-cpu
                row = dict(block=block, threads=threads, repeat=repeat, clip=clip['id'],
                           duration_s=clip['duration_s'], latency_s=elapsed, cpu_s=cpu_s,
                           cpu_percent=100*cpu_s/elapsed/os.cpu_count(), text=result.text,
                           word_errors=distance(words(clip['reference']), words(result.text)),
                           reference_words=len(words(clip['reference'])), **stages)
                row['other_s'] = elapsed-sum(stages.values())
                report['rows'].append(row)
                args.output.write_text(json.dumps(report, indent=2))
                print(f"  {threads} threads {clip['id']} run {repeat}: {elapsed:.3f}s", flush=True)
        rows = [r for r in report['rows'] if r['block'] == block]
        item = dict(block=block, threads=threads, **summary(rows),
                    clips={c['id']: summary([r for r in rows if r['clip']==c['id']]) for c in clips})
        report['summaries'].append(item)
        args.output.write_text(json.dumps(report, indent=2))
        print(json.dumps(item), flush=True)

if __name__ == '__main__':
    main()
