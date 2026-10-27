use super::{desktop, drag, injector::Injector, platform::*, storage};
use multimachine_rust::{
    engine::{Effect, Engine},
    files::Catalog,
    layout::Layout,
    model::*,
    network::{Peer, now},
};
use std::{
    cell::{Cell, RefCell},
    sync::Arc,
};
use tokio::runtime::Handle;
use windows::Win32::{
    Foundation::*,
    UI::{Accessibility::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};
pub type Indicator = Arc<dyn Fn(Option<Monitor>, bool, bool) + Send + Sync>;
#[derive(Clone)]
pub struct InputThread {
    sta: Sta,
}
struct State {
    engine: Engine,
    peer: Option<Arc<Peer>>,
    injector: Injector,
    drag: drag::Drag,
    catalog: Arc<Catalog>,
    notice: Notice,
    indicator: Indicator,
    mouse: HHOOK,
    keyboard: HHOOK,
    window: HWND,
    desktop_hook: HWINEVENTHOOK,
    desktop_available: bool,
    desktop_check: u64,
}
thread_local! {static STATE:RefCell<Option<State>>=const{RefCell::new(None)};}
thread_local! {static DESKTOP_WINDOW:Cell<isize>=const{Cell::new(0)};}
const DESKTOP_CHANGED: u32 = WM_APP + 20;

fn install_hooks() -> anyhow::Result<(HHOOK, HHOOK)> {
    unsafe {
        let mouse_hook = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse), Some(module()), 0)?;
        match SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), Some(module()), 0) {
            Ok(keyboard_hook) => Ok((mouse_hook, keyboard_hook)),
            Err(error) => {
                let _ = UnhookWindowsHookEx(mouse_hook);
                Err(error.into())
            }
        }
    }
}
unsafe extern "system" fn desktop_event(
    _: HWINEVENTHOOK,
    _: u32,
    _: HWND,
    _: i32,
    _: i32,
    _: u32,
    _: u32,
) {
    // WinEvents may re-enter the installing thread. Defer state changes to its
    // own window, without borrowing the engine inside the callback.
    DESKTOP_WINDOW.with(|window| {
        if window.get() != 0 {
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(window.get() as *mut _)),
                    DESKTOP_CHANGED,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
    });
}
impl InputThread {
    pub fn new(
        catalog: Arc<Catalog>,
        runtime: Handle,
        notice: Notice,
        indicator: Indicator,
    ) -> anyhow::Result<Self> {
        let (ready, wait) = std::sync::mpsc::sync_channel(1);
        let sta = Sta::spawn("MMR hooks STA", move |sta| {
            let status = notice.clone();
            let result = (|| -> anyhow::Result<()> {
                unsafe {
                    let dest = sta.clone();
                    let done = Arc::new(move |epoch, kind| {
                        dest.post(move || with(|s| s.engine.barrier_done(epoch, kind)))
                    });
                    let dest = sta.clone();
                    let err_notice = notice.clone();
                    let error: Notice = Arc::new(move |text| {
                        (err_notice)(text);
                        dest.post(|| {
                            with(|s| {
                                s.check_desktop(false);
                                if s.desktop_available {
                                    s.engine.emergency(now());
                                    if let Some(p) = &s.peer {
                                        p.close("Injection refusée");
                                    }
                                }
                            })
                        });
                    });
                    let injector = Injector::new(done, error);
                    let dest = sta.clone();
                    let sink: drag::Sink = Arc::new(move |event| {
                        dest.post(move || {
                            with(|s| match event {
                                drag::Event::Captured(offer, point) => {
                                    s.engine.captured(offer, point, now())
                                }
                                drag::Event::CancelOriginal => s.engine.cancel_original(),
                                drag::Event::Returning(point) => s.engine.returning(point, now()),
                                drag::Event::Staged(id) => s.engine.staged(id),
                                drag::Event::Ready(id) => s.engine.native_ready(id),
                                drag::Event::Dropped(id) => s.engine.native_drop(id),
                                drag::Event::Finished(id, dropped) => {
                                    s.engine.native_finished(id, dropped)
                                }
                            })
                        })
                    });
                    let drag = drag::Drag::new(catalog.clone(), runtime, sink, notice.clone())?;
                    let window = hidden("MultiMachineRust.Input", Some(proc))?;
                    let (mouse, keyboard) = match install_hooks() {
                        Ok(hooks) => hooks,
                        Err(e) => {
                            drag.stop();
                            injector.stop();
                            return Err(e);
                        }
                    };
                    let mut engine = Engine::new(true, cursor());
                    engine.seed(
                        (8u16..255).filter(|vk| {
                            !matches!(vk, 0x10..=0x12) && GetAsyncKeyState(*vk as i32) < 0
                        }),
                        (1u8..=5).filter(|b| {
                            GetAsyncKeyState(match b {
                                1 => 1,
                                2 => 2,
                                3 => 4,
                                4 => 5,
                                _ => 6,
                            }) < 0
                        }),
                    );
                    DESKTOP_WINDOW.with(|w| w.set(window.0 as isize));
                    let desktop_hook = SetWinEventHook(
                        EVENT_SYSTEM_DESKTOPSWITCH,
                        EVENT_SYSTEM_DESKTOPSWITCH,
                        None,
                        Some(desktop_event),
                        0,
                        0,
                        WINEVENT_OUTOFCONTEXT,
                    );
                    let desktop_available = desktop::available();
                    injector.desktop_available(desktop_available);
                    STATE.with(|s| {
                        *s.borrow_mut() = Some(State {
                            engine,
                            peer: None,
                            injector,
                            drag,
                            catalog,
                            notice,
                            indicator,
                            mouse,
                            keyboard,
                            window,
                            desktop_hook,
                            desktop_available,
                            desktop_check: now() + 1000,
                        })
                    });
                    SetTimer(Some(window), 1, 100, None);
                    Ok(())
                }
            })();
            if result.is_err() {
                status("Installation des hooks Windows impossible.".into());
            }
            let _ = ready.send(result);
        })?;
        wait.recv()??;
        Ok(Self { sta })
    }
    pub fn attach(&self, peer: Option<Arc<Peer>>, server: bool) {
        self.sta.post(move || {
            with(|s| {
                s.engine.disconnect(now());
                s.apply();
                s.peer = peer.clone();
                s.engine.server = server;
                s.engine.connected = peer.is_some();
                s.drag.attach(peer);
            })
        });
    }
    pub fn layout(&self, id: uuid::Uuid, layout: Option<Layout>) {
        self.sta.post(move || {
            with(|s| {
                if s.peer.as_ref().is_some_and(|p| p.id == id) {
                    s.engine.set_layout(layout, now())
                }
            })
        });
    }
    pub fn connect(
        &self,
        peer: Arc<Peer>,
        server: bool,
        stop: tokio_util::sync::CancellationToken,
    ) {
        self.sta.post(move || {
            with(|s| {
                if stop.is_cancelled() || !peer.connected() {
                    return;
                }
                s.engine.disconnect(now());
                s.apply();
                s.peer = Some(peer.clone());
                s.engine.server = server;
                s.engine.connected = true;
                s.drag.attach(Some(peer));
            })
        });
    }
    pub fn detach(&self, id: uuid::Uuid) {
        self.sta.post(move || {
            with(|s| {
                if s.peer.as_ref().is_some_and(|p| p.id == id) {
                    s.engine.disconnect(now());
                    s.apply();
                    s.peer = None;
                    s.drag.attach(None);
                }
            })
        });
    }
    pub fn control(&self, id: uuid::Uuid, c: Control) {
        self.sta.post(move || {
            with(|s| {
                if s.peer.as_ref().is_some_and(|p| p.id == id) {
                    if matches!(
                        c,
                        Control::Begin { .. }
                            | Control::DragStart { .. }
                            | Control::DragHome { .. }
                    ) {
                        s.check_desktop(false);
                    }
                    if !s.desktop_available {
                        if let Some(peer) = &s.peer {
                            match c {
                                Control::Begin { lease, .. } => {
                                    peer.send(Control::End { lease });
                                }
                                Control::DragStart { offer, .. }
                                | Control::DragHome { offer, .. } => {
                                    peer.send(Control::DragCancel { offer });
                                }
                                _ => {}
                            }
                        }
                        return;
                    }
                    s.engine.control(c, now());
                }
            })
        });
    }
    pub fn offer(&self, id: uuid::Uuid, offer: Offer) {
        self.sta.post(move || {
            with(|s| {
                if s.peer.as_ref().is_some_and(|p| p.id == id) {
                    if s.desktop_available {
                        s.engine.receive_offer(offer, now());
                    } else if let Some(peer) = &s.peer {
                        peer.send(Control::DragCancel { offer: offer.id });
                    }
                }
            })
        });
    }
    pub fn hardware(&self) {
        self.sta
            .post(|| with(|s| s.engine.hardware(cursor(), now())));
    }
    pub fn emergency(&self) {
        self.sta.post(|| with(|s| s.engine.emergency(now())));
    }
    pub fn stop(&self) {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        self.sta.post(move || {
            STATE.with(|s| {
                let state = s.borrow_mut().take();
                if let Some(mut s) = state {
                    s.engine.disconnect(now());
                    s.apply();
                    unsafe {
                        let _ = UnhookWindowsHookEx(s.mouse);
                        let _ = UnhookWindowsHookEx(s.keyboard);
                        let _ = UnhookWinEvent(s.desktop_hook);
                        DESKTOP_WINDOW.with(|w| w.set(0));
                        let _ = DestroyWindow(s.window);
                    }
                    s.drag.stop();
                    s.injector.stop();
                }
            });
            let _ = tx.send(());
        });
        let _ = rx.recv_timeout(std::time::Duration::from_secs(3));
        self.sta.stop();
    }
}
fn with(action: impl FnOnce(&mut State)) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if let Some(s) = s.as_mut() {
            action(s);
            s.apply();
        }
    });
}
impl State {
    fn check_desktop(&mut self, notification: bool) {
        let mut available = desktop::available();
        self.desktop_check = now() + if available { 1000 } else { 500 };
        if available == self.desktop_available && !notification {
            return;
        }
        if available {
            match install_hooks() {
                Ok((mouse, keyboard)) => unsafe {
                    let _ = UnhookWindowsHookEx(self.mouse);
                    let _ = UnhookWindowsHookEx(self.keyboard);
                    self.mouse = mouse;
                    self.keyboard = keyboard;
                },
                Err(_) => {
                    available = false;
                    (self.notice)("Les entrées Windows sont indisponibles ; nouvelle tentative au retour sur le bureau.".into());
                }
            }
        }
        self.desktop_available = available;
        // Do this before the engine's reset/barriers so no pending completion is
        // removed after the engine has generated its new cleanup generation.
        self.injector.desktop_available(available);
        self.engine.hardware(cursor(), now());
        storage::log(if available {
            "desktop.resumed_inputs_reset"
        } else {
            "desktop.unavailable_control_cancelled"
        });
    }
    fn apply(&mut self) {
        for effect in self.engine.effects() {
            match effect {
                Effect::Send(c) => {
                    if let Some(peer) = &self.peer {
                        peer.send(c);
                    }
                }
                Effect::Offer(offer) => {
                    if let Some(peer) = &self.peer {
                        peer.send_bulk(Bulk::Drag { offer });
                    }
                }
                Effect::Inject(input) => {
                    if !self.injector.input(input) {
                        if let Some(peer) = &self.peer {
                            peer.close("File native saturée");
                        }
                    }
                }
                Effect::Reset(_) => self.injector.reset(),
                Effect::SourceLeftUp => self.injector.source_up(),
                Effect::Barrier { epoch, kind } => self.injector.barrier(epoch, kind),
                Effect::Portals(down) => self.drag.buttons(down),
                Effect::Layout(layout) => self.drag.layout(layout),
                Effect::Indicator {
                    monitor,
                    right,
                    receiving,
                } => (self.indicator)(monitor, right, receiving),
                Effect::Stage(id) => self.drag.stage(id),
                Effect::Start(offer) => self.drag.start(offer),
                Effect::CancelNative => self.drag.cancel(),
                Effect::Remove(id) => {
                    self.catalog.remove(id);
                }
                Effect::Status(text) => (self.notice)(text.into()),
            }
        }
    }
}
unsafe extern "system" fn proc(w: HWND, m: u32, p: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if m == WM_TIMER {
            with(|s| {
                if now() >= s.desktop_check
                    && (!s.desktop_available || s.engine.receiving() || !s.engine.outgoing.is_nil())
                {
                    s.check_desktop(false);
                }
                s.engine.poll(now());
            });
            return LRESULT(0);
        }
        if m == DESKTOP_CHANGED {
            with(|s| s.check_desktop(true));
            return LRESULT(0);
        }
        DefWindowProcW(w, m, p, l)
    }
}
unsafe extern "system" fn mouse(code: i32, message: WPARAM, data: LPARAM) -> LRESULT {
    unsafe {
        if code < 0 {
            return CallNextHookEx(None, code, message, data);
        }
        let e = &*(data.0 as *const MSLLHOOKSTRUCT);
        let m = message.0 as u32;
        if e.dwExtraInfo == INPUT_TAG
            || (e.flags & 1 != 0 && !matches!(m, WM_MOUSEWHEEL | WM_MOUSEHWHEEL))
        {
            return CallNextHookEx(None, code, message, data);
        }
        let input = match m {
            WM_MOUSEMOVE => Some(Input::Move {
                position: Point::new(e.pt.x, e.pt.y),
            }),
            WM_LBUTTONDOWN | WM_LBUTTONUP => Some(Input::Button {
                button: 1,
                up: m == WM_LBUTTONUP,
            }),
            WM_RBUTTONDOWN | WM_RBUTTONUP => Some(Input::Button {
                button: 2,
                up: m == WM_RBUTTONUP,
            }),
            WM_MBUTTONDOWN | WM_MBUTTONUP => Some(Input::Button {
                button: 3,
                up: m == WM_MBUTTONUP,
            }),
            WM_XBUTTONDOWN | WM_XBUTTONUP => Some(Input::Button {
                button: 3 + (e.mouseData >> 16) as u8,
                up: m == WM_XBUTTONUP,
            }),
            WM_MOUSEWHEEL | WM_MOUSEHWHEEL => Some(Input::Wheel {
                horizontal: m == WM_MOUSEHWHEEL,
                delta: (e.mouseData >> 16) as i16,
            }),
            _ => None,
        };
        let mut consumed = false;
        if let Some(input) = input {
            with(|s| {
                if s.desktop_available {
                    consumed = s.engine.physical(input, now());
                }
            });
        }
        if consumed {
            LRESULT(1)
        } else {
            CallNextHookEx(None, code, message, data)
        }
    }
}
unsafe extern "system" fn keyboard(code: i32, message: WPARAM, data: LPARAM) -> LRESULT {
    unsafe {
        if code < 0 {
            return CallNextHookEx(None, code, message, data);
        }
        let e = &*(data.0 as *const KBDLLHOOKSTRUCT);
        if e.flags.0 & 0x10 != 0 {
            return CallNextHookEx(None, code, message, data);
        }
        let mut consumed = false;
        with(|s| {
            if !s.desktop_available {
                return;
            }
            consumed = s.engine.physical(
                Input::Key {
                    key: e.vkCode as u16,
                    scan: e.scanCode as u16,
                    extended: e.flags.0 & 1 != 0,
                    up: e.flags.0 & 0x80 != 0,
                },
                now(),
            )
        });
        if consumed {
            LRESULT(1)
        } else {
            CallNextHookEx(None, code, message, data)
        }
    }
}
