"""Start `fermion serve` the way Pronto does, apply scheduling controls to the
child from outside (as Pronto's Rust side would), then run http_bench.py.

Same arguments, environment and CREATE_NO_WINDOW flag as `server_command`
in src-tauri/src/engine.rs. Model load time is reported separately and never
mixed into transcription latency.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import urllib.request

sys.path.insert(0, str(Path(__file__).resolve().parent))
import winqos  # noqa: E402

HERE = Path(__file__).resolve().parent



def main():
    p = argparse.ArgumentParser(description=__doc__)
    data = Path(os.environ['LOCALAPPDATA']) / 'Pronto'
    p.add_argument('--python', type=Path, default=data / 'runtimes/phonon-cpu/python/python.exe')
    p.add_argument('--model', type=Path, default=data / 'models/phonon-2')
    p.add_argument('--port', type=int, default=51900)
    p.add_argument('--threads', type=int, default=0, help='FERMION_CPU_THREADS; 0 = fermion default')
    p.add_argument('--eco', choices=['default', 'on', 'off'], default='default')
    p.add_argument('--priority', choices=['idle', 'below', 'normal', 'above', 'high'])
    p.add_argument('--affinity', type=lambda v: int(v, 0), default=0)
    p.add_argument('--repeats', type=int, default=5)
    p.add_argument('--warmups', type=int, default=1)
    p.add_argument('--gap', type=float, default=0.0)
    p.add_argument('--label', default='')
    p.add_argument('--output', type=Path, required=True)
    args = p.parse_args()
    pack_root = args.python.parent.parent
    env = dict(os.environ, FERMION_DEVICE='cpu', FERMION_CACHE_DIR=str(pack_root / 'cache'),
               HF_HUB_OFFLINE='1', HF_HUB_DISABLE_TELEMETRY='1', TRANSFORMERS_OFFLINE='1')
    env.pop('FERMION_CPU_THREADS', None)
    if args.threads:
        env['FERMION_CPU_THREADS'] = str(args.threads)
    log = open(args.output.with_suffix('.server.log'), 'w')
    started = time.perf_counter()
    child = subprocess.Popen(
        [str(args.python), '-I', '-X', 'utf8', '-m', 'fermion.cli', 'serve', str(args.model),
         '--host', '127.0.0.1', '--port', str(args.port)],
        cwd=pack_root, env=env, stdout=log, stderr=subprocess.STDOUT,
        creationflags=0x0800_0000)  # CREATE_NO_WINDOW, as Pronto
    # Applied immediately after spawn, before the model creates its worker threads.
    handle = winqos.open_process(child.pid)
    winqos.set_power_throttling(args.eco, handle)
    winqos.set_priority(args.priority, handle)
    winqos.set_affinity(args.affinity, handle)
    try:
        while True:
            if child.poll() is not None:
                raise SystemExit(f'server exited during startup; see {log.name}')
            try:
                urllib.request.urlopen(f'http://127.0.0.1:{args.port}/health', timeout=2).read()
                break
            except OSError:
                time.sleep(0.2)
        load_s = time.perf_counter() - started
        print(f'[{args.label}] model loaded in {load_s:.1f}s (not part of latency)', flush=True)
        subprocess.run([sys.executable, '-I', str(HERE / 'http_bench.py'), '--port', str(args.port),
                        '--pid', str(child.pid), '--repeats', str(args.repeats), '--warmups', str(args.warmups),
                        '--gap', str(args.gap), '--label', args.label, '--output', str(args.output)], check=True)
        report = json.loads(args.output.read_text())
        report['server'] = dict(threads=args.threads, eco=args.eco, priority=args.priority,
                                affinity=hex(args.affinity), load_s=load_s)
        args.output.write_text(json.dumps(report, indent=2))
    finally:
        child.kill()
        child.wait()


if __name__ == '__main__':
    main()
