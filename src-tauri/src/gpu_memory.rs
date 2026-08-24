//! Lightweight NVIDIA VRAM telemetry loaded directly from NVML.
//!
//! Calling the driver in-process avoids spawning `nvidia-smi` every few
//! seconds. The monitor is optional: Pronto keeps working normally when NVML
//! is unavailable or the machine does not have an NVIDIA GPU.

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;
    use windows::core::{s, w, PCSTR};
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

    const NVML_SUCCESS: i32 = 0;

    type NvmlDevice = *mut c_void;
    type NvmlInit = unsafe extern "C" fn() -> i32;
    type NvmlShutdown = unsafe extern "C" fn() -> i32;
    type NvmlDeviceGetHandle = unsafe extern "C" fn(u32, *mut NvmlDevice) -> i32;
    type NvmlDeviceGetMemoryInfo = unsafe extern "C" fn(NvmlDevice, *mut NvmlMemory) -> i32;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct NvmlMemory {
        total: u64,
        free: u64,
        used: u64,
    }

    #[derive(Clone, Copy, Debug)]
    pub struct MemoryInfo {
        pub total: u64,
        pub free: u64,
        pub used: u64,
    }

    pub struct GpuMemoryMonitor {
        _module: HMODULE,
        device: NvmlDevice,
        get_memory_info: NvmlDeviceGetMemoryInfo,
        shutdown: NvmlShutdown,
    }

    impl GpuMemoryMonitor {
        pub fn new() -> Result<Self, String> {
            let module = unsafe { LoadLibraryW(w!("nvml.dll")) }
                .map_err(|error| format!("NVML is unavailable: {error}"))?;
            let result = unsafe {
                let init: NvmlInit = load_symbol(module, s!("nvmlInit_v2"))?;
                let shutdown: NvmlShutdown = load_symbol(module, s!("nvmlShutdown"))?;
                let get_handle: NvmlDeviceGetHandle =
                    load_symbol(module, s!("nvmlDeviceGetHandleByIndex_v2"))?;
                let get_memory_info: NvmlDeviceGetMemoryInfo =
                    load_symbol(module, s!("nvmlDeviceGetMemoryInfo"))?;
                if init() != NVML_SUCCESS {
                    return Err("NVML could not initialize".to_string());
                }
                let mut device = std::ptr::null_mut();
                if get_handle(0, &mut device) != NVML_SUCCESS || device.is_null() {
                    let _ = shutdown();
                    return Err("NVML could not open the primary NVIDIA GPU".to_string());
                }
                Ok(Self {
                    _module: module,
                    device,
                    get_memory_info,
                    shutdown,
                })
            };
            result
        }

        pub fn memory_info(&self) -> Result<MemoryInfo, String> {
            let mut memory = NvmlMemory::default();
            let result = unsafe { (self.get_memory_info)(self.device, &mut memory) };
            if result != NVML_SUCCESS {
                return Err(format!("NVML memory query failed ({result})"));
            }
            Ok(MemoryInfo {
                total: memory.total,
                free: memory.free,
                used: memory.used,
            })
        }
    }

    impl Drop for GpuMemoryMonitor {
        fn drop(&mut self) {
            let _ = unsafe { (self.shutdown)() };
            // Keep NVML loaded for the process lifetime. Windows releases the
            // module automatically on exit, and the monitor is created once.
        }
    }

    unsafe fn load_symbol<T: Copy>(module: HMODULE, name: PCSTR) -> Result<T, String> {
        let symbol = unsafe { GetProcAddress(module, name) }
            .ok_or_else(|| format!("NVML function is unavailable: {:?}", name.as_ptr()))?;
        debug_assert_eq!(std::mem::size_of::<T>(), std::mem::size_of_val(&symbol));
        Ok(unsafe { std::mem::transmute_copy(&symbol) })
    }
}

#[cfg(windows)]
pub use platform::{GpuMemoryMonitor, MemoryInfo};

#[cfg(not(windows))]
#[derive(Clone, Copy, Debug)]
pub struct MemoryInfo {
    pub total: u64,
    pub free: u64,
    pub used: u64,
}

#[cfg(not(windows))]
pub struct GpuMemoryMonitor;

#[cfg(not(windows))]
impl GpuMemoryMonitor {
    pub fn new() -> Result<Self, String> {
        Err("GPU memory monitoring is only available on Windows".to_string())
    }

    pub fn memory_info(&self) -> Result<MemoryInfo, String> {
        Err("GPU memory monitoring is only available on Windows".to_string())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an NVIDIA Windows driver"]
    fn reads_vram_without_spawning_a_process() {
        let monitor = GpuMemoryMonitor::new().expect("NVML should initialize");
        let memory = monitor.memory_info().expect("VRAM query should succeed");
        assert!(memory.total > 0);
        assert!(memory.free <= memory.total);
        assert!(memory.used <= memory.total);
    }
}
