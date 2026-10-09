"""Windows scheduling controls for the Phonon benchmarks (ctypes, stdlib only).

`topology()` lists each logical processor's efficiency class (Alder Lake:
P-cores report a higher class than E-cores). `set_power_throttling()` sets the
process's EcoQoS state explicitly: 'on' reproduces what Windows applies to
background, windowless processes; 'off' opts out; 'default' leaves the OS
heuristic in charge (what Pronto's server got before this change).
"""
import ctypes
from ctypes import wintypes

kernel32 = ctypes.WinDLL('kernel32', use_last_error=True)
kernel32.GetCurrentProcess.restype = wintypes.HANDLE
kernel32.SetPriorityClass.argtypes = (wintypes.HANDLE, wintypes.DWORD)

PROCESS_POWER_THROTTLING_CURRENT_VERSION = 1
PROCESS_POWER_THROTTLING_EXECUTION_SPEED = 0x1
PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION = 0x4
ProcessPowerThrottling = 4


class _Throttle(ctypes.Structure):
    _fields_ = [('Version', wintypes.ULONG), ('ControlMask', wintypes.ULONG),
                ('StateMask', wintypes.ULONG)]


def set_power_throttling(state, handle=None):
    if state == 'default':
        return
    control = PROCESS_POWER_THROTTLING_EXECUTION_SPEED | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION
    value = _Throttle(PROCESS_POWER_THROTTLING_CURRENT_VERSION, control,
                      control if state == 'on' else 0)
    kernel32.SetProcessInformation.argtypes = (wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD)
    if not kernel32.SetProcessInformation(handle or kernel32.GetCurrentProcess(), ProcessPowerThrottling,
                                          ctypes.byref(value), ctypes.sizeof(value)):
        raise ctypes.WinError(ctypes.get_last_error())


PRIORITIES = {'idle': 0x40, 'below': 0x4000, 'normal': 0x20, 'above': 0x8000, 'high': 0x80}


def set_priority(name, handle=None):
    if name and not kernel32.SetPriorityClass(handle or kernel32.GetCurrentProcess(), PRIORITIES[name]):
        raise ctypes.WinError(ctypes.get_last_error())


def set_affinity(mask, handle=None):
    if mask:
        kernel32.SetProcessAffinityMask.argtypes = (wintypes.HANDLE, ctypes.c_size_t)
        if not kernel32.SetProcessAffinityMask(handle or kernel32.GetCurrentProcess(), mask):
            raise ctypes.WinError(ctypes.get_last_error())


def open_process(pid):
    kernel32.OpenProcess.restype = wintypes.HANDLE
    # PROCESS_SET_INFORMATION | PROCESS_QUERY_INFORMATION
    handle = kernel32.OpenProcess(0x0200 | 0x0400, False, pid)
    if not handle:
        raise ctypes.WinError(ctypes.get_last_error())
    return handle


def topology():
    """[(logical index, core index, efficiency class)] via GetSystemCpuSetInformation."""
    length = wintypes.ULONG()
    kernel32.GetSystemCpuSetInformation(None, 0, ctypes.byref(length), None, 0)
    buf = (ctypes.c_ubyte * length.value)()
    if not kernel32.GetSystemCpuSetInformation(buf, length, ctypes.byref(length), None, 0):
        raise ctypes.WinError(ctypes.get_last_error())
    out, offset = [], 0
    while offset < length.value:
        size = int.from_bytes(bytes(buf[offset:offset + 4]), 'little')
        # SYSTEM_CPU_SET_INFORMATION.CpuSet: Id@8, Group@12, LogicalProcessorIndex@14,
        # CoreIndex@15, LastLevelCacheIndex@16, NumaNodeIndex@17, EfficiencyClass@18
        lp, core, eff = buf[offset + 14], buf[offset + 15], buf[offset + 18]
        out.append((lp, core, eff))
        offset += size
    return out


if __name__ == '__main__':
    for row in topology():
        print('logical %2d core %2d efficiency class %d' % row)
