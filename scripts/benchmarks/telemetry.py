"""Machine telemetry for the speech benchmarks (ctypes, stdlib only).

`GpuSampler` polls NVML on a background thread while a request runs and
summarises graphics/memory clocks, P-state, utilisation and power for that
window; `GpuSampler.now()` gives an instantaneous reading (the GPU's state
when a request arrives, e.g. idle-downclocked P8). NVML utilisation is the
driver's own moving average (1/6-1 s window), so it is coarse for requests
shorter than ~200 ms; clocks and P-state are instantaneous.
`system_cpu_seconds()` is busy time across all logical CPUs
(GetSystemTimes) and `power_source()` reports AC vs battery.
Every function degrades to None when NVML or the API is unavailable.
"""
import ctypes
from ctypes import wintypes
import statistics
import threading
import time

kernel32 = ctypes.WinDLL('kernel32', use_last_error=True)


class _PowerStatus(ctypes.Structure):
    _fields_ = [('ACLineStatus', ctypes.c_ubyte), ('BatteryFlag', ctypes.c_ubyte),
                ('BatteryLifePercent', ctypes.c_ubyte), ('SystemStatusFlag', ctypes.c_ubyte),
                ('BatteryLifeTime', wintypes.DWORD), ('BatteryFullLifeTime', wintypes.DWORD)]


def power_source():
    status = _PowerStatus()
    if not kernel32.GetSystemPowerStatus(ctypes.byref(status)):
        return None
    return dict(ac={0: False, 1: True}.get(status.ACLineStatus),
                battery_percent=None if status.BatteryLifePercent == 255 else status.BatteryLifePercent,
                # SystemStatusFlag 1 = Windows battery saver on
                battery_saver=bool(status.SystemStatusFlag & 1))


def system_cpu_seconds():
    """Busy seconds summed over all logical CPUs since boot."""
    idle, kernel, user = (wintypes.FILETIME() for _ in range(3))
    if not kernel32.GetSystemTimes(ctypes.byref(idle), ctypes.byref(kernel), ctypes.byref(user)):
        return None
    ticks = lambda t: (t.dwHighDateTime << 32 | t.dwLowDateTime) / 1e7  # noqa: E731
    return ticks(kernel) + ticks(user) - ticks(idle)  # kernel time includes idle


class _Utilization(ctypes.Structure):
    _fields_ = [('gpu', ctypes.c_uint), ('memory', ctypes.c_uint)]


class _Memory(ctypes.Structure):
    _fields_ = [('total', ctypes.c_ulonglong), ('free', ctypes.c_ulonglong), ('used', ctypes.c_ulonglong)]


class Nvml:
    GRAPHICS, MEMORY = 0, 2

    def __init__(self):
        self.lib = ctypes.CDLL('nvml.dll')
        if self.lib.nvmlInit_v2() != 0:
            raise OSError('nvmlInit failed')
        self.device = ctypes.c_void_p()
        if self.lib.nvmlDeviceGetHandleByIndex_v2(0, ctypes.byref(self.device)) != 0:
            raise OSError('no NVIDIA device 0')

    def _uint(self, fn, *args):
        value = ctypes.c_uint()
        return value.value if fn(self.device, *args, ctypes.byref(value)) == 0 else None

    def read(self):
        util, pstate, memory = _Utilization(), ctypes.c_int(), _Memory()
        ok = self.lib.nvmlDeviceGetUtilizationRates(self.device, ctypes.byref(util)) == 0
        power_mw = self._uint(self.lib.nvmlDeviceGetPowerUsage)
        mem_ok = self.lib.nvmlDeviceGetMemoryInfo(self.device, ctypes.byref(memory)) == 0
        return dict(
            t=time.perf_counter(),
            gr_mhz=self._uint(self.lib.nvmlDeviceGetClockInfo, self.GRAPHICS),
            mem_mhz=self._uint(self.lib.nvmlDeviceGetClockInfo, self.MEMORY),
            pstate=pstate.value if self.lib.nvmlDeviceGetPerformanceState(self.device, ctypes.byref(pstate)) == 0 else None,
            util=util.gpu if ok else None,
            # Occasional readings are garbage (hundreds of watts on a ~60 W part); drop them.
            # The cumulative energy counter updates in coarse steps and is unusable per request.
            power_w=power_mw / 1000 if power_mw is not None and power_mw < 200_000 else None,
            vram_used_mib=memory.used / 2**20 if mem_ok else None)


class GpuSampler:
    """`with sampler.window() as w: ...` then `w.summary()`; no-op without NVML."""

    def __init__(self, interval=0.01):
        try:
            self.nvml = Nvml()
        except OSError:
            self.nvml = None
        self.interval = interval

    def now(self):
        return self.nvml.read() if self.nvml else None

    def window(self):
        return _Window(self)


class _Window:
    def __init__(self, sampler):
        self.sampler, self.samples, self._stop = sampler, [], threading.Event()

    def __enter__(self):
        if self.sampler.nvml:
            self.samples.append(self.sampler.now())
            self._thread = threading.Thread(target=self._run, daemon=True)
            self._thread.start()
        return self

    def _run(self):
        while not self._stop.wait(self.sampler.interval):
            self.samples.append(self.sampler.now())

    def __exit__(self, *_):
        if self.sampler.nvml:
            self._stop.set()
            self._thread.join()

    def summary(self):
        if not self.samples:
            return None
        first = self.samples[0]
        col = lambda key: [s[key] for s in self.samples if s[key] is not None]  # noqa: E731
        mean = lambda key: statistics.mean(col(key)) if col(key) else None  # noqa: E731
        peak = lambda key: max(col(key)) if col(key) else None  # noqa: E731
        return dict(start_pstate=first['pstate'], start_gr_mhz=first['gr_mhz'], start_mem_mhz=first['mem_mhz'],
                    min_pstate=min(col('pstate')) if col('pstate') else None,
                    mean_gr_mhz=mean('gr_mhz'), max_gr_mhz=peak('gr_mhz'), mean_mem_mhz=mean('mem_mhz'),
                    mean_util=mean('util'), max_util=peak('util'),
                    mean_power_w=mean('power_w'), max_power_w=peak('power_w'),
                    vram_used_mib=peak('vram_used_mib'), samples=len(self.samples))


if __name__ == '__main__':
    print(power_source())
    sampler = GpuSampler()
    print(sampler.now())
