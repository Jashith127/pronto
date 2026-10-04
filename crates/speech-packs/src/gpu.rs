//! Display adapter detection, used to recommend a speech backend.
use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Other,
}

impl GpuVendor {
    pub fn from_pci_id(vendor: u32) -> Self {
        match vendor {
            0x10DE => GpuVendor::Nvidia,
            0x1002 | 0x1022 => GpuVendor::Amd,
            0x8086 => GpuVendor::Intel,
            _ => GpuVendor::Other,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuAdapter {
    pub name: String,
    pub vendor: GpuVendor,
    pub dedicated_mb: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuInfo {
    pub adapters: Vec<GpuAdapter>,
}

impl GpuInfo {
    pub fn has_nvidia(&self) -> bool {
        self.adapters.iter().any(|a| a.vendor == GpuVendor::Nvidia)
    }

    /// The adapter worth naming in the UI: NVIDIA first, then the one with the
    /// most dedicated memory.
    pub fn primary(&self) -> Option<&GpuAdapter> {
        self.adapters
            .iter()
            .find(|a| a.vendor == GpuVendor::Nvidia)
            .or_else(|| self.adapters.iter().max_by_key(|a| a.dedicated_mb))
    }
}

/// Hardware display adapters (software rasterizers excluded).
#[cfg(windows)]
pub fn detect_gpus() -> GpuInfo {
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
    };

    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else {
        return GpuInfo::default();
    };
    let mut adapters = Vec::new();
    let mut index = 0;
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(index) } {
        index += 1;
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
            continue;
        };
        // Microsoft Basic Render Driver and other WARP adapters.
        if desc.Flags & (DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0 || desc.VendorId == 0x1414 {
            continue;
        }
        let len = desc
            .Description
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(desc.Description.len());
        let name = String::from_utf16_lossy(&desc.Description[..len])
            .trim()
            .to_string();
        let entry = GpuAdapter {
            name,
            vendor: GpuVendor::from_pci_id(desc.VendorId),
            dedicated_mb: desc.DedicatedVideoMemory as u64 / (1024 * 1024),
        };
        // Hybrid laptops can list the same adapter once per output.
        if !adapters
            .iter()
            .any(|a: &GpuAdapter| a.name == entry.name && a.vendor == entry.vendor)
        {
            adapters.push(entry);
        }
    }
    GpuInfo { adapters }
}

#[cfg(not(windows))]
pub fn detect_gpus() -> GpuInfo {
    GpuInfo::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_ids_map_to_vendors() {
        assert_eq!(GpuVendor::from_pci_id(0x10DE), GpuVendor::Nvidia);
        assert_eq!(GpuVendor::from_pci_id(0x1002), GpuVendor::Amd);
        assert_eq!(GpuVendor::from_pci_id(0x8086), GpuVendor::Intel);
        assert_eq!(GpuVendor::from_pci_id(0x5143), GpuVendor::Other);
    }

    #[test]
    fn primary_prefers_nvidia_over_bigger_adapters() {
        let info = GpuInfo {
            adapters: vec![
                GpuAdapter {
                    name: "Radeon".into(),
                    vendor: GpuVendor::Amd,
                    dedicated_mb: 16000,
                },
                GpuAdapter {
                    name: "RTX".into(),
                    vendor: GpuVendor::Nvidia,
                    dedicated_mb: 6000,
                },
            ],
        };
        assert_eq!(info.primary().unwrap().name, "RTX");
    }
}
