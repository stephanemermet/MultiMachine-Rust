#![cfg(windows)]
// Run explicitly on an isolated desktop with MMR_UI_EXE pointing at the release
// executable. This checks the shipped resources and real HWNDs, not a mock layout.
use std::{
    process::{Child, Command},
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::LibraryLoader::*,
        UI::{HiDpi::*, WindowsAndMessaging::*},
    },
    core::*,
};
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
unsafe fn send(window: HWND, message: u32, w: usize, l: isize) -> usize {
    unsafe {
        let mut value = 0;
        assert_ne!(
            SendMessageTimeoutW(
                window,
                message,
                WPARAM(w),
                LPARAM(l),
                SMTO_ABORTIFHUNG,
                2000,
                Some(&mut value)
            )
            .0,
            0,
            "window message {message:#x} failed: {:?}",
            GetLastError()
        );
        value
    }
}
#[test]
#[ignore = "requires the release executable and an isolated Windows/Wine desktop"]
fn shipped_icons_and_responsive_window() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        assert!(
            FindWindowW(w!("MultiMachineRust.Main"), None).is_err(),
            "Close any existing test instance first"
        );
        let mut process = Process(
            Command::new(std::env::var("MMR_UI_EXE").expect("MMR_UI_EXE"))
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(15);
        let window = loop {
            if let Ok(window) = FindWindowW(w!("MultiMachineRust.Main"), None) {
                let mut pid = 0;
                GetWindowThreadProcessId(window, Some(&mut pid));
                assert_eq!(pid, process.0.id());
                if IsWindowVisible(window).as_bool() {
                    break window;
                }
            }
            assert!(Instant::now() < deadline, "window did not open");
            std::thread::sleep(Duration::from_millis(25));
        };
        std::thread::sleep(Duration::from_millis(300));
        let dpi = GetDpiForWindow(window);
        if let Ok(expected) = std::env::var("MMR_EXPECT_DPI") {
            assert_eq!(dpi, expected.parse::<u32>().unwrap());
        }
        // Wine 10 refuses cross-process WM_GETICON marshalling (pack_message).
        // Our in-process unit test checks the same Icons::load/apply path there.
        let wine = GetProcAddress(
            GetModuleHandleW(w!("ntdll.dll")).unwrap(),
            s!("wine_get_version"),
        )
        .is_some();
        for (kind, metric) in [(ICON_SMALL, SM_CXSMICON), (ICON_BIG, SM_CXICON)]
            .into_iter()
            .filter(|_| !wine)
        {
            let icon = HICON(send(window, WM_GETICON, kind as usize, 0) as *mut _);
            assert!(!icon.is_invalid());
            let mut info = ICONINFO::default();
            GetIconInfo(icon, &mut info).unwrap();
            let mut bitmap = BITMAP::default();
            assert_ne!(
                GetObjectW(
                    info.hbmColor.into(),
                    std::mem::size_of::<BITMAP>() as i32,
                    Some((&mut bitmap as *mut BITMAP).cast())
                ),
                0
            );
            assert_eq!(bitmap.bmWidth, GetSystemMetricsForDpi(metric, dpi));
            let _ = DeleteObject(info.hbmColor.into());
            let _ = DeleteObject(info.hbmMask.into());
        }
        assert!(GetWindowLongW(window, GWL_STYLE) as u32 & WS_THICKFRAME.0 != 0);
        for (width, height) in [(800, 650), (400, 420), (680, 620)] {
            SetWindowPos(
                window,
                None,
                10,
                10,
                width * dpi as i32 / 96,
                height * dpi as i32 / 96,
                SWP_NOZORDER,
            )
            .unwrap();
            std::thread::sleep(Duration::from_millis(75));
            let mut client = RECT::default();
            GetClientRect(window, &mut client).unwrap();
            for id in [
                10, 11, 12, 13, 14, 15, 16, 20, 21, 22, 23, 24, 25, 30, 31, 32, 33, 40, 41, 42, 43,
                44, 45,
            ] {
                let control = GetDlgItem(Some(window), id).unwrap();
                let mut r = RECT::default();
                GetWindowRect(control, &mut r).unwrap();
                let mut point = POINT {
                    x: r.left,
                    y: r.top,
                };
                assert!(ScreenToClient(window, &mut point).as_bool());
                assert!(
                    point.x >= 0 && point.x + r.right - r.left <= client.right,
                    "control {id} clipped horizontally at DPI {dpi}: {point:?}, {r:?}, {client:?}"
                );
            }
            // On a narrow window options must reflow vertically, not be clipped.
            if width == 400 {
                let mut a = RECT::default();
                let mut b = RECT::default();
                GetWindowRect(GetDlgItem(Some(window), 20).unwrap(), &mut a).unwrap();
                GetWindowRect(GetDlgItem(Some(window), 22).unwrap(), &mut b).unwrap();
                assert!(b.top >= a.bottom);
            }
            send(window, WM_VSCROLL, SB_BOTTOM.0 as usize, 0);
            let mut last = RECT::default();
            GetWindowRect(GetDlgItem(Some(window), 45).unwrap(), &mut last).unwrap();
            let mut bottom = POINT {
                x: last.right,
                y: last.bottom,
            };
            assert!(ScreenToClient(window, &mut bottom).as_bool());
            assert!(
                bottom.y <= client.bottom && bottom.y > 0,
                "footer inaccessible after scrolling"
            );
            send(window, WM_VSCROLL, SB_TOP.0 as usize, 0);
        }
        send(window, WM_THEMECHANGED, 0, 0);
        send(window, RegisterWindowMessageW(w!("TaskbarCreated")), 0, 0);
        // The window/process can disappear before the cross-process reply arrives.
        let _ = SendMessageTimeoutW(
            window,
            WM_COMMAND,
            WPARAM(102),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            2000,
            None,
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = process.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(Instant::now() < deadline, "shutdown blocked");
            std::thread::sleep(Duration::from_millis(25));
        }
        println!(
            "Responsive controls and scroll verified at {dpi} DPI; cross-process icons checked: {}",
            !wine
        );
    }
}
