use super::{capture, data, platform::*, storage};
use multimachine_rust::{
    files::{Catalog, Clock},
    model::*,
    network::Peer,
};
use std::{
    cell::RefCell,
    sync::{Arc, atomic::AtomicBool},
};
use tokio::runtime::Handle;
use uuid::Uuid;
use windows::Win32::{
    Foundation::*,
    System::{Com::*, DataExchange::*, Ole::*},
    UI::WindowsAndMessaging::*,
};
#[derive(Clone)]
pub struct Clipboard {
    sta: Sta,
}
struct State {
    peer: Option<Arc<Peer>>,
    catalog: Arc<Catalog>,
    clock: Clock,
    origin: Uuid,
    runtime: Handle,
    notice: Notice,
    own: Option<IDataObject>,
    last: u32,
    plain: bool,
    pending: Option<Offer>,
    window: HWND,
    retries: u8,
}
thread_local! {static STATE:RefCell<Option<State>>=const{RefCell::new(None)};}
impl Clipboard {
    pub fn new(catalog: Arc<Catalog>, runtime: Handle, notice: Notice) -> anyhow::Result<Self> {
        let sta = Sta::spawn("MMR clipboard STA", move |_| unsafe {
            let origin = Uuid::new_v4();
            match hidden("MultiMachineRust.Clipboard", Some(proc)) {
                Ok(window) => {
                    let _ = AddClipboardFormatListener(window);
                    SetTimer(Some(window), 1, 150, None);
                    STATE.with(|s| {
                        *s.borrow_mut() = Some(State {
                            peer: None,
                            catalog,
                            clock: Clock::new(origin),
                            origin,
                            runtime,
                            notice,
                            own: None,
                            last: GetClipboardSequenceNumber(),
                            plain: false,
                            pending: None,
                            window,
                            retries: 0,
                        })
                    });
                }
                Err(_) => (notice)("Initialisation du presse-papier impossible.".into()),
            }
        })?;
        Ok(Self { sta })
    }
    pub fn attach(&self, peer: Option<Arc<Peer>>, plain: bool) {
        self.sta.post(move || {
            STATE.with(|s| {
                if let Some(s) = s.borrow_mut().as_mut() {
                    s.peer = peer;
                    s.plain = plain;
                    s.last = 0;
                    s.retries = 0;
                    s.pending = None;
                }
            })
        });
    }
    pub fn plain(&self, plain: bool) {
        self.sta.post(move || {
            STATE.with(|s| {
                if let Some(s) = s.borrow_mut().as_mut() {
                    s.plain = plain;
                }
            })
        });
    }
    pub fn connect(&self, peer: Arc<Peer>, plain: bool, stop: tokio_util::sync::CancellationToken) {
        self.sta.post(move || {
            STATE.with(|s| {
                if stop.is_cancelled() || !peer.connected() {
                    return;
                }
                if let Some(s) = s.borrow_mut().as_mut() {
                    s.peer = Some(peer);
                    s.plain = plain;
                    s.last = 0;
                    s.retries = 0;
                    s.pending = None;
                }
            })
        });
    }
    pub fn detach(&self, id: Uuid) {
        self.sta.post(move || {
            STATE.with(|s| {
                if let Some(s) = s.borrow_mut().as_mut() {
                    if s.peer.as_ref().is_some_and(|p| p.id == id) {
                        s.peer = None;
                        s.pending = None;
                    }
                }
            })
        });
    }
    pub fn receive(&self, peer: Uuid, offer: Offer) {
        self.sta.post(move || {
            STATE.with(|s| {
                if let Some(s) = s.borrow_mut().as_mut() {
                    if s.peer.as_ref().is_some_and(|p| p.id == peer) && s.clock.accept(&offer) {
                        s.pending = Some(offer);
                        s.retries = 0;
                    }
                }
            })
        });
    }
    pub fn stop(&self) {
        self.sta.post(|| {
            STATE.with(|s| {
                let state = s.borrow_mut().take();
                if let Some(s) = state {
                    unsafe {
                        let _ = RemoveClipboardFormatListener(s.window);
                        let _ = DestroyWindow(s.window);
                        if s.own
                            .as_ref()
                            .is_some_and(|d| OleIsCurrentClipboard(d).is_ok())
                        {
                            let _ = OleSetClipboard(None::<&IDataObject>);
                        }
                    }
                }
            })
        });
        self.sta.stop();
    }
}
unsafe extern "system" fn proc(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if message == WM_TIMER || message == WM_CLIPBOARDUPDATE {
            // OLE calls can pump messages. Detach state while accessing a provider so a
            // reentrant clipboard notification cannot borrow the same state or echo it.
            let state = STATE.with(|s| s.borrow_mut().take());
            if let Some(mut state) = state {
                poll(&mut state);
                STATE.with(|s| *s.borrow_mut() = Some(state));
            }
            return LRESULT(0);
        }
        DefWindowProcW(hwnd, message, w, l)
    }
}
fn poll(s: &mut State) {
    unsafe {
        let Some(peer) = s.peer.clone().filter(|p| p.connected()) else {
            return;
        };
        if let Some(offer) = s.pending.clone() {
            let object = data::remote(
                offer,
                peer,
                s.runtime.clone(),
                s.plain,
                None,
                s.notice.clone(),
            );
            if OleSetClipboard(&object).is_ok() {
                s.own = Some(object);
                s.last = GetClipboardSequenceNumber();
                s.pending = None;
                s.retries = 0;
                storage::log("clipboard.remote_applied");
            } else {
                s.retries = s.retries.saturating_add(1);
                if s.retries >= 40 {
                    s.pending = None;
                    (s.notice)("Presse-papier occupé : réception annulée.".into());
                }
            }
            return;
        }
        let sequence = GetClipboardSequenceNumber();
        if sequence == s.last {
            return;
        }
        if s.own
            .as_ref()
            .is_some_and(|d| OleIsCurrentClipboard(d).is_ok())
        {
            s.last = sequence;
            return;
        }
        let result = (|| -> anyhow::Result<Offer> {
            let data = OleGetClipboard()?;
            let mut offer = if capture::supports(&data) {
                capture::files(&data, &s.catalog, &AtomicBool::new(false))?
            } else {
                Offer::empty(0, s.origin)
            };
            offer.formats = capture::formats(&data)?;
            offer.clock = s.clock.next_clock()?;
            offer.origin = s.origin;
            offer.validate()?;
            Ok(offer)
        })();
        if GetClipboardSequenceNumber() != sequence {
            return;
        }
        match result {
            Ok(offer) => {
                s.last = sequence;
                s.retries = 0;
                if !offer.files.is_empty() || !offer.formats.is_empty() {
                    peer.send_bulk(Bulk::Clipboard { offer });
                    storage::log("clipboard.offer_sent");
                }
            }
            Err(_) => {
                s.retries = s.retries.saturating_add(1);
                if s.retries >= 20 {
                    s.last = sequence;
                    s.retries = 0;
                    (s.notice)("Ce contenu de presse-papier n’est pas partageable.".into());
                }
            }
        }
    }
}
