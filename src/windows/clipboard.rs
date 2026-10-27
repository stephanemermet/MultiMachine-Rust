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
use windows::core::{HRESULT, Interface};
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

fn is_current(object: &IDataObject) -> bool {
    // The generated Result<()> wrapper discards S_FALSE: both S_OK and S_FALSE
    // become Ok(()). This API's exact HRESULT is its ownership answer.
    // https://learn.microsoft.com/windows/win32/api/ole2/nf-ole2-oleiscurrentclipboard
    #[link(name = "ole32")]
    unsafe extern "system" {
        #[link_name = "OleIsCurrentClipboard"]
        fn current_raw(object: *mut std::ffi::c_void) -> HRESULT;
    }
    unsafe { current_raw(object.as_raw()) == S_OK }
}
fn clear_owned(object: Option<&IDataObject>) {
    if object.is_some_and(is_current) {
        unsafe {
            let _ = OleSetClipboard(None::<&IDataObject>);
        }
    }
}
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
                        clear_owned(s.own.as_ref());
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
        if s.own.as_ref().is_some_and(is_current) {
            s.last = sequence;
            return;
        }
        // A local application replaced our remote offer: release the obsolete
        // object, capture the new content and publish it back to the peer.
        s.own = None;
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

#[cfg(test)]
mod tests {
    use super::*;
    use multimachine_rust::network::{self, Identity, ServerOptions, Trust};
    use std::{sync::atomic::AtomicU64, time::Duration};
    use tokio::{sync::mpsc, time::timeout};
    use tokio_util::sync::CancellationToken;

    unsafe extern "system" fn test_window(w: HWND, m: u32, p: WPARAM, l: LPARAM) -> LRESULT {
        unsafe { DefWindowProcW(w, m, p, l) }
    }

    #[test]
    fn remote_then_local_clipboard_repeats_text_and_files_without_suppressing_or_clearing() {
        unsafe {
            OleInitialize(None).unwrap();
        }
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let identity = Identity::create().unwrap();
        let listen = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listen.local_addr().unwrap().port();
        drop(listen);
        let trust = Trust {
            host: "127.0.0.1".into(),
            port,
            fingerprint: identity.fingerprint(),
            secret: identity.secret.clone(),
        };
        let root = std::env::temp_dir().join(format!("mmr-clipboard-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let file = root.join("aller-retour.txt");
        std::fs::write(&file, b"copie conservee").unwrap();
        let source = Arc::new(Catalog::default());
        let local = Arc::new(Catalog::default());
        let stop = CancellationToken::new();
        let (sent, mut received) = mpsc::unbounded_channel();
        let (connected, mut peers) = mpsc::unbounded_channel();
        let task = runtime.spawn(network::serve(
            identity,
            ServerOptions {
                port,
                name: "clipboard-test".into(),
                hybrid: false,
                allow_pair: Arc::new(AtomicU64::new(0)),
                approve: Arc::new(|_| Box::pin(async { false })),
                paired: Arc::new(|| {}),
            },
            source.clone(),
            Arc::new(move |_, event| {
                if let network::Event::Clipboard(offer) = event {
                    let _ = sent.send(offer);
                }
            }),
            Arc::new(move |peer| {
                let _ = connected.send(peer);
            }),
            stop.clone(),
        ));
        let peer = runtime.block_on(async {
            for _ in 0..30 {
                if let Ok(connection) = network::connect(
                    &trust,
                    false,
                    local.clone(),
                    Arc::new(|_, _| {}),
                    stop.child_token(),
                )
                .await
                {
                    return connection.start();
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
            panic!("clipboard test connection");
        });
        let remote = runtime.block_on(async {
            timeout(Duration::from_secs(5), peers.recv())
                .await
                .unwrap()
                .unwrap()
        });
        let window = unsafe { hidden("MMR.Clipboard.Test", Some(test_window)).unwrap() };
        let origin = Uuid::new_v4();
        let source_origin = Uuid::new_v4();
        let mut state = State {
            peer: Some(peer.clone()),
            catalog: local,
            clock: Clock::new(origin),
            origin,
            runtime: runtime.handle().clone(),
            notice: Arc::new(|message| panic!("{message}")),
            own: None,
            last: 0,
            plain: false,
            pending: None,
            window,
            retries: 0,
        };
        for index in 0..32 {
            let mut offer = Offer::empty((index + 1) * 10, source_origin);
            if index % 2 == 0 {
                offer.formats.insert(
                    Format::Text,
                    Bytes(
                        wide("texte distant")
                            .iter()
                            .flat_map(|c| c.to_le_bytes())
                            .collect(),
                    ),
                );
            } else {
                offer.files = source
                    .register(offer.id, std::slice::from_ref(&file))
                    .unwrap();
            }
            assert!(state.clock.accept(&offer));
            state.pending = Some(offer);
            poll(&mut state);
            assert!(state.pending.is_none());
            assert!(is_current(state.own.as_ref().unwrap()));
            let (format, bytes) = if index % 2 == 0 {
                (
                    13,
                    wide(&format!("copie locale {index}"))
                        .iter()
                        .flat_map(|c| c.to_le_bytes())
                        .collect::<Vec<_>>(),
                )
            } else {
                let mut bytes = vec![0u8; 20];
                bytes[..4].copy_from_slice(&20u32.to_le_bytes());
                bytes[16..20].copy_from_slice(&1u32.to_le_bytes());
                bytes.extend(
                    wide(&file.to_string_lossy())
                        .iter()
                        .flat_map(|c| c.to_le_bytes()),
                );
                bytes.extend([0, 0]);
                (15, bytes)
            };
            unsafe {
                OpenClipboard(Some(window)).unwrap();
                EmptyClipboard().unwrap();
                SetClipboardData(format, Some(HANDLE(data::global(&bytes).unwrap().0))).unwrap();
                CloseClipboard().unwrap();
            }
            assert!(!is_current(state.own.as_ref().unwrap()));
            // This is the same cleanup path used when quitting: it must preserve
            // a newer copy from another application, for both text and CF_HDROP.
            let sequence = unsafe { GetClipboardSequenceNumber() };
            clear_owned(state.own.as_ref());
            assert_eq!(unsafe { GetClipboardSequenceNumber() }, sequence);
            poll(&mut state);
            let sent = runtime.block_on(async {
                timeout(Duration::from_secs(5), received.recv())
                    .await
                    .unwrap()
                    .unwrap()
            });
            assert_eq!(sent.origin, origin);
            if format == 13 {
                assert_eq!(sent.formats[&Format::Text].0, bytes);
            } else {
                assert_eq!(sent.files.len(), 1);
                assert_eq!(sent.files[0].name, "aller-retour.txt");
                let output = runtime
                    .block_on(remote.read(
                        sent.id,
                        sent.files[0].id,
                        0,
                        b"copie conservee".len() as u32,
                    ))
                    .unwrap();
                assert_eq!(output, b"copie conservee");
            }
            assert!(state.own.is_none());
        }
        assert!(file.exists());
        unsafe {
            OleSetClipboard(None::<&IDataObject>).unwrap();
            let _ = DestroyWindow(window);
        }
        drop(state);
        peer.close("test complete");
        stop.cancel();
        runtime.block_on(task).unwrap().unwrap();
        std::fs::remove_dir_all(root).unwrap();
        unsafe {
            OleUninitialize();
        }
    }
}
