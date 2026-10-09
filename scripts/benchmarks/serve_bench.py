"""Start a speech server the way Pronto does, apply scheduling controls to the
child from outside (as Pronto's Rust side would), then run http_bench.py.

--backend phonon starts `fermion serve`; --backend parakeet starts
`nemo-speech serve` on CUDA. Both use the same arguments, environment and
CREATE_NO_WINDOW flag as `server_command` in src-tauri/src/engine.rs;
--server-threads and --extra change nemo-speech flags for experiments.
Model load time is reported separately and never mixed into transcription
latency. --lifecycle N instead loads, sends one request and stops the server
N times: load, first request, stop and VRAM are the cost of a GPU-pressure
release and reload.
"""
import argparse
import http.client
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import time
import urllib.request

sys.path.insert(0, str(Path(__file__).resolve().parent))
import http_bench  # noqa: E402
import telemetry  # noqa: E402
import winqos  # noqa: E402

HERE = Path(__file__).resolve().parent
DATA = Path(os.environ['LOCALAPPDATA']) / 'Pronto'


def server_command(args):
    """(argv, cwd, env) exactly as engine.rs `server_command` builds them."""
    if args.backend == 'parakeet':
        argv = [str(args.exe), 'serve', '--host', '127.0.0.1', '--port', str(args.port),
                '--threads', str(args.server_threads), '--no-ui', '--device', 'cuda:0',
                '--asr-model', str(args.model)] + shlex.split(args.extra)
        return argv, args.exe.parent, dict(os.environ)
    pack_root = args.python.parent.parent
    env = dict(os.environ, FERMION_DEVICE='cpu', FERMION_CACHE_DIR=str(pack_root / 'cache'),
               HF_HUB_OFFLINE='1', HF_HUB_DISABLE_TELEMETRY='1', TRANSFORMERS_OFFLINE='1')
    env.pop('FERMION_CPU_THREADS', None)
    if args.threads:
        env['FERMION_CPU_THREADS'] = str(args.threads)
    argv = [str(args.python), '-I', '-X', 'utf8', '-m', 'fermion.cli', 'serve', str(args.model),
            '--host', '127.0.0.1', '--port', str(args.port)]
    return argv, pack_root, env


def start(args, log):
    """Spawn, apply scheduling controls, wait for /health. Returns (child, load_s)."""
    argv, cwd, env = server_command(args)
    started = time.perf_counter()
    child = subprocess.Popen(argv, cwd=cwd, env=env, stdout=log, stderr=subprocess.STDOUT,
                             creationflags=0x0800_0000)  # CREATE_NO_WINDOW, as Pronto
    # Applied immediately after spawn, before the model creates its worker threads.
    handle = winqos.open_process(child.pid)
    winqos.set_power_throttling(args.eco, handle)
    winqos.set_priority(args.priority, handle)
    winqos.set_affinity(args.affinity, handle)
    while True:
        if child.poll() is not None:
            raise SystemExit(f'server exited during startup; see {log.name}')
        try:
            urllib.request.urlopen(f'http://127.0.0.1:{args.port}/health', timeout=2).read()
            return child, time.perf_counter() - started
        except OSError:
            time.sleep(0.02)  # fine polling: true load time (Pronto polls every 200 ms)


def stop(child):
    started = time.perf_counter()
    child.kill()
    child.wait()
    return time.perf_counter() - started


def lifecycle(args, log):
    manifest = json.loads(args.manifest.read_text())
    clip = min(manifest['clips'], key=lambda c: c['duration_s'])
    blob = (args.manifest.parent / clip['path']).read_bytes()
    gpu = telemetry.GpuSampler()
    vram = lambda: (gpu.now() or {}).get('vram_used_mib')  # noqa: E731
    rows = []
    for cycle in range(args.lifecycle):
        time.sleep(args.gap)
        before = vram()
        child, load_s = start(args, log)
        loaded = vram()
        conn = http.client.HTTPConnection('127.0.0.1', args.port, timeout=600)
        first = time.perf_counter()
        text = http_bench.transcribe(conn, blob, args.backend)
        first_s = time.perf_counter() - first
        second = time.perf_counter()
        http_bench.transcribe(conn, blob, args.backend)
        second_s = time.perf_counter() - second
        conn.close()
        stop_s = stop(child)
        time.sleep(0.5)
        rows.append(dict(cycle=cycle, load_s=load_s, first_request_s=first_s, second_request_s=second_s,
                         stop_s=stop_s, vram_before_mib=before, vram_loaded_mib=loaded,
                         vram_after_stop_mib=vram(), clip=clip['id'], text=text))
        print(json.dumps(rows[-1]), flush=True)
    return dict(label=args.label, backend=args.backend, power=telemetry.power_source(), rows=rows,
                command=server_command(args)[0])


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--backend', choices=['phonon', 'parakeet'], default='phonon')
    p.add_argument('--python', type=Path, default=DATA / 'runtimes/phonon-cpu/python/python.exe')
    p.add_argument('--exe', type=Path, default=DATA / 'runtimes/nemo-speech-cuda/bin/nemo-speech.exe')
    p.add_argument('--model', type=Path, help='default: the installed model for --backend')
    p.add_argument('--port', type=int, default=51900)
    p.add_argument('--threads', type=int, default=0, help='Phonon FERMION_CPU_THREADS; 0 = fermion default')
    p.add_argument('--server-threads', type=int, default=1, help='nemo-speech --threads (HTTP workers); Pronto uses 1')
    p.add_argument('--extra', default='', help='extra nemo-speech serve arguments')
    p.add_argument('--eco', choices=['default', 'on', 'off'], default='default')
    p.add_argument('--priority', choices=['idle', 'below', 'normal', 'above', 'high'])
    p.add_argument('--affinity', type=lambda v: int(v, 0), default=0)
    p.add_argument('--manifest', type=Path, default=Path('dist/phonon-benchmark/clips/manifest.json'))
    p.add_argument('--repeats', type=int, default=5)
    p.add_argument('--warmups', type=int, default=1)
    p.add_argument('--gap', type=float, default=0.0)
    p.add_argument('--concurrency', type=int, default=1)
    p.add_argument('--lifecycle', type=int, default=0, help='load/request/stop cycles instead of a latency run')
    p.add_argument('--label', default='')
    p.add_argument('--output', type=Path, required=True)
    args = p.parse_args()
    if args.model is None:
        args.model = DATA / ('models/parakeet-tdt-0.6b-v3.q8_0.gguf' if args.backend == 'parakeet'
                             else 'models/phonon-2')
    args.output.parent.mkdir(parents=True, exist_ok=True)
    log = open(args.output.with_suffix('.server.log'), 'w')
    if args.lifecycle:
        args.output.write_text(json.dumps(lifecycle(args, log), indent=2))
        return
    child, load_s = start(args, log)
    try:
        print(f'[{args.label}] model loaded in {load_s:.2f}s (not part of latency)', flush=True)
        subprocess.run([sys.executable, '-I', str(HERE / 'http_bench.py'), '--port', str(args.port),
                        '--pid', str(child.pid), '--backend', args.backend, '--manifest', str(args.manifest),
                        '--repeats', str(args.repeats), '--warmups', str(args.warmups), '--gap', str(args.gap),
                        '--concurrency', str(args.concurrency), '--label', args.label,
                        '--output', str(args.output)], check=True)
        report = json.loads(args.output.read_text())
        report['server'] = dict(backend=args.backend, threads=args.threads, server_threads=args.server_threads,
                                extra=args.extra, eco=args.eco, priority=args.priority,
                                affinity=hex(args.affinity), load_s=load_s, command=server_command(args)[0])
        args.output.write_text(json.dumps(report, indent=2))
    finally:
        stop(child)


if __name__ == '__main__':
    main()
