use super::*;
use rustls::{
    ClientConfig, DigitallySignedStruct, ServerConfig, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    future::Future,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    pin::Pin,
};
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tokio_rustls::{TlsAcceptor, TlsConnector};

const MAGIC: &str = "multimachine-rust/1";
#[derive(Clone, Serialize, Deserialize)]
pub struct Identity {
    pub certificate: Bytes,
    pub key: Bytes,
    pub secret: Bytes,
    pub paired: bool,
}
impl Identity {
    pub fn create() -> Result<Self> {
        let cert = rcgen::generate_simple_self_signed(vec!["multimachine-rust.local".into()])?;
        Ok(Self {
            certificate: Bytes(cert.cert.der().to_vec()),
            key: Bytes(cert.key_pair.serialize_der()),
            secret: Bytes(rand::random::<[u8; 32]>().to_vec()),
            paired: false,
        })
    }
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.certificate.0)
    }
    fn tls(&self) -> Result<Arc<ServerConfig>> {
        ensure!(self.secret.0.len() == 32, "Identité serveur invalide");
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        Ok(Arc::new(
            ServerConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()?
                .with_no_client_auth()
                .with_single_cert(
                    vec![CertificateDer::from(self.certificate.0.clone())],
                    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.0.clone())),
                )?,
        ))
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Trust {
    pub host: String,
    pub port: u16,
    pub fingerprint: String,
    pub secret: Bytes,
}
pub struct PairPrompt {
    pub name: String,
    pub address: String,
    pub code: String,
}
pub struct ClientHello {
    pub fingerprint: String,
    pub code: String,
}
pub type PairAnswer =
    Arc<dyn Fn(PairPrompt) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;
pub struct ServerOptions {
    pub port: u16,
    pub name: String,
    pub hybrid: bool,
    pub allow_pair: Arc<AtomicU64>,
    pub approve: PairAnswer,
    pub paired: Arc<dyn Fn() + Send + Sync>,
}
#[derive(Serialize, Deserialize)]
struct Auth {
    magic: String,
    session: Uuid,
    channel: String,
    secret: Bytes,
    name: String,
    hybrid: bool,
}
#[derive(Serialize, Deserialize)]
struct Ack {
    accepted: bool,
    #[serde(default)]
    secret: Option<Bytes>,
    #[serde(default)]
    motion: Option<udp::Offer>,
}
fn fingerprint(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn code(pin: &str) -> String {
    pin.get(..12)
        .unwrap_or("")
        .as_bytes()
        .chunks(4)
        .map(|s| String::from_utf8_lossy(s).to_uppercase())
        .collect::<Vec<_>>()
        .join(" ")
}
#[derive(Debug)]
struct Verify {
    expected: Option<String>,
    observed: Arc<Mutex<Option<String>>>,
    algorithms: rustls::crypto::WebPkiSupportedAlgorithms,
}
impl ServerCertVerifier for Verify {
    fn verify_server_cert(
        &self,
        end: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        let pin = fingerprint(end.as_ref());
        if self
            .expected
            .as_ref()
            .is_some_and(|e| e.as_bytes().ct_eq(pin.as_bytes()).unwrap_u8() != 1)
        {
            return Err(rustls::Error::General(
                "L’identité du serveur a changé.".into(),
            ));
        }
        *self.observed.lock() = Some(pin);
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}
async fn client_channel(
    host: &str,
    port: u16,
    pin: Option<String>,
) -> Result<(tokio_rustls::client::TlsStream<TcpStream>, String)> {
    let observed = Arc::new(Mutex::new(None));
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(Verify {
        expected: pin,
        observed: observed.clone(),
        algorithms: provider.signature_verification_algorithms,
    });
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    let tcp = timeout(Duration::from_secs(8), TcpStream::connect((host, port))).await??;
    tcp.set_nodelay(true)?;
    let tls = timeout(
        Duration::from_secs(8),
        TlsConnector::from(Arc::new(config))
            .connect(ServerName::try_from("multimachine-rust.local")?, tcp),
    )
    .await??;
    let pin = observed.lock().clone().context("Certificat manquant")?;
    Ok((tls, pin))
}
pub async fn pair(
    host: String,
    port: u16,
    name: String,
    on_hello: Arc<dyn Fn(ClientHello) + Send + Sync>,
    stop: CancellationToken,
) -> Result<Trust> {
    let work = async {
        let (mut tls, pin) = client_channel(&host, port, None).await?;
        on_hello(ClientHello {
            code: code(&pin),
            fingerprint: pin.clone(),
        });
        write_frame(
            &mut tls,
            &Auth {
                magic: MAGIC.into(),
                session: Uuid::new_v4(),
                channel: "pair".into(),
                secret: Bytes(vec![]),
                name,
                hybrid: false,
            },
            8192,
        )
        .await?;
        let ack: Ack = read_frame(&mut tls, 8192).await?;
        ensure!(
            ack.accepted,
            "Association refusée. Autorisez la nouvelle association sur le serveur."
        );
        let secret = ack.secret.context("Secret d’association absent")?;
        ensure!(secret.0.len() == 32, "Association invalide");
        Ok(Trust {
            host,
            port,
            fingerprint: pin,
            secret,
        })
    };
    tokio::select! {r=timeout(Duration::from_secs(120),work)=>r.context("Association expirée")?,_=stop.cancelled()=>Err(anyhow::anyhow!("Association annulée"))}
}
pub async fn connect(
    trust: &Trust,
    hybrid: bool,
    catalog: Arc<Catalog>,
    sink: Sink,
    stop: CancellationToken,
) -> Result<Connection> {
    let session = Uuid::new_v4();
    let work = async {
        ensure!(
            trust.secret.0.len() == 32 && trust.fingerprint.len() == 64,
            "Association mémorisée invalide"
        );
        let (mut control, _) =
            client_channel(&trust.host, trust.port, Some(trust.fingerprint.clone())).await?;
        let auth = Auth {
            magic: MAGIC.into(),
            session,
            channel: "control".into(),
            secret: trust.secret.clone(),
            name: String::new(),
            hybrid,
        };
        write_frame(&mut control, &auth, 8192).await?;
        let ack: Ack = read_frame(&mut control, 8192).await?;
        ensure!(ack.accepted, "Connexion refusée");
        let (mut bulk, _) =
            client_channel(&trust.host, trust.port, Some(trust.fingerprint.clone())).await?;
        write_frame(
            &mut bulk,
            &Auth {
                channel: "bulk".into(),
                ..auth
            },
            8192,
        )
        .await?;
        let ack: Ack = read_frame(&mut bulk, 8192).await?;
        ensure!(ack.accepted, "Connexion refusée");
        let negotiated = hybrid && ack.motion.is_some();
        let path = if let Some(offer) = ack.motion {
            ensure!(
                negotiated && offer.session == session && offer.port >= 1024,
                "Offre UDP invalide"
            );
            let tcp = bulk.get_ref().0;
            udp::MotionPath::client(tcp.local_addr()?.ip(), tcp.peer_addr()?.ip(), &offer).await
        } else {
            None
        };
        Ok(Connection::new(
            control.into(),
            bulk.into(),
            session,
            negotiated,
            path,
            catalog,
            sink,
        ))
    };
    tokio::select! {r=timeout(Duration::from_secs(20),work)=>r.context("Connexion expirée")?,_=stop.cancelled()=>Err(anyhow::anyhow!("Connexion annulée"))}
}

/// Listening roles never reverse when physical ownership changes. Only one paired session is active.
pub async fn serve(
    identity: Identity,
    options: ServerOptions,
    catalog: Arc<Catalog>,
    sink: Sink,
    connected: Arc<dyn Fn(Arc<Peer>) + Send + Sync>,
    stop: CancellationToken,
) -> Result<()> {
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, options.port)).await?;
    let acceptor = TlsAcceptor::from(identity.tls()?);
    let port = listener.local_addr()?.port();
    let discovery_stop = stop.child_token();
    let discovery = tokio::spawn(discovery::respond(
        options.name.clone(),
        port,
        identity.fingerprint(),
        discovery_stop.clone(),
    ));
    let mut first: Option<(Uuid, IpAddr, bool, TlsStream<TcpStream>, u64)> = None;
    let mut active: Option<Arc<Peer>> = None;
    let mut paired = identity.paired;
    let result:Result<()>=async{loop{
        let (tcp,address)=tokio::select!{_=stop.cancelled()=>break,r=listener.accept()=>r?};tcp.set_nodelay(true)?;
        let handshake=async{
            let mut tls=timeout(Duration::from_secs(8),acceptor.accept(tcp)).await??;let auth:Auth=timeout(Duration::from_secs(8),read_frame(&mut tls,8192)).await??;
            ensure!(auth.magic==MAGIC&&!auth.session.is_nil()&&auth.name.len()<=1024,"Protocole incompatible");
            if auth.channel=="pair"{
                let allowed=!paired||now()<options.allow_pair.load(Ordering::Relaxed);
                let accepted=allowed&&(options.approve)(PairPrompt{name:auth.name,address:address.ip().to_string(),code:code(&identity.fingerprint())}).await;
                write_frame(&mut tls,&Ack{accepted,secret:accepted.then(||identity.secret.clone()),motion:None},8192).await?;
                if accepted{paired=true;(options.paired)();}return Ok(());
            }
            ensure!(auth.secret.0.len()==32&&auth.secret.0.ct_eq(&identity.secret.0).unwrap_u8()==1,"Association invalide");
            ensure!(active.as_ref().is_none_or(|p|!p.connected()),"Un client est déjà connecté");
            if auth.channel=="control"{write_frame(&mut tls,&Ack{accepted:true,secret:None,motion:None},8192).await?;first=Some((auth.session,address.ip(),auth.hybrid&&options.hybrid,tls.into(),now()));}
            else if auth.channel=="bulk"{
                let (session,ip,hybrid,control,since)=first.take().context("Canal de contrôle manquant")?;ensure!(session==auth.session&&ip==address.ip()&&now().saturating_sub(since)<12000,"Canaux non associés");
                let udp=if hybrid&&auth.hybrid{udp::MotionPath::server(SocketAddr::new(tls.get_ref().0.local_addr()?.ip(),port),ip,session).await}else{None};
                write_frame(&mut tls,&Ack{accepted:true,secret:None,motion:udp.as_ref().map(|p|p.1.clone())},8192).await?;
                let connection=Connection::new(control,tls.into(),session,udp.is_some(),udp.map(|p|p.0),catalog.clone(),sink.clone());connected(connection.peer.clone());active=Some(connection.start());
            }else{anyhow::bail!("Canal inconnu")}
            Ok::<_,anyhow::Error>(())
        };
        // Approval is deliberately bounded; cancellation interrupts the listener and dialogs.
        tokio::select!{_=stop.cancelled()=>break,_=timeout(Duration::from_secs(125),handshake)=>{}}
    }Ok(())}.await;
    if let Some(peer) = active {
        peer.close("Serveur arrêté");
    }
    discovery_stop.cancel();
    let _ = discovery.await;
    result
}
