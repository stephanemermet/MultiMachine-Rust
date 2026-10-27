use multimachine_rust::model::*;
use std::{
    sync::{Arc, mpsc},
    thread,
};
use windows::{
    Win32::{
        Foundation::*,
        System::{LibraryLoader::*, Ole::*, Threading::*},
        UI::WindowsAndMessaging::*,
    },
    core::*,
};
pub type Job = Box<dyn FnOnce() + Send + 'static>;
#[derive(Clone)]
pub struct Sta {
    tx: mpsc::Sender<Job>,
    id: u32,
}
impl Sta {
    pub fn spawn(name: &str, init: impl FnOnce(Sta) + Send + 'static) -> anyhow::Result<Self> {
        let (tx, rx) = mpsc::channel::<Job>();
        let (ready, wait) = mpsc::sync_channel(1);
        let send = tx.clone();
        thread::Builder::new()
            .name(name.into())
            .spawn(move || unsafe {
                if OleInitialize(None).is_err() {
                    let _ = ready.send(Err("Initialisation OLE impossible"));
                    return;
                }
                let mut message = MSG::default();
                let _ = PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE);
                let sta = Sta {
                    tx: send,
                    id: GetCurrentThreadId(),
                };
                let _ = ready.send(Ok(sta.clone()));
                init(sta);
                while GetMessageW(&mut message, None, 0, 0).0 > 0 {
                    if message.message == WM_APP {
                        while let Ok(job) = rx.try_recv() {
                            job();
                        }
                    } else {
                        let _ = TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                }
                OleUninitialize();
            })?;
        wait.recv()?.map_err(anyhow::Error::msg)
    }
    pub fn post(&self, job: impl FnOnce() + Send + 'static) {
        if self.tx.send(Box::new(job)).is_ok() {
            unsafe {
                let _ = PostThreadMessageW(self.id, WM_APP, WPARAM(0), LPARAM(0));
            }
        }
    }
    pub fn stop(&self) {
        self.post(|| unsafe { PostQuitMessage(0) });
    }
    pub fn id(&self) -> u32 {
        self.id
    }
}
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
pub fn text(window: HWND) -> String {
    unsafe {
        let n = GetWindowTextLengthW(window).clamp(0, 65536) as usize;
        let mut b = vec![0; n + 1];
        let n = GetWindowTextW(window, &mut b) as usize;
        String::from_utf16_lossy(&b[..n])
    }
}
pub fn set_text(hwnd: HWND, s: &str) {
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR(wide(s).as_ptr()));
    }
}
pub fn cursor() -> Point {
    unsafe {
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        Point::new(p.x, p.y)
    }
}
pub fn left() -> bool {
    unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(1) < 0 }
}
pub fn module() -> HINSTANCE {
    unsafe { GetModuleHandleW(None).unwrap_or_default().into() }
}
pub unsafe fn hidden(class: &str, proc: WNDPROC) -> Result<HWND> {
    unsafe {
        let name = wide(class);
        let wc = WNDCLASSW {
            lpfnWndProc: proc,
            hInstance: module(),
            lpszClassName: PCWSTR(name.as_ptr()),
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(name.as_ptr()),
            w!(""),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(module()),
            None,
        )
    }
}
pub type Notice = Arc<dyn Fn(String) + Send + Sync>;
