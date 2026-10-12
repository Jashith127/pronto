"""Speeds up Phonon's CPU load in the fermion-research package that
build-speech-packs.ps1 installs. Each edit must match the pinned release
exactly; when a fermion bump moves the code, this fails the build instead of
shipping a half-patched runtime. Neither edit changes the model's arithmetic.

  * The 264 packed encoder matrices are built in parallel instead of one by
    one: 10.2 s -> 3.1 s on an i5-12450H (8 threads), outputs bit-identical.
    PackedMatrix setup is ctypes calls into the native library, which release
    the GIL, and each matrix owns its own native handle.
  * Physical cores come from GetLogicalProcessorInformationEx instead of a
    PowerShell + CIM subprocess, which cost 2-3 s of every load.

Usage: python patch-fermion.py <site-packages> <fermion version>
"""
import sys
from pathlib import Path

PINNED = "0.2.7"

SERIAL_BUILD = """\
                    packed[e["n"]] = pk.PackedMatrix(packed_lib, None, planes=planes[e["n"]])   # cache hit: no trit decode
"""
DEFERRED_BUILD = """\
                    packed[e["n"]] = None                  # cache hit: no trit decode; built in parallel below
"""

READ_END = """\
        assert fh.read(1) == b"", "trailing bytes"
    return sd, raw, packed, n_fv
"""
PARALLEL_READ_END = """\
        assert fh.read(1) == b"", "trailing bytes"
    # [pronto] PackedMatrix setup is ctypes calls that release the GIL, one native handle per matrix.
    pending = [name for name, pm in packed.items() if pm is None]
    if pending:
        from concurrent.futures import ThreadPoolExecutor
        with ThreadPoolExecutor(max(1, min(len(pending), os.cpu_count() or 4, 16))) as pool:
            built = pool.map(lambda name: pk.PackedMatrix(packed_lib, None, planes=planes[name]), pending)
            for name, pm in zip(pending, built):
                packed[name] = pm
    return sd, raw, packed, n_fv
"""

POWERSHELL_CORES = """\
def _physical_cores_windows() -> int | None:
    import subprocess
    try:
        out = subprocess.run(["powershell", "-NoProfile", "-Command",
                              "(Get-CimInstance Win32_Processor | Measure-Object -Property NumberOfCores -Sum).Sum"],
                             capture_output=True, text=True, timeout=8).stdout.strip()
        return int(out) or None
    except Exception:                                    # noqa: BLE001
        return None
"""
NATIVE_CORES = """\
def _physical_cores_windows() -> int | None:
    # [pronto] One record per physical core (RelationProcessorCore = 0), summed across sockets like
    # Win32_Processor.NumberOfCores, without the 2-3 s PowerShell start.
    import ctypes
    from ctypes import wintypes
    try:
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        query = kernel32.GetLogicalProcessorInformationEx
        query.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.POINTER(wintypes.DWORD)]
        query.restype = wintypes.BOOL
        size = wintypes.DWORD(0)
        query(0, None, ctypes.byref(size))
        buf = ctypes.create_string_buffer(size.value)
        if not size.value or not query(0, buf, ctypes.byref(size)):
            return None
        cores, offset, raw = 0, 0, buf.raw
        while offset + 8 <= size.value:
            step = int.from_bytes(raw[offset + 4: offset + 8], "little")   # Relationship, then Size
            if step <= 0:
                return None
            cores += 1; offset += step
        return cores or None
    except Exception:                                    # noqa: BLE001
        return None
"""

EDITS = [(SERIAL_BUILD, DEFERRED_BUILD), (READ_END, PARALLEL_READ_END), (POWERSHELL_CORES, NATIVE_CORES)]


def main() -> None:
    site, version = Path(sys.argv[1]), sys.argv[2]
    if version != PINNED:
        sys.exit(f"patch-fermion.py targets fermion-research {PINNED}, not {version}. Re-check each edit, then bump PINNED.")
    target = site / "fermion" / "_speech" / "engine_phonon2_cpu.py"
    source = target.read_text(encoding="utf-8")
    for old, new in EDITS:
        if source.count(old) != 1:
            sys.exit(f"{target}: expected one match for:\n{old}")
        source = source.replace(old, new)
    target.write_text(source, encoding="utf-8", newline="\n")
    print(f"Patched {target}")


if __name__ == "__main__":
    main()
