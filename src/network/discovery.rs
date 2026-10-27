use super::*;
use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, SocketAddr};
use tokio::net::UdpSocket;
#[derive(Serialize, Deserialize)]
struct Query {
    magic: String,
    nonce: Uuid,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Server {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub fingerprint: String,
}
#[derive(Serialize, Deserialize)]
struct Reply {
    magic: String,
    nonce: Uuid,
    name: String,
    port: u16,
    fingerprint: String,
}
pub async fn respond(
    name: String,
    port: u16,
    fingerprint: String,
    stop: CancellationToken,
) -> Result<()> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, DISCOVERY_PORT)).await?;
    let mut bytes = [0u8; 4096];
    loop {
        let (n, from) =
            tokio::select! {_=stop.cancelled()=>return Ok(()),r=socket.recv_from(&mut bytes)=>r?};
        if let Ok(q) = serde_json::from_slice::<Query>(&bytes[..n]) {
            if q.magic == "MMR-discover-1" && !q.nonce.is_nil() {
                let reply = Reply {
                    magic: q.magic,
                    nonce: q.nonce,
                    name: name.clone(),
                    port,
                    fingerprint: fingerprint.clone(),
                };
                socket.send_to(&serde_json::to_vec(&reply)?, from).await?;
            }
        }
    }
}
pub async fn find() -> Result<Vec<Server>> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    socket.set_broadcast(true)?;
    let nonce = Uuid::new_v4();
    let query = serde_json::to_vec(&Query {
        magic: "MMR-discover-1".into(),
        nonce,
    })?;
    socket
        .send_to(
            &query,
            SocketAddr::from((Ipv4Addr::BROADCAST, DISCOVERY_PORT)),
        )
        .await?;
    let mut bytes = [0u8; 4096];
    let mut servers = vec![];
    let end = tokio::time::Instant::now() + Duration::from_millis(1400);
    while let Ok(Ok((n, addr))) = tokio::time::timeout_at(end, socket.recv_from(&mut bytes)).await {
        if let Ok(r) = serde_json::from_slice::<Reply>(&bytes[..n]) {
            if r.magic == "MMR-discover-1"
                && r.nonce == nonce
                && r.name.len() <= 1024
                && r.port >= 1024
                && r.fingerprint.len() == 64
                && r.fingerprint.bytes().all(|c| c.is_ascii_hexdigit())
                && !servers.iter().any(|s: &Server| {
                    s.host == addr.ip().to_string() && s.fingerprint == r.fingerprint
                })
            {
                servers.push(Server {
                    name: r.name,
                    host: addr.ip().to_string(),
                    port: r.port,
                    fingerprint: r.fingerprint,
                });
            }
        }
    }
    Ok(servers)
}
