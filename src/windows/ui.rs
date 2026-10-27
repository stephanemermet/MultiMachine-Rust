use super::{
    clipboard::Clipboard,
    display::{self, Halo, Trails},
    input::InputThread,
    layout_dialog,
    platform::*,
    storage,
};
use anyhow::{Result, ensure};
use multimachine_rust::{
    files::Catalog,
    layout::{self, Layout},
    model::*,
    network::{self, Identity, Peer, Trust},
};
use parking_lot::Mutex;
use std::{
    cell::RefCell,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};
use tokio::{runtime::Handle, sync::oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::{Ole::*, RemoteDesktop::*, Threading::*},
        UI::{Controls::SetScrollInfo, HiDpi::*, Shell::*, WindowsAndMessaging::*},
    },
    core::*,
};
type Job = Box<dyn FnOnce(&mut App) + Send>;
#[derive(Clone)]
struct Ui {
    tx: mpsc::Sender<Job>,
    window: isize,
}
impl Ui {
    fn post(&self, job: impl FnOnce(&mut App) + Send + 'static) {
        if self.tx.send(Box::new(job)).is_ok() {
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(self.window as *mut _)),
                    WM_APP,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
    }
}
struct Session {
    id: Uuid,
    stop: CancellationToken,
    allow_pair: Arc<AtomicU64>,
}
struct App {
    window: HWND,
    ui: Ui,
    rx: mpsc::Receiver<Job>,
    settings: Settings,
    runtime: Handle,
    catalog: Arc<Catalog>,
    input: InputThread,
    clipboard: Clipboard,
    session: Option<Session>,
    peer: Option<Arc<Peer>>,
    local: Desktop,
    remote: Option<Desktop>,
    halo: Halo,
    trails: Trails,
    receiving: bool,
    was_running: bool,
    identity: Identity,
    trust: Option<Trust>,
    controls: std::collections::HashMap<usize, HWND>,
    widgets: Vec<(usize, HWND)>,
    fonts: Vec<HFONT>,
    icons: Icons,
    dpi: u32,
    scroll: i32,
    content_height: i32,
    tray: bool,
    notice: String,
    topology_ticks: u32,
    closing: bool,
}
thread_local! {static APP:RefCell<Option<App>>=const{RefCell::new(None)};}
const ROLE: usize = 10;
const HOST: usize = 11;
const PORTID: usize = 12;
const DISCOVER: usize = 13;
const START: usize = 14;
const SCREENS: usize = 15;
const PAIR: usize = 16;
const HYBRID: usize = 20;
const PLAIN: usize = 21;
const TRAILS: usize = 22;
const AUTOSTART: usize = 23;
const AUTO: usize = 24;
const MINIMIZED: usize = 25;
const STATUS: usize = 30;
const TRANSPORT: usize = 31;
const HIDE: usize = 32;
const LOGS: usize = 33;
const TITLE: usize = 40;
const SUBTITLE: usize = 41;
const ROLE_LABEL: usize = 42;
const HOST_LABEL: usize = 43;
const PORT_LABEL: usize = 44;
const SHORTCUT: usize = 45;
const TRAY_MESSAGE: u32 = WM_APP + 2;
const REDRAW_STATUS: u32 = WM_APP + 3;
const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY;
const MAIN_STYLE: WINDOW_STYLE = WS_OVERLAPPEDWINDOW;
const MAIN_EX_STYLE: WINDOW_EX_STYLE = WS_EX_APPWINDOW;

struct Icons {
    small: HICON,
    large: HICON,
}
impl Icons {
    fn load(dpi: u32) -> Result<Self> {
        unsafe {
            let load = |x, y| {
                LoadImageW(
                    Some(module()),
                    w!("MMR_ICON"),
                    IMAGE_ICON,
                    GetSystemMetricsForDpi(x, dpi),
                    GetSystemMetricsForDpi(y, dpi),
                    LR_DEFAULTCOLOR,
                )
                .map(|h| HICON(h.0))
            };
            let small = load(SM_CXSMICON, SM_CYSMICON)?;
            match load(SM_CXICON, SM_CYICON) {
                Ok(large) => Ok(Self { small, large }),
                Err(error) => {
                    let _ = DestroyIcon(small);
                    Err(error.into())
                }
            }
        }
    }
    fn apply(&self, window: HWND) {
        unsafe {
            SendMessageW(
                window,
                WM_SETICON,
                Some(WPARAM(ICON_BIG as usize)),
                Some(LPARAM(self.large.0 as isize)),
            );
            SendMessageW(
                window,
                WM_SETICON,
                Some(WPARAM(ICON_SMALL as usize)),
                Some(LPARAM(self.small.0 as isize)),
            );
        }
    }
}
impl Drop for Icons {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyIcon(self.small);
            let _ = DestroyIcon(self.large);
        }
    }
}
fn outer_size(width: i32, height: i32, dpi: u32) -> (i32, i32) {
    let mut r = RECT {
        right: width,
        bottom: height,
        ..Default::default()
    };
    unsafe {
        let _ = AdjustWindowRectExForDpi(&mut r, MAIN_STYLE, false, MAIN_EX_STYLE, dpi);
    }
    (r.right - r.left, r.bottom - r.top)
}

pub fn run() -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        OleInitialize(None)?;
        let mutex = CreateMutexW(None, false, w!("Local\\MultiMachineRust.Application"))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let w = FindWindowW(w!("MultiMachineRust.Main"), None)?;
            let _ = ShowWindow(w, SW_RESTORE);
            let _ = SetForegroundWindow(w);
            let _ = CloseHandle(mutex);
            OleUninitialize();
            return Ok(());
        }
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: module(),
            lpszClassName: w!("MultiMachineRust.Main"),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hIcon: LoadIconW(Some(module()), w!("MMR_ICON"))?,
            hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as *mut _),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let window = CreateWindowExW(
            MAIN_EX_STYLE,
            w!("MultiMachineRust.Main"),
            w!("MultiMachine Rust"),
            MAIN_STYLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            700,
            600,
            None,
            None,
            Some(module()),
            None,
        )?;
        let (tx, rx) = mpsc::channel();
        let ui = Ui {
            tx,
            window: window.0 as isize,
        };
        let dest = ui.clone();
        let notice: Notice = Arc::new(move |text| dest.post(move |a| a.status(&text)));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(3)
            .enable_all()
            .thread_name("MMR network")
            .build()?;
        let catalog = Arc::new(Catalog::default());
        let clipboard = Clipboard::new(catalog.clone(), runtime.handle().clone(), notice.clone())?;
        let dest = ui.clone();
        let indicator = Arc::new(move |m: Option<Monitor>, right, receiving| {
            dest.post(move |a| {
                a.receiving = receiving;
                a.halo.show(m, right);
                if a.trails.set(a.settings.trails && receiving).is_err() {
                    a.status("Windows n’a pas pu modifier les traces du curseur.");
                }
            })
        });
        let input = InputThread::new(catalog.clone(), runtime.handle().clone(), notice, indicator)?;
        let settings = storage::settings()?;
        let identity = storage::identity()?;
        let trust = storage::trust()?;
        let dpi = GetDpiForWindow(window).max(96);
        let icons = Icons::load(dpi)?;
        let mut app = App {
            window,
            ui,
            rx,
            settings,
            runtime: runtime.handle().clone(),
            catalog,
            input,
            clipboard,
            session: None,
            peer: None,
            local: display::desktop(),
            remote: None,
            halo: Halo::new()?,
            trails: Trails::new()?,
            receiving: false,
            was_running: false,
            identity,
            trust,
            controls: Default::default(),
            widgets: vec![],
            fonts: vec![],
            icons,
            dpi,
            scroll: 0,
            content_height: 0,
            tray: false,
            notice: "Prêt. Choisissez le rôle de cette machine.".into(),
            topology_ticks: 0,
            closing: false,
        };
        app.create_controls();
        app.rescale();
        app.fill();
        app.fit_window();
        app.tray(true);
        let auto = app.settings.auto_connect;
        let minimized = app.settings.minimized || std::env::args().any(|s| s == "--startup");
        APP.with(|a| *a.borrow_mut() = Some(app));
        let _ = WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION);
        SetTimer(Some(window), 1, 2000, None);
        if !minimized || !app_tray_available() {
            let _ = ShowWindow(window, SW_SHOW);
        }
        if auto {
            with(|a| a.start());
        }
        storage::log("application.start");
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            if !IsDialogMessageW(window, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        APP.with(|a| {
            a.borrow_mut().take();
        });
        runtime.shutdown_timeout(Duration::from_secs(2));
        let _ = CloseHandle(mutex);
        OleUninitialize();
        Ok(())
    }
}
fn app_tray_available() -> bool {
    APP.with(|a| a.borrow().as_ref().is_some_and(|a| a.tray))
}
fn with(f: impl FnOnce(&mut App)) {
    APP.with(|a| {
        if let Ok(mut a) = a.try_borrow_mut() {
            if let Some(a) = a.as_mut() {
                f(a)
            }
        }
    });
}
unsafe extern "system" fn proc(w: HWND, m: u32, p: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        // Dialogs and OLE can re-enter this window; pending jobs remain queued until the
        // outer handler returns instead of attempting a second mutable state borrow.
        match m {
            WM_APP => {
                with(|a| {
                    while let Ok(job) = a.rx.try_recv() {
                        job(a)
                    }
                });
                return LRESULT(0);
            }
            WM_TIMER => {
                with(|a| {
                    while let Ok(job) = a.rx.try_recv() {
                        job(a)
                    }
                    a.tick();
                });
                return LRESULT(0);
            }
            WM_COMMAND => {
                with(|a| a.command(p.0 & 0xffff, (p.0 >> 16) as u16));
                return LRESULT(0);
            }
            WM_CLOSE => {
                with(|a| a.hide());
                return LRESULT(0);
            }
            WM_SIZE if p.0 == SIZE_MINIMIZED as usize => {
                with(|a| {
                    if a.tray {
                        let _ = ShowWindow(w, SW_HIDE);
                    }
                });
                return LRESULT(0);
            }
            WM_SIZE => {
                with(|a| a.layout());
                return LRESULT(0);
            }
            WM_GETMINMAXINFO => {
                let dpi = GetDpiForWindow(w).max(96);
                let (width, height) = outer_size(360 * dpi as i32 / 96, 240 * dpi as i32 / 96, dpi);
                let limits = &mut *(l.0 as *mut MINMAXINFO);
                limits.ptMinTrackSize = POINT {
                    x: width,
                    y: height,
                };
                return LRESULT(0);
            }
            WM_VSCROLL => {
                with(|a| a.scroll(p.0 as u32 & 0xffff));
                return LRESULT(0);
            }
            WM_MOUSEWHEEL => {
                with(|a| {
                    a.scroll -= ((p.0 >> 16) as i16 as i32) * 48 * a.dpi as i32 / (120 * 96);
                    a.layout();
                });
                return LRESULT(0);
            }
            WM_SHOWWINDOW if p.0 != 0 => {
                let _ = PostMessageW(Some(w), REDRAW_STATUS, WPARAM(0), LPARAM(0));
            }
            REDRAW_STATUS => {
                with(|a| {
                    a.render_status();
                    a.layout();
                });
                return LRESULT(0);
            }
            WM_DISPLAYCHANGE | WM_DEVICECHANGE => {
                with(|a| {
                    a.input.hardware();
                    a.refresh_topology();
                });
                return LRESULT(0);
            }
            WM_WTSSESSION_CHANGE => {
                with(|a| {
                    if p.0 == WTS_SESSION_LOCK as usize {
                        a.was_running = a.session.is_some();
                        a.stop();
                        a.status("Session verrouillée.");
                    } else if p.0 == WTS_SESSION_UNLOCK as usize && a.was_running {
                        a.was_running = false;
                        a.start();
                    }
                });
                return LRESULT(0);
            }
            WM_POWERBROADCAST => {
                with(|a| {
                    if p.0 == 4 {
                        a.was_running = a.session.is_some();
                        a.stop();
                    } else if [7, 18].contains(&p.0) && a.was_running {
                        a.was_running = false;
                        a.start();
                    }
                });
                return LRESULT(1);
            }
            WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
                SetBkColor(HDC(p.0 as *mut _), COLORREF(GetSysColor(COLOR_WINDOW)));
                SetTextColor(HDC(p.0 as *mut _), COLORREF(GetSysColor(COLOR_WINDOWTEXT)));
                return LRESULT(GetSysColorBrush(COLOR_WINDOW).0 as isize);
            }
            WM_ENDSESSION if p.0 != 0 => {
                with(|a| a.quit());
                return LRESULT(0);
            }
            WM_DPICHANGED => {
                let r = &*(l.0 as *const RECT);
                let _ = SetWindowPos(
                    w,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                with(|a| a.rescale());
                return LRESULT(0);
            }
            WM_THEMECHANGED | WM_SETTINGCHANGE => {
                with(|a| a.rescale());
            }
            TRAY_MESSAGE => {
                with(|a| match l.0 as u32 & 0xffff {
                    WM_LBUTTONDBLCLK | WM_LBUTTONUP | NIN_SELECT | NIN_KEYSELECT => {
                        let _ = ShowWindow(w, SW_RESTORE);
                        let _ = SetForegroundWindow(w);
                    }
                    WM_RBUTTONUP | WM_CONTEXTMENU => a.menu(),
                    _ => (),
                });
                return LRESULT(0);
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                return LRESULT(0);
            }
            _ => {
                let restored = RegisterWindowMessageW(w!("TaskbarCreated"));
                if m == restored {
                    with(|a| {
                        a.tray = false;
                        a.tray(true)
                    });
                    return LRESULT(0);
                }
            }
        }
        DefWindowProcW(w, m, p, l)
    }
}
impl App {
    fn rescale(&mut self) {
        unsafe {
            let dpi = GetDpiForWindow(self.window).max(96);
            let font = |height: i32, weight| {
                CreateFontW(
                    -height * dpi as i32 / 96,
                    0,
                    0,
                    0,
                    weight,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET,
                    OUT_DEFAULT_PRECIS,
                    CLIP_DEFAULT_PRECIS,
                    CLEARTYPE_QUALITY,
                    0,
                    w!("Segoe UI"),
                )
            };
            let body = font(15, 400);
            let title = font(22, 600);
            for (id, child) in &self.widgets {
                SendMessageW(
                    *child,
                    WM_SETFONT,
                    Some(WPARAM(if *id == TITLE { title.0 } else { body.0 } as usize)),
                    Some(LPARAM(1)),
                );
            }
            for font in self.fonts.drain(..) {
                let _ = DeleteObject(font.into());
            }
            self.fonts = vec![body, title];
            if self.dpi != dpi {
                if let Ok(icons) = Icons::load(dpi) {
                    icons.apply(self.window);
                    self.icons = icons;
                    if self.tray {
                        self.tray(true);
                    }
                }
            } else {
                self.icons.apply(self.window);
            }
            self.dpi = dpi;
            self.layout();
        }
    }
    fn fit_window(&mut self) {
        unsafe {
            // Size the client area with this window's actual monitor DPI. System
            // DPI can differ on the first launch on a secondary monitor.
            let mut monitor = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !GetMonitorInfoW(
                MonitorFromWindow(self.window, MONITOR_DEFAULTTONEAREST),
                &mut monitor,
            )
            .as_bool()
            {
                return;
            }
            let work = monitor.rcWork;
            let mut previous = RECT::default();
            let _ = GetWindowRect(self.window, &mut previous);
            let scale = self.dpi as i32;
            let (width, height) = outer_size(680 * scale / 96, 580 * scale / 96, self.dpi);
            let width = width.min(work.right - work.left);
            let height = height.min(work.bottom - work.top);
            let x = previous.left.clamp(work.left, work.right - width);
            let y = previous.top.clamp(work.top, work.bottom - height);
            let _ = SetWindowPos(
                self.window,
                None,
                x,
                y,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            self.layout();
            let (_, fitted) = outer_size(0, self.content_height, self.dpi);
            let height = fitted.min(work.bottom - y);
            let _ = SetWindowPos(
                self.window,
                None,
                x,
                y,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            self.layout();
        }
    }
    fn layout(&mut self) {
        if self.fonts.is_empty() {
            return;
        }
        unsafe {
            let s = |n: i32| (n * self.dpi as i32 + 48) / 96;
            let dc = GetDC(Some(self.window));
            let previous_font = SelectObject(dc, self.fonts[0].into());
            let measured = |id: usize, width: i32| {
                let mut chars = wide(&text(self.controls[&id]));
                chars.pop();
                let mut r = RECT {
                    right: width.max(1),
                    ..Default::default()
                };
                DrawTextW(
                    dc,
                    &mut chars,
                    &mut r,
                    DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
                );
                (r.right, r.bottom)
            };
            // Showing or hiding the vertical scrollbar can change client width.
            // Recompute before placing children, including after a DPI change.
            for _ in 0..3 {
                let mut client = RECT::default();
                let _ = GetClientRect(self.window, &mut client);
                let margin = s(20);
                let gap = s(12);
                let width = (client.right - 2 * margin).max(s(100));
                let wide_form = width >= s(600);
                let row = s(32);
                let mut positions = Vec::new();
                let mut place = |id, x, y, w, h| positions.push((id, x, y, w, h));
                let mut y = s(16);
                place(TITLE, margin, y, width, s(30));
                y += s(34);
                place(SUBTITLE, margin, y, width, s(24));
                y += s(38);
                let label = s(150);
                if wide_form {
                    place(ROLE_LABEL, margin, y + s(4), label, row);
                    place(ROLE, margin + label, y, width - label, s(150));
                    y += row + s(10);
                    place(HOST_LABEL, margin, y + s(4), label, row);
                    place(HOST, margin + label, y, width - label - s(125) - gap, row);
                    place(DISCOVER, margin + width - s(125), y, s(125), row);
                    y += row + s(10);
                    place(PORT_LABEL, margin, y + s(4), label, row);
                    place(PORTID, margin + label, y, s(90), row);
                    let left = margin + label + s(90) + gap;
                    let button = (margin + width - left - gap) / 2;
                    place(START, left, y, button, row);
                    place(SCREENS, left + button + gap, y, button, row);
                    y += row + s(20);
                } else {
                    place(ROLE_LABEL, margin, y, width, s(24));
                    y += s(25);
                    place(ROLE, margin, y, width, s(150));
                    y += row + s(10);
                    place(HOST_LABEL, margin, y, width, s(24));
                    y += s(25);
                    place(HOST, margin, y, width - s(105) - gap, row);
                    place(DISCOVER, margin + width - s(105), y, s(105), row);
                    y += row + s(10);
                    place(PORT_LABEL, margin, y + s(4), label, row);
                    place(PORTID, margin + label, y, (width - label).min(s(110)), row);
                    y += row + gap;
                    let button = (width - gap) / 2;
                    place(START, margin, y, button, row);
                    place(SCREENS, margin + button + gap, y, button, row);
                    y += row + s(20);
                }
                let pairs = [(HYBRID, TRAILS), (PLAIN, AUTO), (AUTOSTART, MINIMIZED)];
                let column = (width - gap) / 2;
                let two_columns = pairs.iter().all(|(a, b)| {
                    [a, b]
                        .iter()
                        .all(|id| measured(**id, 100_000).0 + s(30) <= column)
                });
                for (a, b) in pairs {
                    if two_columns {
                        place(a, margin, y, column, s(28));
                        place(b, margin + column + gap, y, column, s(28));
                        y += s(33);
                    } else {
                        for id in [a, b] {
                            let height = (measured(id, width - s(30)).1 + s(8)).max(s(28));
                            place(id, margin, y, width, height);
                            y += height + s(5);
                        }
                    }
                }
                y += s(12);
                let buttons = [PAIR, LOGS, HIDE];
                let thirds = (width - 2 * gap) / 3;
                if buttons
                    .iter()
                    .all(|id| measured(*id, 100_000).0 + s(22) <= thirds)
                {
                    for (i, id) in buttons.into_iter().enumerate() {
                        place(id, margin + i as i32 * (thirds + gap), y, thirds, row);
                    }
                    y += row + s(18);
                } else {
                    for id in buttons {
                        place(id, margin, y, width, row);
                        y += row + s(8);
                    }
                    y += s(10);
                }
                let height = (measured(STATUS, width).1 + s(6)).max(s(48));
                place(STATUS, margin, y, width, height);
                y += height + s(10);
                place(TRANSPORT, margin, y, width, s(44));
                y += s(48);
                let height = (measured(SHORTCUT, width).1 + s(6)).max(s(24));
                place(SHORTCUT, margin, y, width, height);
                y += height + margin;
                self.content_height = y;
                self.scroll = self.scroll.clamp(0, (y - client.bottom).max(0));
                let info = SCROLLINFO {
                    cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                    fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
                    nMin: 0,
                    nMax: y - 1,
                    nPage: client.bottom.max(0) as u32,
                    nPos: self.scroll,
                    ..Default::default()
                };
                SetScrollInfo(self.window, SB_VERT, &info, true);
                let mut after = RECT::default();
                let _ = GetClientRect(self.window, &mut after);
                if after.right != client.right {
                    continue;
                }
                for (id, x, y, w, h) in positions {
                    let _ = SetWindowPos(
                        self.controls[&id],
                        None,
                        x,
                        y - self.scroll,
                        w,
                        h,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                break;
            }
            SelectObject(dc, previous_font);
            ReleaseDC(Some(self.window), dc);
            let _ = InvalidateRect(Some(self.window), None, true);
        }
    }
    fn scroll(&mut self, command: u32) {
        unsafe {
            let mut client = RECT::default();
            let _ = GetClientRect(self.window, &mut client);
            let line = 32 * self.dpi as i32 / 96;
            match command {
                n if n == SB_LINEUP.0 as u32 => self.scroll -= line,
                n if n == SB_LINEDOWN.0 as u32 => self.scroll += line,
                n if n == SB_PAGEUP.0 as u32 => self.scroll -= client.bottom - line,
                n if n == SB_PAGEDOWN.0 as u32 => self.scroll += client.bottom - line,
                n if n == SB_TOP.0 as u32 => self.scroll = 0,
                n if n == SB_BOTTOM.0 as u32 => self.scroll = self.content_height,
                n if n == SB_THUMBTRACK.0 as u32 || n == SB_THUMBPOSITION.0 as u32 => {
                    let mut info = SCROLLINFO {
                        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                        fMask: SIF_TRACKPOS,
                        ..Default::default()
                    };
                    if GetScrollInfo(self.window, SB_VERT, &mut info).is_ok() {
                        self.scroll = info.nTrackPos;
                    }
                }
                _ => return,
            }
            self.layout();
        }
    }
    fn create_controls(&mut self) {
        unsafe {
            let specs = [
                ("STATIC", "MultiMachine Rust", 0, TITLE),
                ("STATIC", "Deux PC, un espace de travail", 0, SUBTITLE),
                ("STATIC", "Cette machine", 0, ROLE_LABEL),
                ("COMBOBOX", "", CBS_DROPDOWNLIST as u32, ROLE),
                ("STATIC", "Adresse du serveur", 0, HOST_LABEL),
                ("EDIT", "", WS_BORDER.0 | ES_AUTOHSCROLL as u32, HOST),
                ("BUTTON", "Détecter", BS_PUSHBUTTON as u32, DISCOVER),
                ("STATIC", "Port", 0, PORT_LABEL),
                ("EDIT", "", WS_BORDER.0 | ES_NUMBER as u32, PORTID),
                ("BUTTON", "Démarrer", BS_DEFPUSHBUTTON as u32, START),
                ("BUTTON", "Écrans…", BS_PUSHBUTTON as u32, SCREENS),
                (
                    "BUTTON",
                    "Mouvement hybride UDP + TCP",
                    (BS_AUTOCHECKBOX | BS_MULTILINE) as u32,
                    HYBRID,
                ),
                (
                    "BUTTON",
                    "Traces du curseur distant",
                    (BS_AUTOCHECKBOX | BS_MULTILINE) as u32,
                    TRAILS,
                ),
                (
                    "BUTTON",
                    "Texte sans mise en forme",
                    (BS_AUTOCHECKBOX | BS_MULTILINE) as u32,
                    PLAIN,
                ),
                (
                    "BUTTON",
                    "Connexion automatique",
                    (BS_AUTOCHECKBOX | BS_MULTILINE) as u32,
                    AUTO,
                ),
                (
                    "BUTTON",
                    "Démarrer avec Windows",
                    (BS_AUTOCHECKBOX | BS_MULTILINE) as u32,
                    AUTOSTART,
                ),
                (
                    "BUTTON",
                    "Démarrer réduit",
                    (BS_AUTOCHECKBOX | BS_MULTILINE) as u32,
                    MINIMIZED,
                ),
                ("BUTTON", "Nouvelle association", BS_PUSHBUTTON as u32, PAIR),
                ("BUTTON", "Diagnostics", BS_PUSHBUTTON as u32, LOGS),
                (
                    "BUTTON",
                    "Réduire près de l’horloge",
                    BS_PUSHBUTTON as u32,
                    HIDE,
                ),
                ("STATIC", "", 0, STATUS),
                ("STATIC", "Connexion arrêtée", 0, TRANSPORT),
                (
                    "STATIC",
                    "Ctrl + Alt + F12 : reprendre la main",
                    0,
                    SHORTCUT,
                ),
            ];
            for (class, label, style, id) in specs {
                let c = layout_dialog::control(self.window, class, label, style, 0, 0, 1, 1, id);
                self.widgets.push((id, c));
                self.controls.insert(id, c);
            }
            for name in [
                "Serveur · accepte la connexion",
                "Client · rejoint le serveur",
            ] {
                SendMessageW(
                    self.controls[&ROLE],
                    CB_ADDSTRING,
                    None,
                    Some(LPARAM(wide(name).as_ptr() as isize)),
                );
            }
        }
    }
    fn fill(&self) {
        unsafe {
            SendMessageW(
                self.controls[&ROLE],
                CB_SETCURSEL,
                Some(WPARAM(if self.settings.server { 0 } else { 1 })),
                None,
            );
            set_text(self.controls[&HOST], &self.settings.host);
            set_text(self.controls[&PORTID], &self.settings.port.to_string());
            for (id, value) in [
                (HYBRID, self.settings.hybrid),
                (PLAIN, self.settings.plain),
                (TRAILS, self.settings.trails),
                (AUTOSTART, self.settings.auto_start),
                (AUTO, self.settings.auto_connect),
                (MINIMIZED, self.settings.minimized),
            ] {
                SendMessageW(
                    self.controls[&id],
                    BM_SETCHECK,
                    Some(WPARAM(if value { 1 } else { 0 })),
                    None,
                );
            }
            set_text(self.controls[&STATUS], &self.notice);
        }
    }
    fn read_form(&mut self) -> Result<()> {
        unsafe {
            let checked = |id| SendMessageW(self.controls[&id], BM_GETCHECK, None, None).0 == 1;
            self.settings.server =
                SendMessageW(self.controls[&ROLE], CB_GETCURSEL, None, None).0 == 0;
            self.settings.host = text(self.controls[&HOST]).trim().to_owned();
            self.settings.port = text(self.controls[&PORTID]).parse()?;
            self.settings.hybrid = checked(HYBRID);
            self.settings.plain = checked(PLAIN);
            self.settings.trails = checked(TRAILS);
            self.settings.auto_start = checked(AUTOSTART);
            self.settings.auto_connect = checked(AUTO);
            self.settings.minimized = checked(MINIMIZED);
            self.settings.validate()?;
            storage::autostart(self.settings.auto_start)?;
            storage::save("settings.json", &self.settings, false)?;
            self.trails.set(self.settings.trails && self.receiving)?;
            Ok(())
        }
    }
    fn status(&mut self, text: &str) {
        self.notice = text.to_owned();
        if unsafe { IsWindowVisible(self.window).as_bool() } {
            self.render_status();
            self.layout();
        }
    }
    fn command(&mut self, id: usize, notification: u16) {
        match id {
            START => {
                if self.session.is_some() {
                    self.stop()
                } else {
                    self.start()
                }
            }
            HIDE => self.hide(),
            LOGS => unsafe {
                if let Some(peer) = &self.peer {
                    let (q, c, u, received, rtt) = peer.stats();
                    storage::log(&format!(
                        "diagnostics.requested queue={q} coalesced={c} udp_sent={u} udp_received={received} tcp_rtt_ms={rtt:?}"
                    ));
                }
                let path = storage::root();
                let _ = std::fs::create_dir_all(&path);
                ShellExecuteW(
                    Some(self.window),
                    w!("open"),
                    PCWSTR(wide(&path.to_string_lossy()).as_ptr()),
                    None,
                    None,
                    SW_SHOW,
                );
            },
            DISCOVER => self.discover(),
            SCREENS => {
                if let Some(remote) = self.remote.clone() {
                    let placement = if self.settings.server {
                        self.settings.placement
                    } else {
                        Placement {
                            right: !self.settings.placement.right,
                            offset: -self.settings.placement.offset,
                        }
                    };
                    if let Some(chosen) =
                        layout_dialog::show(self.window, self.local.clone(), remote, placement)
                    {
                        self.settings.placement = if self.settings.server {
                            chosen
                        } else {
                            Placement {
                                right: !chosen.right,
                                offset: -chosen.offset,
                            }
                        };
                        self.remember_layout();
                        self.update_layout();
                        if let Some(p) = &self.peer {
                            p.send(Control::Placement {
                                placement: self.settings.placement,
                            });
                        }
                    }
                } else {
                    self.status("Connectez les deux PC pour placer leurs écrans.");
                }
            }
            PAIR => {
                if self.settings.server {
                    if let Some(s) = &self.session {
                        s.allow_pair
                            .store(network::now() + 120000, Ordering::Relaxed);
                        self.status("Nouvelle association autorisée pendant deux minutes.");
                    } else {
                        self.status("Démarrez le serveur, puis autorisez la nouvelle association.");
                    }
                } else {
                    self.stop();
                    self.trust = None;
                    let _ = std::fs::remove_file(storage::root().join("trust.dpapi"));
                    self.status(
                        "Association oubliée. La prochaine connexion demandera une confirmation.",
                    );
                }
            }
            HYBRID | PLAIN | TRAILS | AUTOSTART | AUTO | MINIMIZED | ROLE => {
                if notification == 0 || notification == CBN_SELCHANGE as u16 {
                    let reconnect = self.session.is_some() && [HYBRID, ROLE].contains(&id);
                    if reconnect {
                        self.stop()
                    }
                    if self.read_form().is_err() {
                        self.status("Impossible d’enregistrer ces réglages.");
                        return;
                    }
                    if id == PLAIN {
                        self.clipboard.plain(self.settings.plain);
                    }
                    if reconnect {
                        self.start()
                    }
                }
            }
            100 => unsafe {
                let _ = ShowWindow(self.window, SW_RESTORE);
                let _ = SetForegroundWindow(self.window);
            },
            101 => self.stop(),
            102 => self.quit(),
            _ => (),
        }
    }
    fn active(&self, id: Uuid) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| s.id == id && !s.stop.is_cancelled())
    }
    fn start(&mut self) {
        if self.session.is_some() {
            return;
        }
        if self.read_form().is_err() {
            self.status("Réglages invalides : vérifiez l’adresse et le port (1024 à 65535).");
            return;
        }
        self.local = display::desktop();
        let id = Uuid::new_v4();
        let stop = CancellationToken::new();
        let allow_pair = Arc::new(AtomicU64::new(0));
        self.session = Some(Session {
            id,
            stop: stop.clone(),
            allow_pair: allow_pair.clone(),
        });
        set_text(self.controls[&START], "Arrêter");
        self.status(if self.settings.server {
            "Serveur ouvert · en attente de l’autre PC…"
        } else {
            "Recherche et connexion au serveur…"
        });
        let ui = self.ui.clone();
        let input = self.input.clone();
        let clipboard = self.clipboard.clone();
        let sink: network::Sink = Arc::new(move |peer, event| match event {
            network::Event::Control(c)
                if !matches!(c, Control::Desktop { .. } | Control::Placement { .. }) =>
            {
                input.control(peer, c)
            }
            network::Event::Clipboard(offer) => clipboard.receive(peer, offer),
            network::Event::Drag(offer) => input.offer(peer, offer),
            event => {
                if matches!(event, network::Event::Closed(_)) {
                    input.detach(peer);
                    clipboard.detach(peer);
                }
                ui.post(move |a| {
                    if a.active(id) && a.peer.as_ref().is_some_and(|p| p.id == peer) {
                        a.network_event(event)
                    }
                });
            }
        });
        let ui = self.ui.clone();
        let input = self.input.clone();
        let clipboard = self.clipboard.clone();
        let session_stop = stop.clone();
        let server = self.settings.server;
        let plain = self.settings.plain;
        let connected: Arc<dyn Fn(Arc<Peer>) + Send + Sync> = Arc::new(move |peer| {
            if session_stop.is_cancelled() {
                peer.close("Session arrêtée");
                return;
            }
            input.connect(peer.clone(), server, session_stop.clone());
            clipboard.connect(peer.clone(), plain, session_stop.clone());
            ui.post(move |a| {
                if a.active(id) && peer.connected() {
                    a.attach(peer)
                } else {
                    peer.close("Session arrêtée")
                }
            });
        });
        let settings = self.settings.clone();
        let catalog = self.catalog.clone();
        let ui = self.ui.clone();
        if settings.server {
            let identity = Arc::new(Mutex::new(self.identity.clone()));
            let pair_id = identity.clone();
            let dest = ui.clone();
            let paired: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                let mut identity = pair_id.lock();
                identity.paired = true;
                let value = identity.clone();
                dest.post(move |a| {
                    if a.active(id) {
                        a.identity = value;
                        if storage::save("identity.dpapi", &a.identity, true).is_err() {
                            a.status("Association active mais sa sauvegarde a échoué.");
                        }
                    }
                });
            });
            let dest = ui.clone();
            let approve: network::PairAnswer = Arc::new(move |prompt| {
                let (tx, rx) = oneshot::channel();
                dest.post(move|a|{let accepted=a.active(id)&&unsafe{MessageBoxW(Some(a.window),PCWSTR(wide(&format!("Le PC {} ({}) souhaite se connecter.\n\nCode : {}\n\nComparez ce code avec celui affiché sur le client, puis confirmez.",prompt.name,prompt.address,prompt.code)).as_ptr()),w!("Associer les deux PC"),MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2)==IDYES};let _=tx.send(accepted);});
                Box::pin(async move { rx.await.unwrap_or(false) })
            });
            let name = self.local.name.clone();
            self.runtime.spawn(async move{loop{if stop.is_cancelled(){break}let options=network::ServerOptions{port:settings.port,name:name.clone(),hybrid:settings.hybrid,allow_pair:allow_pair.clone(),approve:approve.clone(),paired:paired.clone()};let identity=identity.lock().clone();let result=network::serve(identity,options,catalog.clone(),sink.clone(),connected.clone(),stop.clone()).await;if stop.is_cancelled(){break}
if result.is_err(){let ui=ui.clone();ui.post(move|a|if a.active(id){a.status("Écoute indisponible · nouvelle tentative dans trois secondes.");});}tokio::select!{_=stop.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(3))=>{}}}});
        } else {
            let trust = self.trust.clone();
            let name = self.local.name.clone();
            self.runtime.spawn(async move {
                client_loop(
                    id, settings, trust, name, ui, catalog, sink, connected, stop,
                )
                .await;
            });
        }
    }
    fn attach(&mut self, peer: Arc<Peer>) {
        if let Some(old) = self.peer.replace(peer.clone()) {
            old.close("Nouvelle session")
        }
        self.remote = None;
        peer.send(Control::Desktop {
            desktop: self.local.clone(),
        });
        if self.settings.server {
            peer.send(Control::Placement {
                placement: self.settings.placement,
            });
        }
        self.status("Connecté. Franchissez le bord commun pour passer sur l’autre PC.");
        storage::log("connection.ready");
    }
    fn network_event(&mut self, event: network::Event) {
        match event {
            network::Event::Control(Control::Desktop { desktop }) => {
                self.remote = Some(desktop);
                self.restore_layout();
                self.update_layout();
                if self.settings.server {
                    if let Some(p) = &self.peer {
                        p.send(Control::Placement {
                            placement: self.settings.placement,
                        });
                    }
                }
            }
            network::Event::Control(Control::Placement { placement }) => {
                if self.settings.placement != placement {
                    self.settings.placement = placement;
                    self.remember_layout();
                    self.update_layout();
                }
            }
            network::Event::Control(_) | network::Event::Clipboard(_) | network::Event::Drag(_) => {
            }
            network::Event::Closed(_) => {
                self.peer = None;
                self.remote = None;
                self.halo.show(None, false);
                self.receiving = false;
                let _ = self.trails.set(false);
                self.status(if self.settings.server {
                    "Connexion perdue · serveur toujours ouvert."
                } else {
                    "Connexion perdue · nouvelle tentative automatique…"
                });
                storage::log("connection.closed");
            }
        }
    }
    fn update_layout(&mut self) {
        if let Some(remote) = self.remote.clone() {
            let layout = if self.settings.server {
                Layout::new(self.local.clone(), remote, self.settings.placement)
            } else {
                Layout::new(remote, self.local.clone(), self.settings.placement)
                    .map(|l| l.reverse())
            };
            match layout {
                Ok(l) => {
                    if let Some(p) = &self.peer {
                        self.input.layout(p.id, Some(l));
                    }
                }
                Err(_) => self.status("Disposition invalide : ouvrez Écrans."),
            }
        }
    }
    fn topology_key(&self) -> Option<String> {
        self.remote.as_ref().map(|r| {
            if self.settings.server {
                layout::key(&self.local, r)
            } else {
                layout::key(r, &self.local)
            }
        })
    }
    fn remember_layout(&mut self) {
        if let Some(key) = self.topology_key() {
            if self.settings.profiles.len() >= 256 {
                if let Some(old) = self.settings.profiles.keys().next().cloned() {
                    self.settings.profiles.remove(&old);
                }
            }
            self.settings.profiles.insert(key, self.settings.placement);
        }
        let _ = storage::save("settings.json", &self.settings, false);
    }
    fn restore_layout(&mut self) {
        let Some(remote) = self.remote.as_ref() else {
            return;
        };
        let placement = self
            .topology_key()
            .and_then(|k| self.settings.profiles.get(&k).copied())
            .unwrap_or(self.settings.placement);
        self.settings.placement = if self.settings.server {
            layout::adapt(&self.local, remote, placement)
        } else {
            layout::adapt(remote, &self.local, placement)
        };
        self.remember_layout();
    }
    fn refresh_topology(&mut self) {
        let new = display::desktop();
        if new != self.local {
            self.local = new;
            self.input.hardware();
            if let Some(p) = &self.peer {
                p.send(Control::Desktop {
                    desktop: self.local.clone(),
                });
            }
            self.restore_layout();
            self.update_layout();
            if self.settings.server {
                if let Some(p) = &self.peer {
                    p.send(Control::Placement {
                        placement: self.settings.placement,
                    });
                }
            }
            storage::log("display.topology_changed");
        }
    }
    fn tick(&mut self) {
        self.topology_ticks += 1;
        self.refresh_topology();
        if !self.tray && self.topology_ticks % 3 == 0 {
            self.tray(true);
        }
        self.render_status();
    }
    fn render_status(&self) {
        // No statistic reads, formatting, WM_SETTEXT or logging for a hidden UI.
        if !unsafe { IsWindowVisible(self.window).as_bool() }
            || unsafe { IsIconic(self.window).as_bool() }
        {
            return;
        }
        let changed = |id, value: &str| {
            let window = self.controls[&id];
            if text(window) != value {
                set_text(window, value);
            }
        };
        changed(STATUS, &self.notice);
        let label = match &self.peer {
            Some(peer) if peer.active() => {
                let rtt = peer
                    .stats()
                    .4
                    .map(|n| format!("{n} ms"))
                    .unwrap_or_else(|| "en attente".into());
                format!("{} · ping TCP aller-retour : {rtt}", peer.transport())
            }
            Some(peer) => format!("{} · surveillance espacée", peer.transport()),
            None => "Connexion arrêtée".into(),
        };
        changed(TRANSPORT, &label);
    }
    fn hide(&mut self) {
        if !self.tray {
            self.tray(true);
        }
        unsafe {
            // If Explorer cannot register the tray icon, retain the taskbar entry.
            let _ = ShowWindow(self.window, if self.tray { SW_HIDE } else { SW_MINIMIZE });
        }
    }
    fn stop(&mut self) {
        if let Some(s) = self.session.take() {
            s.stop.cancel();
        }
        if let Some(p) = self.peer.take() {
            p.close("Arrêt demandé")
        }
        self.input.attach(None, self.settings.server);
        self.clipboard.attach(None, self.settings.plain);
        self.input.emergency();
        self.remote = None;
        self.receiving = false;
        self.halo.show(None, false);
        let _ = self.trails.set(false);
        set_text(self.controls[&START], "Démarrer");
        self.status("Arrêté. Vos périphériques contrôlent ce PC.");
    }
    fn discover(&mut self) {
        let ui = self.ui.clone();
        self.status("Recherche des serveurs sur le réseau local…");
        self.runtime.spawn(async move {
            let servers = network::discovery::find().await;
            ui.post(move |a| match servers {
                Ok(list) if !list.is_empty() => {
                    let chosen = if list.len() == 1 {
                        Some(&list[0])
                    } else {
                        list.iter().find(|s| unsafe {
                            MessageBoxW(
                                Some(a.window),
                                PCWSTR(
                                    wide(&format!("Utiliser {} ({}) ?", s.name, s.host)).as_ptr(),
                                ),
                                w!("Serveur détecté"),
                                MB_YESNO | MB_ICONQUESTION,
                            ) == IDYES
                        })
                    };
                    if let Some(s) = chosen {
                        set_text(a.controls[&HOST], &s.host);
                        set_text(a.controls[&PORTID], &s.port.to_string());
                        a.status("Serveur trouvé. Cliquez sur Démarrer.");
                    }
                }
                _ => a.status("Aucun serveur découvert. Vous pouvez saisir son adresse IP."),
            });
        });
    }
    fn tray(&mut self, show: bool) {
        unsafe {
            let mut data = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: self.window,
                uID: 1,
                uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
                uCallbackMessage: TRAY_MESSAGE,
                hIcon: self.icons.small,
                ..Default::default()
            };
            let tip = wide("MultiMachine Rust");
            data.szTip[..tip.len()].copy_from_slice(&tip);
            self.tray = if show {
                let registered = (self.tray && Shell_NotifyIconW(NIM_MODIFY, &data).as_bool())
                    || Shell_NotifyIconW(NIM_ADD, &data).as_bool();
                if registered {
                    data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                    let _ = Shell_NotifyIconW(NIM_SETVERSION, &data);
                }
                registered
            } else {
                let _ = Shell_NotifyIconW(NIM_DELETE, &data);
                false
            };
        }
    }
    fn menu(&mut self) {
        unsafe {
            if let Ok(menu) = CreatePopupMenu() {
                let _ = AppendMenuW(menu, MF_STRING, 100, w!("Ouvrir"));
                let _ = AppendMenuW(menu, MF_STRING, 101, w!("Arrêter la connexion"));
                let _ = AppendMenuW(menu, MF_STRING, 102, w!("Quitter"));
                let p = cursor();
                let _ = SetForegroundWindow(self.window);
                let choice = TrackPopupMenu(
                    menu,
                    TPM_RETURNCMD | TPM_RIGHTBUTTON,
                    p.x,
                    p.y,
                    None,
                    self.window,
                    None,
                );
                let _ = DestroyMenu(menu);
                if choice.0 != 0 {
                    self.command(choice.0 as usize, 0);
                }
            }
        }
    }
    fn quit(&mut self) {
        if self.closing {
            return;
        }
        self.closing = true;
        let _ = self.read_form();
        self.stop();
        self.tray(false);
        self.input.stop();
        self.clipboard.stop();
        unsafe {
            let _ = WTSUnRegisterSessionNotification(self.window);
            let _ = DestroyWindow(self.window);
            for font in self.fonts.drain(..) {
                let _ = DeleteObject(font.into());
            }
        }
    }
}
#[allow(clippy::too_many_arguments)]
async fn client_loop(
    id: Uuid,
    settings: Settings,
    mut trust: Option<Trust>,
    name: String,
    ui: Ui,
    catalog: Arc<Catalog>,
    sink: network::Sink,
    connected: Arc<dyn Fn(Arc<Peer>) + Send + Sync>,
    stop: CancellationToken,
) {
    loop {
        if stop.is_cancelled() {
            break;
        }
        let attempt = async {
            if trust.is_none() {
                let (host, port) = if settings.host.is_empty() {
                    let servers = network::discovery::find().await?;
                    ensure!(
                        servers.len() == 1,
                        "Saisissez l’adresse si plusieurs serveurs sont détectés"
                    );
                    (servers[0].host.clone(), servers[0].port)
                } else {
                    (settings.host.clone(), settings.port)
                };
                let dest = ui.clone();
                let hello = Arc::new(move |hello: network::ClientHello| {
                    dest.post(move |a| {
                        if a.active(id) {
                            a.status(&format!(
                                "Code {} · comparez puis acceptez sur le serveur.",
                                hello.code
                            ));
                        }
                    })
                });
                let paired = network::pair(host, port, name.clone(), hello, stop.clone()).await?;
                let value = paired.clone();
                let dest = ui.clone();
                dest.post(move |a| {
                    if a.active(id) {
                        a.trust = Some(value);
                        if storage::save("trust.dpapi", a.trust.as_ref().unwrap(), true).is_err() {
                            a.status("Association active mais sa sauvegarde a échoué.");
                        }
                    }
                });
                trust = Some(paired);
            }
            let mut target = trust.clone().unwrap();
            if !settings.host.is_empty() {
                target.host = settings.host.clone();
                target.port = settings.port;
            }
            let connection = match network::connect(
                &target,
                settings.hybrid,
                catalog.clone(),
                sink.clone(),
                stop.clone(),
            )
            .await
            {
                Ok(c) => c,
                Err(first) => {
                    if settings.host.is_empty() {
                        if let Ok(servers) = network::discovery::find().await {
                            if let Some(s) = servers
                                .into_iter()
                                .find(|s| s.fingerprint == target.fingerprint)
                            {
                                target.host = s.host;
                                target.port = s.port;
                                network::connect(
                                    &target,
                                    settings.hybrid,
                                    catalog.clone(),
                                    sink.clone(),
                                    stop.clone(),
                                )
                                .await?
                            } else {
                                return Err(first);
                            }
                        } else {
                            return Err(first);
                        }
                    } else {
                        return Err(first);
                    }
                }
            };
            let peer = connection.peer.clone();
            connected(peer.clone());
            connection.start();
            tokio::select! {_=peer.closed()=>{},_=stop.cancelled()=>peer.close("Arrêt demandé")};
            Ok::<_, anyhow::Error>(())
        };
        let result = tokio::select! {_=stop.cancelled()=>break,r=attempt=>r};
        if stop.is_cancelled() {
            break;
        }
        if result.is_err() {
            let dest = ui.clone();
            dest.post(move|a|if a.active(id){a.status("Serveur indisponible · nouvel essai dans trois secondes. Vérifiez l’adresse et l’association.");});
        }
        tokio::select! {_=stop.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(3))=>{}}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn embedded_icons_load_at_each_dpi_and_are_assigned_to_window() {
        unsafe extern "system" fn window_proc(w: HWND, m: u32, p: WPARAM, l: LPARAM) -> LRESULT {
            unsafe { DefWindowProcW(w, m, p, l) }
        }
        unsafe {
            let window = hidden("MultiMachineRust.IconTest", Some(window_proc)).unwrap();
            for dpi in [96, 120, 144, 192] {
                let icons = Icons::load(dpi).unwrap();
                icons.apply(window);
                for (kind, expected, metric) in [
                    (ICON_SMALL, icons.small, SM_CXSMICON),
                    (ICON_BIG, icons.large, SM_CXICON),
                ] {
                    let actual =
                        SendMessageW(window, WM_GETICON, Some(WPARAM(kind as usize)), None);
                    assert_eq!(actual.0, expected.0 as isize);
                    let mut info = ICONINFO::default();
                    GetIconInfo(expected, &mut info).unwrap();
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
                SendMessageW(
                    window,
                    WM_SETICON,
                    Some(WPARAM(ICON_SMALL as usize)),
                    Some(LPARAM(0)),
                );
                SendMessageW(
                    window,
                    WM_SETICON,
                    Some(WPARAM(ICON_BIG as usize)),
                    Some(LPARAM(0)),
                );
            }
            DestroyWindow(window).unwrap();
        }
    }
}
