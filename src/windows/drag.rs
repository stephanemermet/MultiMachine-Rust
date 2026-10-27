#![allow(non_snake_case)]
use super::{capture, data, platform::*, storage};
use multimachine_rust::{
    files::Catalog,
    layout::Layout,
    model::*,
    network::{Peer, now},
};
use parking_lot::Mutex;
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};
use tokio::runtime::Handle;
use uuid::Uuid;
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::{Com::*, Ole::*, SystemServices::*, Threading::*},
        UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    },
    core::*,
};
#[derive(Clone, Debug)]
pub enum Event {
    Captured(Offer, Point),
    CancelOriginal,
    Returning(Point),
    Staged(Uuid),
    Ready(Uuid),
    Dropped(Uuid),
    Finished(Uuid, bool),
}
pub type Sink = Arc<dyn Fn(Event) + Send + Sync>;
#[derive(Clone)]
pub struct Drag {
    sta: Sta,
    shared: Arc<Shared>,
}
struct Shared {
    operation: Mutex<Option<Arc<Operation>>>,
    capture_stop: Mutex<Arc<AtomicBool>>,
    stopped: AtomicBool,
    released: AtomicU64,
    thread: AtomicU64,
    sink: Sink,
    notice: Notice,
    catalog: Arc<Catalog>,
    runtime: Handle,
}
struct Operation {
    id: Uuid,
    cancelled: AtomicBool,
    dropped: Arc<AtomicBool>,
    running: AtomicBool,
    callback: AtomicBool,
    since: u64,
}
struct State {
    shared: Arc<Shared>,
    portals: Vec<(HWND, IDropTarget)>,
    buttons: bool,
    peer: Option<Arc<Peer>>,
}
thread_local! {static STATE:RefCell<Option<State>>=const{RefCell::new(None)};}
impl Drag {
    pub fn new(
        catalog: Arc<Catalog>,
        runtime: Handle,
        sink: Sink,
        notice: Notice,
    ) -> anyhow::Result<Self> {
        let shared = Arc::new(Shared {
            operation: Mutex::new(None),
            capture_stop: Mutex::new(Arc::new(AtomicBool::new(false))),
            stopped: AtomicBool::new(false),
            released: AtomicU64::new(0),
            thread: AtomicU64::new(0),
            sink,
            notice,
            catalog,
            runtime,
        });
        let s = shared.clone();
        let sta = Sta::spawn("MMR drag OLE STA", move |sta| {
            s.thread.store(sta.id() as u64, Ordering::Release);
            STATE.with(|state| {
                *state.borrow_mut() = Some(State {
                    shared: s,
                    portals: vec![],
                    buttons: false,
                    peer: None,
                })
            });
        })?;
        let s = shared.clone();
        thread::Builder::new()
            .name("MMR OLE watchdog".into())
            .spawn(move || {
                while !s.stopped.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(100));
                    s.watch();
                }
            })?;
        Ok(Self { sta, shared })
    }
    pub fn attach(&self, peer: Option<Arc<Peer>>) {
        self.cancel();
        self.sta.post(move || {
            STATE.with(|s| {
                if let Some(s) = s.borrow_mut().as_mut() {
                    s.peer = peer
                }
            })
        });
    }
    pub fn layout(&self, layout: Option<Layout>) {
        self.sta.post(move || {
            STATE.with(|s| {
                if let Some(s) = s.borrow_mut().as_mut() {
                    unsafe {
                        for (w, _) in s.portals.drain(..) {
                            let _ = RevokeDragDrop(w);
                            let _ = DestroyWindow(w);
                        }
                        if let Some(layout) = layout {
                            for area in layout.portals() {
                                match portal(s.shared.clone(), area, layout.placement.right) {
                                    Ok((w, target)) => {
                                        if s.buttons {
                                            let _ = ShowWindow(w, SW_SHOWNOACTIVATE);
                                        }
                                        s.portals.push((w, target));
                                    }
                                    Err(_) => (s.shared.notice)(
                                        "Création du passage de fichiers impossible.".into(),
                                    ),
                                }
                            }
                        }
                    }
                }
            })
        });
    }
    pub fn buttons(&self, down: bool) {
        self.shared
            .released
            .store(if down { 0 } else { now() }, Ordering::Release);
        self.sta.post(move || {
            STATE.with(|s| {
                if let Some(s) = s.borrow_mut().as_mut() {
                    s.buttons = down;
                    unsafe {
                        for (w, _) in &s.portals {
                            let _ = ShowWindow(*w, if down { SW_SHOWNOACTIVATE } else { SW_HIDE });
                        }
                        if s.shared.operation.lock().is_none() {
                            release_capture();
                        }
                    }
                }
            })
        });
    }
    pub fn stage(&self, id: Uuid) {
        self.sta.post(move || {
            STATE.with(|s| {
                if let Some(s) = s.borrow().as_ref() {
                    unsafe {
                        for (w, _) in &s.portals {
                            let _ = ShowWindow(*w, SW_SHOWNOACTIVATE);
                        }
                    }
                    (s.shared.sink)(Event::Staged(id));
                }
            })
        });
    }
    pub fn cancel(&self) {
        self.shared
            .capture_stop
            .lock()
            .store(true, Ordering::Release);
        let operation = self.shared.operation.lock().clone();
        if let Some(op) = operation {
            if !op.dropped.load(Ordering::Acquire) {
                op.cancelled.store(true, Ordering::Release);
                self.shared.wake(&op);
            }
        }
    }
    pub fn start(&self, offer: Offer) {
        let op = Arc::new(Operation {
            id: offer.id,
            cancelled: AtomicBool::new(false),
            dropped: Arc::new(AtomicBool::new(false)),
            running: AtomicBool::new(false),
            callback: AtomicBool::new(false),
            since: now(),
        });
        {
            let mut slot = self.shared.operation.lock();
            if slot.is_some() || self.shared.stopped.load(Ordering::Acquire) {
                (self.shared.sink)(Event::Finished(offer.id, false));
                return;
            }
            *slot = Some(op.clone());
        }
        let shared = self.shared.clone();
        self.sta.post(move || {
            let peer = STATE.with(|s| s.borrow().as_ref().and_then(|s| s.peer.clone()));
            if let Some(peer) = peer.filter(|p| p.connected()) {
                if !op.cancelled.load(Ordering::Acquire) && left() {
                    unsafe {
                        release_capture();
                        op.running.store(true, Ordering::Release);
                        let source: IDropSource = Source {
                            operation: op.clone(),
                            shared: shared.clone(),
                            peer: peer.clone(),
                        }
                        .into();
                        let data = data::remote(
                            offer,
                            peer,
                            shared.runtime.clone(),
                            false,
                            Some(op.dropped.clone()),
                            shared.notice.clone(),
                        );
                        // Prepared ACK unblocks the physical relay before the native modal loop.
                        (shared.sink)(Event::Ready(op.id));
                        storage::log("drag.native_begin");
                        let mut effect = DROPEFFECT_NONE;
                        let result = DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect);
                        storage::log(&format!(
                            "drag.native_end hr={:08x} effect={}",
                            result.0 as u32, effect.0
                        ));
                        release_capture();
                        (shared.notice)(
                            if op.dropped.load(Ordering::Acquire) {
                                "Geste remis à l’application ; transfert à sa demande."
                            } else {
                                "Glisser-déposer annulé. Source conservée."
                            }
                            .into(),
                        );
                    }
                }
            }
            op.running.store(false, Ordering::Release);
            {
                let mut slot = shared.operation.lock();
                if slot.as_ref().is_some_and(|s| Arc::ptr_eq(s, &op)) {
                    *slot = None;
                }
            }
            (shared.sink)(Event::Finished(op.id, op.dropped.load(Ordering::Acquire)));
        });
    }
    pub fn stop(&self) {
        self.cancel();
        self.shared.stopped.store(true, Ordering::Release);
        self.layout(None);
        self.sta.stop();
    }
}
impl Shared {
    fn capture_window(&self) -> HWND {
        unsafe {
            let tid = self.thread.load(Ordering::Acquire) as u32;
            let mut info = GUITHREADINFO {
                cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
                ..Default::default()
            };
            if tid == 0
                || GetGUIThreadInfo(tid, &mut info).is_err()
                || info.hwndCapture.is_invalid()
            {
                return HWND::default();
            }
            let mut pid = 0;
            if GetWindowThreadProcessId(info.hwndCapture, Some(&mut pid)) == tid
                && pid == GetCurrentProcessId()
            {
                info.hwndCapture
            } else {
                HWND::default()
            }
        }
    }
    fn wake(&self, expected: &Arc<Operation>) {
        let slot = self.operation.lock();
        if !slot.as_ref().is_some_and(|s| Arc::ptr_eq(s, expected))
            || !expected.running.load(Ordering::Acquire)
            || !expected.cancelled.load(Ordering::Acquire)
        {
            return;
        }
        unsafe {
            let w = self.capture_window();
            if !w.is_invalid() {
                let _ = PostMessageW(
                    Some(w),
                    WM_KEYUP,
                    WPARAM(0x1b),
                    LPARAM(0xc0010001u32 as i32 as isize),
                );
            } else {
                let _ = PostThreadMessageW(
                    self.thread.load(Ordering::Acquire) as u32,
                    WM_KEYUP,
                    WPARAM(0x1b),
                    LPARAM(0xc0010001u32 as i32 as isize),
                );
            }
        }
    }
    fn watch(&self) {
        let Some(op) = self.operation.lock().clone() else {
            return;
        };
        if !op.running.load(Ordering::Acquire) || op.dropped.load(Ordering::Acquire) {
            return;
        }
        let released = self.released.load(Ordering::Acquire);
        if released != 0 && now().saturating_sub(released) > 1500 {
            op.cancelled.store(true, Ordering::Release);
        }
        if now().saturating_sub(op.since) > 15000 && !op.callback.load(Ordering::Acquire) {
            op.cancelled.store(true, Ordering::Release);
        }
        if op.cancelled.load(Ordering::Acquire) {
            self.wake(&op);
            return;
        }
        if op.callback.load(Ordering::Acquire) || !left() {
            return;
        }
        let slot = self.operation.lock();
        if !slot.as_ref().is_some_and(|s| Arc::ptr_eq(s, &op)) {
            return;
        }
        unsafe {
            let capture = self.capture_window();
            if capture.is_invalid() {
                return;
            }
            let p = cursor();
            let mut p = POINT { x: p.x, y: p.y };
            if !ScreenToClient(capture, &mut p).as_bool() {
                return;
            }
            let mut keys = 1usize;
            for (v, flag) in [(2, 2), (0x10, 4), (0x11, 8), (4, 16), (5, 32), (6, 64)] {
                if GetAsyncKeyState(v) < 0 {
                    keys |= flag
                }
            }
            let _ = PostMessageW(
                Some(capture),
                WM_MOUSEMOVE,
                WPARAM(keys),
                LPARAM(((p.x as u16 as u32) | ((p.y as u16 as u32) << 16)) as isize),
            );
        }
    }
}
unsafe fn release_capture() {
    unsafe {
        if !GetCapture().is_invalid() {
            let _ = ReleaseCapture();
        }
    }
}
#[implement(IDropSource)]
struct Source {
    operation: Arc<Operation>,
    shared: Arc<Shared>,
    peer: Arc<Peer>,
}
impl IDropSource_Impl for Source_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        self.operation.callback.store(true, Ordering::Release);
        if escape.as_bool()
            || self.operation.cancelled.load(Ordering::Acquire)
            || !self.peer.connected()
            || self.shared.stopped.load(Ordering::Acquire)
        {
            self.operation.cancelled.store(true, Ordering::Release);
            return DRAGDROP_S_CANCEL;
        }
        if keys.0 & 1 == 0 {
            if !self.operation.dropped.swap(true, Ordering::AcqRel) {
                (self.shared.sink)(Event::Dropped(self.operation.id));
            }
            return DRAGDROP_S_DROP;
        }
        S_OK
    }
    fn GiveFeedback(&self, _: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}
struct PortalState {
    data: Option<IDataObject>,
    captured: Option<Offer>,
    exit: Point,
    entered: bool,
    preparing: bool,
    waiting: bool,
    attempted: bool,
    since: u64,
    stop: Arc<AtomicBool>,
}
#[implement(IDropTarget)]
struct Portal {
    shared: Arc<Shared>,
    area: Monitor,
    right: bool,
    state: Rc<RefCell<PortalState>>,
}
impl Portal {
    fn over(
        &self,
        keys: MODIFIERKEYS_FLAGS,
        point: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> Result<()> {
        unsafe {
            if effect.is_null() {
                return Err(Error::from_hresult(E_POINTER));
            }
            let allowed = (*effect).0 & 1 != 0;
            *effect = DROPEFFECT_NONE;
            let seam = Point::new(
                if self.right {
                    self.area.right() - 1
                } else {
                    self.area.x
                },
                point.y,
            );
            let edge = if self.right {
                point.x >= self.area.right() - 2
            } else {
                point.x <= self.area.x + 1
            };
            if !allowed
                || keys.0 & 1 == 0
                || !edge
                || !self.area.contains(Point::new(point.x, point.y))
            {
                return Ok(());
            }
            let active = self.shared.operation.lock().clone();
            if let Some(op) = active {
                op.cancelled.store(true, Ordering::Release);
                self.shared.wake(&op);
                (self.shared.sink)(Event::Returning(seam));
                return Ok(());
            }
            let data = {
                let mut s = self.state.borrow_mut();
                if !s.entered
                    || s.preparing
                    || s.waiting
                    || s.attempted
                    || self.shared.stopped.load(Ordering::Acquire)
                {
                    return Ok(());
                }
                let Some(data) = s.data.take() else {
                    return Ok(());
                };
                s.attempted = true;
                s.preparing = true;
                s.stop = Arc::new(AtomicBool::new(false));
                *self.shared.capture_stop.lock() = s.stop.clone();
                data
            };
            if !capture::supports(&data) {
                self.state.borrow_mut().preparing = false;
                return Ok(());
            }
            if !self.state.borrow().entered {
                return Ok(());
            }
            (self.shared.notice)("Préparation du passage des fichiers…".into());
            storage::log("drag.capture_begin");
            let stop = self.state.borrow().stop.clone();
            // A provider is allowed to pump messages: do not hold the PortalState borrow here.
            let offer = capture::files(&data, &self.shared.catalog, &stop);
            let mut s = self.state.borrow_mut();
            if !Arc::ptr_eq(&s.stop, &stop) {
                if let Ok(offer) = offer {
                    self.shared.catalog.remove(offer.id);
                }
                return Ok(());
            }
            s.preparing = false;
            match offer {
                Ok(offer) => {
                    if !s.entered
                        || stop.load(Ordering::Acquire)
                        || !left()
                        || !self.area.contains(cursor())
                    {
                        self.shared.catalog.remove(offer.id);
                        return Ok(());
                    }
                    s.captured = Some(offer);
                    s.exit = seam;
                    s.waiting = true;
                    s.since = now();
                    drop(s);
                    (self.shared.sink)(Event::CancelOriginal);
                    storage::log("drag.capture_ready");
                }
                Err(_) => (self.shared.notice)(
                    "Passage refusé : la source ne fournit pas de fichiers lisibles.".into(),
                ),
            }
            Ok(())
        }
    }
}
impl IDropTarget_Impl for Portal_Impl {
    fn DragEnter(
        &self,
        data: Ref<IDataObject>,
        keys: MODIFIERKEYS_FLAGS,
        p: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> Result<()> {
        let incoming = data.as_ref().cloned();
        let old = {
            let mut s = self.state.borrow_mut();
            s.stop.store(true, Ordering::Release);
            let old = std::mem::replace(&mut s.data, incoming);
            if let Some(offer) = s.captured.take() {
                self.shared.catalog.remove(offer.id);
            }
            s.entered = true;
            s.preparing = false;
            s.waiting = false;
            s.attempted = false;
            old
        };
        drop(old);
        self.over(keys, p, effect)
    }
    fn DragOver(
        &self,
        keys: MODIFIERKEYS_FLAGS,
        p: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> Result<()> {
        self.over(keys, p, effect)
    }
    fn DragLeave(&self) -> Result<()> {
        let mut s = self.state.borrow_mut();
        s.entered = false;
        let old = s.data.take();
        if !s.waiting {
            s.stop.store(true, Ordering::Release);
        }
        let captured = s.captured.take();
        let valid =
            s.waiting && !s.stop.load(Ordering::Acquire) && now().saturating_sub(s.since) < 2000;
        let exit = s.exit;
        s.waiting = false;
        drop(s);
        drop(old);
        if let Some(offer) = captured {
            if valid {
                (self.shared.sink)(Event::Captured(offer, exit));
            } else {
                self.shared.catalog.remove(offer.id);
            }
        }
        Ok(())
    }
    fn Drop(
        &self,
        _: Ref<IDataObject>,
        _: MODIFIERKEYS_FLAGS,
        _: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> Result<()> {
        unsafe {
            if !effect.is_null() {
                *effect = DROPEFFECT_NONE
            }
        }
        let mut s = self.state.borrow_mut();
        s.entered = false;
        s.waiting = false;
        s.stop.store(true, Ordering::Release);
        let old = s.data.take();
        if let Some(offer) = s.captured.take() {
            self.shared.catalog.remove(offer.id);
        }
        drop(s);
        drop(old);
        Ok(())
    }
}
unsafe extern "system" fn portal_proc(w: HWND, m: u32, p: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if m == WM_MOUSEACTIVATE {
            return LRESULT(MA_NOACTIVATE as isize);
        }
        if (WM_LBUTTONDOWN..=WM_MBUTTONDBLCLK).contains(&m)
            || (WM_XBUTTONDOWN..=WM_XBUTTONDBLCLK).contains(&m)
        {
            return LRESULT(0);
        }
        DefWindowProcW(w, m, p, l)
    }
}
unsafe fn portal(shared: Arc<Shared>, area: Monitor, right: bool) -> Result<(HWND, IDropTarget)> {
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(portal_proc),
            hInstance: module(),
            lpszClassName: w!("MultiMachineRust.Portal"),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
            w!("MultiMachineRust.Portal"),
            w!(""),
            WS_POPUP,
            area.x,
            area.y,
            area.width,
            area.height,
            None,
            None,
            Some(module()),
            None,
        )?;
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 25, LWA_ALPHA);
        let target: IDropTarget = Portal {
            shared,
            area,
            right,
            state: Rc::new(RefCell::new(PortalState {
                data: None,
                captured: None,
                exit: Point::default(),
                entered: false,
                preparing: false,
                waiting: false,
                attempted: false,
                since: 0,
                stop: Arc::new(AtomicBool::new(false)),
            })),
        }
        .into();
        if let Err(e) = RegisterDragDrop(hwnd, &target) {
            let _ = DestroyWindow(hwnd);
            return Err(e);
        }
        Ok((hwnd, target))
    }
}
