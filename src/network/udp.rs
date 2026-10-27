use super::*;
use crate::hybrid::{DATAGRAM_SIZE, Datagram, Motion};
use serde::{Deserialize, Serialize};
use std::{
    net::{IpAddr, SocketAddr},
    sync::{Weak, atomic::AtomicBool},
};
use tokio::net::UdpSocket;
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Offer {
    pub session: Uuid,
    pub port: u16,
    pub client: [u8; 32],
    pub server: [u8; 32],
}
pub(super) struct MotionPath {
    socket: Mutex<Option<Arc<UdpSocket>>>,
    ip: IpAddr,
    endpoint: Mutex<Option<SocketAddr>>,
    send: Mutex<Datagram>,
    receive: Mutex<Datagram>,
    ack: AtomicU64,
    acked: AtomicU64,
    probe: AtomicU64,
    started: AtomicBool,
    pub stop: CancellationToken,
    pub sent: AtomicU64,
    pub accepted: AtomicU64,
    pub packets: AtomicU64,
    activity: AtomicU64,
    wake: Notify,
}
fn normalize(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v) => v.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        _ => ip,
    }
}
impl MotionPath {
    fn new(
        socket: UdpSocket,
        ip: IpAddr,
        endpoint: Option<SocketAddr>,
        offer: &Offer,
        server: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            socket: Mutex::new(Some(Arc::new(socket))),
            ip: normalize(ip),
            endpoint: Mutex::new(endpoint),
            send: Mutex::new(Datagram::new(
                offer.session,
                if server { &offer.server } else { &offer.client },
            )),
            receive: Mutex::new(Datagram::new(
                offer.session,
                if server { &offer.client } else { &offer.server },
            )),
            ack: AtomicU64::new(0),
            acked: AtomicU64::new(0),
            probe: AtomicU64::new(0),
            started: AtomicBool::new(false),
            stop: CancellationToken::new(),
            sent: AtomicU64::new(0),
            accepted: AtomicU64::new(0),
            packets: AtomicU64::new(0),
            activity: AtomicU64::new(now()),
            wake: Notify::new(),
        })
    }
    pub async fn server(
        local: SocketAddr,
        remote: IpAddr,
        session: Uuid,
    ) -> Option<(Arc<Self>, Offer)> {
        let socket = UdpSocket::bind(SocketAddr::new(normalize(local.ip()), local.port()))
            .await
            .ok()?;
        let offer = Offer {
            session,
            port: socket.local_addr().ok()?.port(),
            client: rand::random(),
            server: rand::random(),
        };
        Some((Self::new(socket, remote, None, &offer, true), offer))
    }
    pub async fn client(local: IpAddr, remote: IpAddr, offer: &Offer) -> Option<Arc<Self>> {
        let socket = UdpSocket::bind(SocketAddr::new(normalize(local), 0))
            .await
            .ok()?;
        Some(Self::new(
            socket,
            remote,
            Some(SocketAddr::new(normalize(remote), offer.port)),
            offer,
            false,
        ))
    }
    pub fn close(&self) {
        self.stop.cancel();
        self.socket.lock().take();
    }
    pub fn touch(&self) {
        let tick = now();
        let previous = self.activity.swap(tick, Ordering::Relaxed);
        if tick.saturating_sub(previous) >= health::ACTIVE_GRACE_MS {
            self.wake.notify_one();
        }
    }
    pub fn ready(&self) -> bool {
        let ack = self.ack.load(Ordering::Relaxed);
        !self.stop.is_cancelled() && ack > 0 && now().saturating_sub(ack) < 350
    }
    async fn send(&self, kind: u8, frame: &Motion) -> bool {
        let Some(target) = *self.endpoint.lock() else {
            return false;
        };
        if self.stop.is_cancelled() {
            return false;
        }
        let Ok(bytes) = self.send.lock().encode(kind, frame) else {
            self.stop.cancel();
            return false;
        };
        let Some(socket) = self.socket.lock().clone() else {
            return false;
        };
        let result = tokio::select! {r=socket.send_to(&bytes,target)=>r.is_ok(),_=self.stop.cancelled()=>false};
        if !result {
            self.ack.store(0, Ordering::Relaxed)
        } else {
            self.packets.fetch_add(1, Ordering::Relaxed);
        }
        result
    }
    pub async fn send_motion(&self, m: &Motion) -> bool {
        if !self.ready() {
            return false;
        }
        let ok = self.send(3, m).await;
        if ok {
            self.sent.fetch_add(1, Ordering::Relaxed);
        }
        ok
    }
    pub fn start(self: &Arc<Self>, peer: Weak<Peer>) {
        if self.started.swap(true, Ordering::Relaxed) {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            let mut last = 0;
            loop {
                let tick = now();
                let active_until = this
                    .activity
                    .load(Ordering::Relaxed)
                    .saturating_add(health::ACTIVE_GRACE_MS);
                if tick >= active_until {
                    tokio::select! { _=this.stop.cancelled()=>break, _=this.wake.notified()=>{} }
                    // Probe immediately on renewed use, even if the previous path expired.
                    last = 0;
                    continue;
                }
                let interval = if this.ready() { 100 } else { 1000 };
                if last == 0 || tick.saturating_sub(last) >= interval {
                    last = tick;
                    let sequence = this.probe.fetch_add(1, Ordering::Relaxed) + 1;
                    this.send(
                        1,
                        &Motion {
                            lease: Uuid::nil(),
                            sequence,
                            after: tick,
                            position: Point::default(),
                        },
                    )
                    .await;
                }
                let deadline = active_until.min(last.saturating_add(interval));
                tokio::select! {
                    _=this.stop.cancelled()=>break,
                    _=this.wake.notified()=>{},
                    _=tokio::time::sleep(Duration::from_millis(deadline.saturating_sub(now())))=>{}
                }
            }
        });
        let this = self.clone();
        tokio::spawn(async move {
            let mut bytes = [0u8; DATAGRAM_SIZE + 1];
            loop {
                let Some(socket) = this.socket.lock().clone() else {
                    break;
                };
                let packet = tokio::select! {_=this.stop.cancelled()=>break,r=socket.recv_from(&mut bytes)=>r};
                let Ok((count, mut address)) = packet else {
                    this.ack.store(0, Ordering::Relaxed);
                    tokio::select! {_=this.stop.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(1))=>{}}
                    continue;
                };
                address.set_ip(normalize(address.ip()));
                if address.ip() != this.ip || this.endpoint.lock().is_some_and(|a| a != address) {
                    continue;
                }
                let Some((kind, frame)) = this.receive.lock().decode(&bytes[..count]) else {
                    continue;
                };
                {
                    let mut endpoint = this.endpoint.lock();
                    if endpoint.is_none() {
                        if kind != 1 {
                            continue;
                        }
                        *endpoint = Some(address);
                    }
                }
                match kind {
                    1 => {
                        this.send(2, &frame).await;
                    }
                    2 => {
                        let n = now();
                        if frame.after <= n
                            && n - frame.after < 350
                            && frame.sequence > this.acked.load(Ordering::Relaxed)
                            && frame.sequence <= this.probe.load(Ordering::Relaxed)
                        {
                            this.acked.store(frame.sequence, Ordering::Relaxed);
                            this.ack.store(n, Ordering::Relaxed);
                            this.wake.notify_one();
                        }
                    }
                    3 => {
                        if frame.validate().is_err() {
                            continue;
                        }
                        let Some(p) = peer.upgrade() else { break };
                        this.accepted.fetch_add(1, Ordering::Relaxed);
                        if p.accept(Frame::Motion { motion: frame }).is_err() {
                            p.close("Mouvement invalide");
                            break;
                        }
                    }
                    _ => (),
                }
            }
        });
    }
}
