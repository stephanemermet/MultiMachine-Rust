use super::{platform::*, storage};
use anyhow::Result;
use multimachine_rust::model::*;
use serde::{Deserialize, Serialize};
use windows::{
    Win32::{Foundation::*, Graphics::Gdi::*, UI::WindowsAndMessaging::*},
    core::*,
};

pub fn desktop() -> Desktop {
    unsafe {
        unsafe extern "system" fn visit(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
            unsafe {
                let output = &mut *(data.0 as *mut Vec<Monitor>);
                let mut info = MONITORINFOEXW::default();
                info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
                if GetMonitorInfoW(m, (&mut info as *mut MONITORINFOEXW).cast()).as_bool() {
                    let r = info.monitorInfo.rcMonitor;
                    let n = info
                        .szDevice
                        .iter()
                        .position(|c| *c == 0)
                        .unwrap_or(info.szDevice.len());
                    output.push(Monitor {
                        name: String::from_utf16_lossy(&info.szDevice[..n]),
                        x: r.left,
                        y: r.top,
                        width: r.right - r.left,
                        height: r.bottom - r.top,
                    });
                }
                true.into()
            }
        }
        let mut monitors = Vec::new();
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(visit),
            LPARAM(&mut monitors as *mut _ as isize),
        );
        monitors.sort_by_key(|m: &Monitor| (m.x, m.y));
        Desktop {
            name: std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows".into()),
            monitors,
        }
    }
}
pub struct Halo {
    window: HWND,
}
impl Halo {
    pub fn new() -> Result<Self> {
        unsafe {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(halo_proc),
                hInstance: module(),
                lpszClassName: w!("MultiMachineRust.Halo"),
                ..Default::default()
            };
            RegisterClassW(&wc);
            let window = CreateWindowExW(
                WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE
                    | WS_EX_TRANSPARENT
                    | WS_EX_LAYERED,
                w!("MultiMachineRust.Halo"),
                w!(""),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(module()),
                None,
            )?;
            Ok(Self { window })
        }
    }
    pub fn show(&self, monitor: Option<Monitor>, right: bool) {
        unsafe {
            let Some(m) = monitor else {
                let _ = ShowWindow(self.window, SW_HIDE);
                return;
            };
            let width = 20.min(m.width);
            let height = m.height;
            let hdc = GetDC(None);
            let mem = CreateCompatibleDC(Some(hdc));
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut pixels = std::ptr::null_mut();
            if let Ok(bitmap) =
                CreateDIBSection(Some(hdc), &info, DIB_RGB_COLORS, &mut pixels, None, 0)
            {
                if !pixels.is_null() {
                    let data = std::slice::from_raw_parts_mut(
                        pixels.cast::<u8>(),
                        (width * height * 4) as usize,
                    );
                    for y in 0..height {
                        let gradient =
                            (y as f64 / (height.max(1)) as f64 * std::f64::consts::PI).sin();
                        for x in 0..width {
                            let distance = if right { width - 1 - x } else { x };
                            let alpha = if distance < 7 {
                                225.0
                            } else {
                                100.0 * ((20 - distance) as f64 / 13.0).powi(2)
                            } as u32;
                            let green = (115.0 + 100.0 * gradient) as u32;
                            let i = ((y * width + x) * 4) as usize;
                            data[i] = (255 * alpha / 255) as u8;
                            data[i + 1] = (green * alpha / 255) as u8;
                            data[i + 2] = (20 * alpha / 255) as u8;
                            data[i + 3] = alpha as u8;
                        }
                    }
                    let old = SelectObject(mem, bitmap.into());
                    let dest = POINT {
                        x: if right { m.right() - width } else { m.x },
                        y: m.y,
                    };
                    let size = SIZE {
                        cx: width,
                        cy: height,
                    };
                    let origin = POINT::default();
                    let blend = BLENDFUNCTION {
                        BlendOp: AC_SRC_OVER as u8,
                        SourceConstantAlpha: 255,
                        AlphaFormat: AC_SRC_ALPHA as u8,
                        BlendFlags: 0,
                    };
                    let _ = UpdateLayeredWindow(
                        self.window,
                        Some(hdc),
                        Some(&dest),
                        Some(&size),
                        Some(mem),
                        Some(&origin),
                        COLORREF(0),
                        Some(&blend),
                        ULW_ALPHA,
                    );
                    SelectObject(mem, old);
                    let _ = ShowWindow(self.window, SW_SHOWNOACTIVATE);
                    let _ = SetWindowPos(
                        self.window,
                        Some(HWND_TOPMOST),
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                }
                let _ = DeleteObject(bitmap.into());
            }
            let _ = DeleteDC(mem);
            ReleaseDC(None, hdc);
        }
    }
}
impl Drop for Halo {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.window);
        }
    }
}
unsafe extern "system" fn halo_proc(w: HWND, m: u32, p: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if m == WM_NCHITTEST {
            return LRESULT(HTTRANSPARENT as isize);
        }
        if m == WM_MOUSEACTIVATE {
            return LRESULT(MA_NOACTIVATE as isize);
        }
        DefWindowProcW(w, m, p, l)
    }
}
#[derive(Clone, Copy, Serialize, Deserialize)]
struct Saved {
    previous: u32,
    applied: u32,
}
pub struct Trails {
    saved: Option<Saved>,
}
impl Trails {
    pub fn new() -> Result<Self> {
        let saved = if storage::root().join("cursor-trails-restore.json").exists() {
            Some(storage::read("cursor-trails-restore.json", false)?)
        } else {
            None
        };
        let mut this = Self { saved };
        this.set(false)?;
        Ok(this)
    }
    fn read() -> Result<u32> {
        unsafe {
            let mut value = 0u32;
            SystemParametersInfoW(
                SPI_GETMOUSETRAILS,
                0,
                Some((&mut value as *mut u32).cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )?;
            Ok(value)
        }
    }
    fn write(value: u32) -> Result<()> {
        unsafe {
            SystemParametersInfoW(
                SPI_SETMOUSETRAILS,
                value,
                None,
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )?;
        }
        Ok(())
    }
    pub fn set(&mut self, remote: bool) -> Result<()> {
        if remote && self.saved.is_none() {
            let previous = Self::read()?;
            let saved = Saved {
                previous,
                applied: 7,
            };
            storage::save("cursor-trails-restore.json", &saved, false)?;
            self.saved = Some(saved);
            Self::write(saved.applied)?;
        }
        if !remote {
            if let Some(saved) = self.saved {
                if Self::read()? == saved.applied {
                    Self::write(saved.previous)?
                }
                let path = storage::root().join("cursor-trails-restore.json");
                if path.exists() {
                    std::fs::remove_file(path)?;
                }
                self.saved = None;
            }
        }
        Ok(())
    }
}
impl Drop for Trails {
    fn drop(&mut self) {
        let _ = self.set(false);
    }
}
