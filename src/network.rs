pub mod discovery;
mod health;
mod tls;
mod udp;
use crate::{
    files::Catalog,
    hybrid::{Frame, Receiver, Sender},
    model::*,
};
use anyhow::{Context, Result, ensure};
use parking_lot::Mutex;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
pub use tls::{
    ClientHello, Identity, PairAnswer, PairPrompt, ServerOptions, Trust, connect, pair, serve,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    sync::{Notify, Semaphore, mpsc, oneshot},
    time::timeout,
};
use tokio_rustls::TlsStream;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub fn now() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}
pub async fn write_frame<W: AsyncWrite + Unpin, T: Serialize>(
    stream: &mut W,
    value: &T,
    limit: usize,
) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() <= limit, "Trame trop grande");
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}
pub async fn read_frame<R: AsyncRead + Unpin, T: DeserializeOwned>(
    stream: &mut R,
    limit: usize,
) -> Result<T> {
    let n = stream.read_u32().await? as usize;
    ensure!(n > 0 && n <= limit, "Taille de trame refusée");
    let mut bytes = vec![0; n];
    stream.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub enum Event {
    Control(Control),
    Clipboard(Offer),
    Drag(Offer),
    Closed(String),
}
pub type Sink = Arc<dyn Fn(Uuid, Event) + Send + Sync>;
struct Outgoing {
    sender: Option<Sender>,
    queue: VecDeque<Frame>,
    coalesced: u64,
}
pub struct Peer {
    pub id: Uuid,
    stop: CancellationToken,
    closing: AtomicBool,
    tx: Mutex<Outgoing>,
    ready: Notify,
    rx: Mutex<Option<Receiver>>,
    bulk: mpsc::Sender<Bulk>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Vec<u8>>>>>,
    requests: AtomicU64,
    reads: Semaphore,
    catalog: Arc<Catalog>,
    sink: Sink,
    udp: Option<Arc<udp::MotionPath>>,
    pong: AtomicU64,
    rtt: AtomicU64,
    activity: AtomicU64,
    health_ready: Notify,
    control_sent: AtomicU64,
    held: Mutex<health::Held>,
    sensitive: AtomicBool,
}
pub struct Connection {
    pub peer: Arc<Peer>,
    control: TlsStream<TcpStream>,
    bulk: TlsStream<TcpStream>,
    bulk_rx: mpsc::Receiver<Bulk>,
}
impl Connection {
    fn new(
        control: TlsStream<TcpStream>,
        bulk: TlsStream<TcpStream>,
        id: Uuid,
        hybrid: bool,
        udp: Option<Arc<udp::MotionPath>>,
        catalog: Arc<Catalog>,
        sink: Sink,
    ) -> Self {
        let (bulk_tx, bulk_rx) = mpsc::channel(32);
        let peer = Arc::new(Peer {
            id,
            stop: CancellationToken::new(),
            closing: AtomicBool::new(false),
            tx: Mutex::new(Outgoing {
                sender: hybrid.then(Sender::default),
                queue: VecDeque::new(),
                coalesced: 0,
            }),
            ready: Notify::new(),
            rx: Mutex::new(hybrid.then(Receiver::default)),
            bulk: bulk_tx,
            pending: Mutex::new(HashMap::new()),
            requests: AtomicU64::new(1),
            reads: Semaphore::new(16),
            catalog,
            sink,
            udp,
            pong: AtomicU64::new(0),
            rtt: AtomicU64::new(u64::MAX),
            activity: AtomicU64::new(now()),
            health_ready: Notify::new(),
            control_sent: AtomicU64::new(0),
            held: Mutex::new(health::Held::default()),
            sensitive: AtomicBool::new(false),
        });
        Self {
            peer,
            control,
            bulk,
            bulk_rx,
        }
    }
    pub fn start(self) -> Arc<Peer> {
        let Self {
            peer,
            control,
            bulk,
            mut bulk_rx,
        } = self;
        let (mut cr, mut cw) = tokio::io::split(control);
        let (mut br, mut bw) = tokio::io::split(bulk);
        let p = peer.clone();
        guard(peer.clone(), async move {
            loop {
                let frame: Frame = read_frame(&mut cr, CONTROL_FRAME).await?;
                ensure!(
                    !matches!(frame, Frame::Motion { .. }),
                    "Mouvement non fiable sur TCP"
                );
                p.accept(frame)?;
            }
        });
        let p = peer.clone();
        guard(peer.clone(), async move {
            loop {
                let (item, deadline) = {
                    let mut tx = p.tx.lock();
                    let item = tx
                        .queue
                        .pop_front()
                        .or_else(|| tx.sender.as_mut().and_then(|s| s.checkpoint(now())));
                    let deadline = tx.sender.as_ref().and_then(Sender::checkpoint_deadline);
                    (item, deadline)
                };
                if let Some(frame) = item {
                    let movement = frame.motion_key().is_some();
                    if let Frame::Motion { motion } = frame {
                        let sent = if let Some(path) = &p.udp {
                            path.send_motion(&motion).await
                        } else {
                            false
                        };
                        if !sent {
                            write_frame(&mut cw, &Frame::Final { motion }, CONTROL_FRAME).await?;
                            p.control_sent.fetch_add(1, Ordering::Relaxed);
                        }
                    } else {
                        write_frame(&mut cw, &frame, CONTROL_FRAME).await?;
                        p.control_sent.fetch_add(1, Ordering::Relaxed);
                    }
                    if movement {
                        tokio::time::sleep(Duration::from_millis(4)).await;
                    }
                } else {
                    if let Some(deadline) = deadline {
                        tokio::select! {
                            _ = p.ready.notified() => {},
                            _ = tokio::time::sleep(Duration::from_millis(deadline.saturating_sub(now()))) => {}
                        }
                    } else {
                        p.ready.notified().await;
                    }
                }
            }
        });
        guard(peer.clone(), async move {
            while let Some(message) = bulk_rx.recv().await {
                write_frame(&mut bw, &message, BULK_FRAME).await?;
            }
            Ok(())
        });
        let p = peer.clone();
        guard(peer.clone(), async move {
            loop {
                let message: Bulk = read_frame(&mut br, BULK_FRAME).await?;
                message.validate()?;
                p.touch();
                match message {
                    Bulk::Clipboard { offer } => (p.sink)(p.id, Event::Clipboard(offer)),
                    Bulk::Drag { offer } => (p.sink)(p.id, Event::Drag(offer)),
                    Bulk::Data {
                        request,
                        bytes,
                        error,
                    } => {
                        if let Some(tx) = p.pending.lock().remove(&request) {
                            let result = match error {
                                Some(e) => Err(anyhow::anyhow!(e)),
                                None => Ok(bytes.0),
                            };
                            let _ = tx.send(result);
                        }
                    }
                    Bulk::Read {
                        request,
                        offer,
                        file,
                        offset,
                        count,
                    } => {
                        let catalog = p.catalog.clone();
                        let result = tokio::task::spawn_blocking(move || {
                            catalog.read(offer, file, offset, count as usize)
                        })
                        .await?;
                        let (bytes, error) = match result {
                            Ok(b) => (b, None),
                            Err(_) => (
                                vec![],
                                Some(
                                    "La source est inaccessible, modifiée ou son offre a expiré."
                                        .into(),
                                ),
                            ),
                        };
                        p.send_bulk(Bulk::Data {
                            request,
                            bytes: Bytes(bytes),
                            error,
                        });
                    }
                }
            }
        });
        let p = peer.clone();
        guard(peer.clone(), async move {
            let mut health = health::Health::default();
            loop {
                let tick = now();
                let step = health
                    .step(
                        tick,
                        p.active() || p.sensitive.load(Ordering::Relaxed),
                        p.pong.load(Ordering::Relaxed),
                    )
                    .context("Connexion sans réponse")?;
                if step.probe {
                    p.send(Control::Ping {
                        id: tick,
                        reply: false,
                    });
                }
                tokio::select! {
                    _ = p.health_ready.notified() => {},
                    _ = tokio::time::sleep(Duration::from_millis(step.wake_at.saturating_sub(now()))) => {}
                }
            }
        });
        if let Some(path) = &peer.udp {
            path.start(Arc::downgrade(&peer));
        }
        peer
    }
}
fn guard(peer: Arc<Peer>, fut: impl std::future::Future<Output = Result<()>> + Send + 'static) {
    tokio::spawn(async move {
        tokio::select! {_ =peer.stop.cancelled()=>{},r=fut=>{peer.close(if r.is_ok(){"Connexion fermée"}else{"Connexion interrompue"});}}
    });
}
impl Peer {
    fn observe(&self, event: &Control) {
        // Keep fast failure detection while an input can remain pressed remotely
        // or an OLE gesture is in flight, even if the pointer is stationary.
        if !matches!(
            event,
            Control::Input {
                input: Input::Move { .. } | Input::Wheel { .. },
                ..
            }
        ) {
            let mut held = self.held.lock();
            self.sensitive.store(held.observe(event), Ordering::Relaxed);
        }
        self.touch();
    }
    fn touch(&self) {
        let tick = now();
        let previous = self.activity.swap(tick, Ordering::Relaxed);
        if tick.saturating_sub(previous) >= health::ACTIVE_GRACE_MS {
            self.health_ready.notify_one();
        }
        if let Some(path) = &self.udp {
            path.touch();
        }
    }
    pub fn active(&self) -> bool {
        now().saturating_sub(self.activity.load(Ordering::Relaxed)) < health::ACTIVE_GRACE_MS
    }
    pub fn sent_packets(&self) -> (u64, u64) {
        (
            self.control_sent.load(Ordering::Relaxed),
            self.udp
                .as_ref()
                .map_or(0, |path| path.packets.load(Ordering::Relaxed)),
        )
    }
    pub fn connected(&self) -> bool {
        !self.stop.is_cancelled()
    }
    pub fn hybrid(&self) -> bool {
        self.rx.lock().is_some()
    }
    pub fn transport(&self) -> &'static str {
        if !self.hybrid() {
            "TCP classique"
        } else if !self.active() {
            "Hybride au repos"
        } else if self.udp.as_ref().is_some_and(|p| p.ready()) {
            "Hybride actif"
        } else {
            "Secours TCP"
        }
    }
    pub fn stats(&self) -> (usize, u64, u64, u64, Option<u64>) {
        let t = self.tx.lock();
        let r = self.rtt.load(Ordering::Relaxed);
        (
            t.queue.len(),
            t.coalesced,
            self.udp
                .as_ref()
                .map_or(0, |p| p.sent.load(Ordering::Relaxed)),
            self.udp
                .as_ref()
                .map_or(0, |p| p.accepted.load(Ordering::Relaxed)),
            (r != u64::MAX).then_some(r),
        )
    }
    pub fn send(&self, event: Control) -> bool {
        if !self.connected() {
            return false;
        }
        if event.validate().is_err() {
            self.close("Entrée invalide");
            return false;
        }
        if !matches!(event, Control::Ping { .. }) {
            self.observe(&event);
        }
        let mut tx = self.tx.lock();
        let frame = match &mut tx.sender {
            Some(s) => match s.prepare(event, now()) {
                Ok(f) => f,
                Err(_) => {
                    drop(tx);
                    self.close("Ordre d’entrée invalide");
                    return false;
                }
            },
            None => Frame::Plain { event },
        };
        if let Some(lease) = frame.motion_key() {
            if tx
                .queue
                .back()
                .is_some_and(|f| f.motion_key() == Some(lease))
            {
                *tx.queue.back_mut().unwrap() = frame;
                tx.coalesced += 1;
                return true;
            }
        }
        if tx.queue.len() >= 4096 {
            drop(tx);
            self.close("File de contrôle saturée");
            return false;
        }
        tx.queue.push_back(frame);
        drop(tx);
        self.ready.notify_one();
        true
    }
    fn accept(&self, frame: Frame) -> Result<()> {
        // This lock includes dispatch, so UDP cannot overtake an action/anchor pair.
        let mut receiver = self.rx.lock();
        let events = if let Some(rx) = receiver.as_mut() {
            rx.accept(frame)?
        } else {
            match frame {
                Frame::Plain { event } => {
                    event.validate()?;
                    vec![event]
                }
                _ => anyhow::bail!("Format de transport inattendu"),
            }
        };
        for event in events {
            match event {
                Control::Ping { id, reply: true } => {
                    ensure!(id <= now(), "Réponse de contrôle invalide");
                    self.pong.fetch_max(id, Ordering::Relaxed);
                    self.rtt.store(now().saturating_sub(id), Ordering::Relaxed);
                    self.health_ready.notify_one();
                }
                Control::Ping { id, reply: false } => {
                    self.send(Control::Ping { id, reply: true });
                }
                other => {
                    self.observe(&other);
                    (self.sink)(self.id, Event::Control(other));
                }
            }
        }
        Ok(())
    }
    pub fn send_bulk(&self, message: Bulk) -> bool {
        if !self.connected() {
            return false;
        }
        self.touch();
        if message.validate().is_err() || self.bulk.try_send(message).is_err() {
            self.close("Canal de transfert saturé ou invalide");
            return false;
        }
        true
    }
    pub async fn read(&self, offer: Uuid, file: Uuid, offset: u64, count: u32) -> Result<Vec<u8>> {
        ensure!(self.connected(), "Source déconnectée");
        let _slot = timeout(Duration::from_secs(30), self.reads.acquire()).await??;
        let request = self.requests.fetch_add(1, Ordering::Relaxed);
        ensure!(request != u64::MAX, "Compteur épuisé");
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(request, tx);
        if !self.send_bulk(Bulk::Read {
            request,
            offer,
            file,
            offset,
            count,
        }) {
            self.pending.lock().remove(&request);
            anyhow::bail!("Source déconnectée")
        }
        let r = tokio::select! {r=timeout(Duration::from_secs(30),rx)=>r.context("Lecture expirée").and_then(|r|r.context("Lecture annulée")).and_then(|r|r),_=self.stop.cancelled()=>Err(anyhow::anyhow!("Lecture interrompue"))};
        self.pending.lock().remove(&request);
        r
    }
    pub async fn closed(&self) {
        self.stop.cancelled().await
    }
    pub fn close(&self, reason: &str) {
        if self.closing.swap(true, Ordering::AcqRel) {
            return;
        }
        self.stop.cancel();
        if let Some(path) = &self.udp {
            path.close();
        }
        self.tx.lock().queue.clear();
        for (_, tx) in self.pending.lock().drain() {
            let _ = tx.send(Err(anyhow::anyhow!("Source déconnectée")));
        }
        (self.sink)(self.id, Event::Closed(reason.into()));
    }
}
