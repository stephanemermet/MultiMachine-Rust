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
        UI::{HiDpi::*, Shell::*, WindowsAndMessaging::*},
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
    widgets: Vec<(HWND, i32, i32, i32, i32)>,
    fonts: Vec<HFONT>,
    icon: HICON,
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
const TRAY_MESSAGE: u32 = WM_APP + 2;

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
            hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as *mut _),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let window = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("MultiMachineRust.Main"),
            w!("MultiMachine Rust"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            690 * GetDpiForSystem() as i32 / 96,
            530 * GetDpiForSystem() as i32 / 96,
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
        let icon = LoadImageW(
            Some(module()),
            PCWSTR(std::ptr::dangling::<u16>()),
            IMAGE_ICON,
            32,
            32,
            LR_DEFAULTCOLOR,
        )
        .map(|h| HICON(h.0))
        .unwrap_or_else(|_| LoadIconW(None, IDI_APPLICATION).unwrap_or_default());
        SendMessageW(
            window,
            WM_SETICON,
            Some(WPARAM(ICON_BIG as usize)),
            Some(LPARAM(icon.0 as isize)),
        );
        SendMessageW(
            window,
            WM_SETICON,
            Some(WPARAM(ICON_SMALL as usize)),
            Some(LPARAM(icon.0 as isize)),
        );
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
            icon,
            tray: false,
            notice: "Prêt. Choisissez le rôle de cette machine.".into(),
            topology_ticks: 0,
            closing: false,
        };
        app.create_controls();
        app.rescale();
        app.fill();
        app.tray(true);
        let auto = app.settings.auto_connect;
        let minimized = app.settings.minimized || std::env::args().any(|s| s == "--startup");
        APP.with(|a| *a.borrow_mut() = Some(app));
        let _ = WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION);
        SetTimer(Some(window), 1, 500, None);
        if !minimized || !app_tray_available() {
            let _ = ShowWindow(window, SW_SHOW);
        }
        if auto {
            with(|a| a.start());
        }
        storage::log("application.start");
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
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
                with(|a| {
                    if !a.tray {
                        a.tray(true)
                    }
                    let _ = ShowWindow(w, SW_HIDE);
                });
                return LRESULT(0);
            }
            WM_SIZE if p.0 == SIZE_MINIMIZED as usize => {
                let _ = ShowWindow(w, SW_HIDE);
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
                SetBkColor(HDC(p.0 as *mut _), COLORREF(0xffffff));
                return LRESULT(GetStockObject(WHITE_BRUSH).0 as isize);
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
            TRAY_MESSAGE => {
                with(|a| match l.0 as u32 {
                    WM_LBUTTONDBLCLK | WM_LBUTTONUP => {
                        let _ = ShowWindow(w, SW_RESTORE);
                        let _ = SetForegroundWindow(w);
                    }
                    WM_RBUTTONUP => a.menu(),
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
            let dpi = GetDpiForWindow(self.window) as i32;
            let body = CreateFontW(
                -15 * dpi / 96,
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                0,
                w!("Segoe UI"),
            );
            let title = CreateFontW(
                -22 * dpi / 96,
                0,
                0,
                0,
                600,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                0,
                w!("Segoe UI"),
            );
            for (i, (child, x, y, w, h)) in self.widgets.iter().enumerate() {
                let _ = SetWindowPos(
                    *child,
                    None,
                    x * dpi / 96,
                    y * dpi / 96,
                    w * dpi / 96,
                    h * dpi / 96,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                SendMessageW(
                    *child,
                    WM_SETFONT,
                    Some(WPARAM(if i == 0 { title.0 } else { body.0 } as usize)),
                    Some(LPARAM(1)),
                );
            }
            for font in self.fonts.drain(..) {
                let _ = DeleteObject(font.into());
            }
            self.fonts = vec![body, title];
        }
    }
    fn create_controls(&mut self) {
        unsafe {
            let specs = [
                ("STATIC", "MultiMachine Rust", 0, 22, 16, 620, 28, 0),
                (
                    "STATIC",
                    "Deux PC, un espace de travail",
                    0,
                    22,
                    47,
                    620,
                    24,
                    0,
                ),
                ("STATIC", "Cette machine", 0, 22, 88, 150, 24, 0),
                (
                    "COMBOBOX",
                    "",
                    CBS_DROPDOWNLIST as u32,
                    175,
                    83,
                    450,
                    100,
                    ROLE,
                ),
                ("STATIC", "Adresse du serveur", 0, 22, 130, 150, 24, 0),
                (
                    "EDIT",
                    "",
                    WS_BORDER.0 | ES_AUTOHSCROLL as u32,
                    175,
                    125,
                    310,
                    28,
                    HOST,
                ),
                (
                    "BUTTON",
                    "Détecter",
                    BS_PUSHBUTTON as u32,
                    500,
                    124,
                    125,
                    30,
                    DISCOVER,
                ),
                ("STATIC", "Port", 0, 22, 173, 150, 24, 0),
                (
                    "EDIT",
                    "",
                    WS_BORDER.0 | ES_NUMBER as u32,
                    175,
                    166,
                    95,
                    28,
                    PORTID,
                ),
                (
                    "BUTTON",
                    "Démarrer",
                    BS_DEFPUSHBUTTON as u32,
                    290,
                    164,
                    150,
                    34,
                    START,
                ),
                (
                    "BUTTON",
                    "Écrans…",
                    BS_PUSHBUTTON as u32,
                    455,
                    164,
                    170,
                    34,
                    SCREENS,
                ),
                (
                    "BUTTON",
                    "Mouvement hybride UDP + TCP",
                    BS_AUTOCHECKBOX as u32,
                    22,
                    217,
                    350,
                    24,
                    HYBRID,
                ),
                (
                    "BUTTON",
                    "Traces du curseur distant",
                    BS_AUTOCHECKBOX as u32,
                    380,
                    217,
                    265,
                    24,
                    TRAILS,
                ),
                (
                    "BUTTON",
                    "Texte sans mise en forme",
                    BS_AUTOCHECKBOX as u32,
                    22,
                    250,
                    350,
                    24,
                    PLAIN,
                ),
                (
                    "BUTTON",
                    "Connexion automatique",
                    BS_AUTOCHECKBOX as u32,
                    380,
                    250,
                    265,
                    24,
                    AUTO,
                ),
                (
                    "BUTTON",
                    "Démarrer avec Windows",
                    BS_AUTOCHECKBOX as u32,
                    22,
                    283,
                    350,
                    24,
                    AUTOSTART,
                ),
                (
                    "BUTTON",
                    "Démarrer réduit",
                    BS_AUTOCHECKBOX as u32,
                    380,
                    283,
                    265,
                    24,
                    MINIMIZED,
                ),
                (
                    "BUTTON",
                    "Nouvelle association",
                    BS_PUSHBUTTON as u32,
                    22,
                    326,
                    210,
                    31,
                    PAIR,
                ),
                (
                    "BUTTON",
                    "Diagnostics",
                    BS_PUSHBUTTON as u32,
                    247,
                    326,
                    180,
                    31,
                    LOGS,
                ),
                (
                    "BUTTON",
                    "Réduire dans la zone",
                    BS_PUSHBUTTON as u32,
                    442,
                    326,
                    183,
                    31,
                    HIDE,
                ),
                ("STATIC", "", 0, 22, 378, 620, 48, STATUS),
                (
                    "STATIC",
                    "Ctrl + Alt + F12 : reprendre la main",
                    0,
                    22,
                    442,
                    620,
                    24,
                    TRANSPORT,
                ),
            ];
            for (class, label, style, x, y, w, h, id) in specs {
                let c = layout_dialog::control(self.window, class, label, style, x, y, w, h, id);
                SendMessageW(
                    c,
                    WM_SETFONT,
                    Some(WPARAM(GetStockObject(DEFAULT_GUI_FONT).0 as usize)),
                    Some(LPARAM(1)),
                );
                self.widgets.push((c, x, y, w, h));
                if id != 0 {
                    self.controls.insert(id, c);
                }
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
        set_text(self.controls[&STATUS], text);
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
            HIDE => unsafe {
                let _ = ShowWindow(self.window, SW_HIDE);
            },
            LOGS => unsafe {
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
        if self.topology_ticks % 4 == 0 {
            self.refresh_topology();
        }
        if let Some(p) = &self.peer {
            let (q, c, u, received, rtt) = p.stats();
            set_text(
                self.controls[&TRANSPORT],
                &format!(
                    "{} · délai {} ms · Ctrl + Alt + F12 pour revenir",
                    p.transport(),
                    rtt.map(|n| n.to_string()).unwrap_or_else(|| "—".into())
                ),
            );
            if self.topology_ticks % 20 == 0 {
                storage::log(&format!(
                    "transport queue={q} coalesced={c} udp_sent={u} udp_received={received} rtt_ms={}",
                    rtt.unwrap_or(0)
                ));
            }
        } else {
            set_text(
                self.controls[&TRANSPORT],
                "Ctrl + Alt + F12 : reprendre la main",
            );
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
                hIcon: self.icon,
                ..Default::default()
            };
            let tip = wide("MultiMachine Rust");
            data.szTip[..tip.len()].copy_from_slice(&tip);
            self.tray = if show {
                Shell_NotifyIconW(NIM_ADD, &data).as_bool()
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
