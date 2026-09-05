use serde::Serialize;
use std::mem::{size_of, zeroed};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::ExtractIconExW;
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, GetIconInfo, GetWindowThreadProcessId, HICON, ICONINFO,
};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingIcon {
    pub width: u32,
    pub height: u32,
    /// 32-bit RGBA pixels, row-major, top row first.
    pub rgba: Vec<u8>,
}

/// Best-effort icon for the app owning `hwnd`: exe path -> first icon
/// resource -> RGBA bitmap. Anything failing yields None and the overlay
/// falls back to a bundled vendor glyph. No window content is touched.
pub fn icon_for_window(hwnd: HWND) -> Option<MeetingIcon> {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut path = vec![0u16; 1024];
        let mut length = path.len() as u32;
        let exe = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_FORMAT(0),
            PWSTR(path.as_mut_ptr()),
            &mut length,
        )
        .ok()
        .map(|_| String::from_utf16_lossy(&path[..length as usize]));
        let _ = CloseHandle(process);
        let exe = exe?;
        let exe_w: Vec<u16> = exe.encode_utf16().chain(std::iter::once(0)).collect();
        let mut large = HICON::default();
        let mut small = HICON::default();
        if ExtractIconExW(
            PCWSTR(exe_w.as_ptr()),
            0,
            Some(&mut large),
            Some(&mut small),
            1,
        ) == 0
        {
            return None;
        }
        let icon = if !large.is_invalid() {
            large
        } else {
            small
        };
        let result = if icon.is_invalid() {
            None
        } else {
            hicon_to_rgba(icon)
        };
        if !large.is_invalid() {
            let _ = DestroyIcon(large);
        }
        if !small.is_invalid() {
            let _ = DestroyIcon(small);
        }
        result
    }
}

fn hicon_to_rgba(icon: HICON) -> Option<MeetingIcon> {
    unsafe {
        let mut info = zeroed();
        GetIconInfo(icon, &mut info).ok()?;
        let result = hicon_info_to_rgba(&info);
        let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
        let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
        result
    }
}

fn hicon_info_to_rgba(info: &ICONINFO) -> Option<MeetingIcon> {
    unsafe {
        let mut bm: BITMAP = zeroed();
        if GetObjectW(
            info.hbmColor.into(),
            size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut BITMAP as *mut std::ffi::c_void),
        ) == 0
        {
            return None;
        }
        let width = bm.bmWidth;
        let height = bm.bmHeight.abs();
        if width <= 0 || height <= 0 || width > 256 || height > 256 {
            return None;
        }
        let hdc: HDC = GetDC(None);
        if hdc.is_invalid() {
            return None;
        }
        let pixels = read_bitmap_bits(hdc, info.hbmColor, width, height)?;
        let mask = read_bitmap_bits(hdc, info.hbmMask, width, height)?;
        ReleaseDC(None, hdc);
        // DIBs come back BGRA; the mask decides transparency. Where the
        // color bitmap already carries alpha (Vista+ PNG icons), keep it.
        let mut rgba = Vec::with_capacity(pixels.len());
        for (pixel, mask_pixel) in pixels.chunks_exact(4).zip(mask.chunks_exact(4)) {
            let transparent = mask_pixel[0] != 0 || mask_pixel[1] != 0 || mask_pixel[2] != 0;
            let alpha = if transparent {
                0
            } else if pixel[3] != 0 {
                pixel[3]
            } else {
                255
            };
            rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], alpha]);
        }
        Some(MeetingIcon {
            width: width as u32,
            height: height as u32,
            rgba,
        })
    }
}

fn read_bitmap_bits(hdc: HDC, bitmap: HBITMAP, width: i32, height: i32) -> Option<Vec<u8>> {
    unsafe {
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                // Negative height => top-down rows, matching canvas order.
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [zeroed()],
        };
        let mut bits = vec![0u8; (width as usize) * (height as usize) * 4];
        let lines = GetDIBits(
            hdc,
            bitmap,
            0,
            height as u32,
            Some(bits.as_mut_ptr() as *mut std::ffi::c_void),
            &mut info,
            DIB_RGB_COLORS,
        );
        if lines == 0 {
            return None;
        }
        Some(bits)
    }
}

#[cfg(test)]
mod tests {
    // Pure pixel math (mask + BGRA -> RGBA) exercised without a window.
    fn compose(color: &[u8], mask: &[u8]) -> Vec<u8> {
        let mut rgba = Vec::new();
        for (pixel, mask_pixel) in color.chunks_exact(4).zip(mask.chunks_exact(4)) {
            let transparent = mask_pixel[0] != 0 || mask_pixel[1] != 0 || mask_pixel[2] != 0;
            let alpha = if transparent {
                0
            } else if pixel[3] != 0 {
                pixel[3]
            } else {
                255
            };
            rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], alpha]);
        }
        rgba
    }

    #[test]
    fn compose_prefers_mask_transparency_then_native_alpha() {
        // Opaque red, empty mask, no native alpha -> opaque.
        assert_eq!(
            compose(&[0, 0, 255, 0], &[0, 0, 0, 0]),
            vec![255, 0, 0, 255]
        );
        // Mask set -> transparent even with native alpha present.
        assert_eq!(
            compose(&[0, 0, 255, 200], &[255, 255, 255, 0]),
            vec![255, 0, 0, 0]
        );
        // Native alpha preserved when the mask is clear.
        assert_eq!(
            compose(&[0, 128, 0, 128], &[0, 0, 0, 0]),
            vec![0, 128, 0, 128]
        );
    }
}
