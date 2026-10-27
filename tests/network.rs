use multimachine_rust::{files::Catalog, model::*, network::*};
use parking_lot::Mutex;
use std::{
    fs,
    io::Write,
    sync::{Arc, atomic::AtomicU64},
    time::Duration,
};
use tokio::{sync::mpsc, time::timeout};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
fn sink() -> (Sink, mpsc::UnboundedReceiver<Event>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (
        Arc::new(move |_, event| {
            let _ = tx.send(event);
        }),
        rx,
    )
}
async fn next(rx: &mut mpsc::UnboundedReceiver<Event>) -> Event {
    timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
        .unwrap()
}
async fn control(rx: &mut mpsc::UnboundedReceiver<Event>) -> Control {
    loop {
        if let Event::Control(c) = next(rx).await {
            return c;
        }
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tls_pairing_udp_tcp_bulk_and_reconnect() {
    let identity = Identity::create().unwrap();
    let fingerprint = identity.fingerprint();
    let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = held.local_addr().unwrap().port();
    drop(held);
    let stop = CancellationToken::new();
    let (server_sink, mut server_events) = sink();
    let (client_sink, mut client_events) = sink();
    let server_catalog = Arc::new(Catalog::default());
    let client_catalog = Arc::new(Catalog::default());
    let (tx, mut peers) = mpsc::unbounded_channel();
    let approve: PairAnswer = Arc::new(|p| {
        assert_eq!(p.code.len(), 14);
        Box::pin(async { true })
    });
    let options = ServerOptions {
        port,
        name: "test-server".into(),
        hybrid: true,
        allow_pair: Arc::new(AtomicU64::new(0)),
        approve,
        paired: Arc::new(|| {}),
    };
    let server_stop = stop.clone();
    let catalog = server_catalog.clone();
    let server = tokio::spawn(async move {
        serve(
            identity,
            options,
            catalog,
            server_sink,
            Arc::new(move |p| {
                tx.send(p).unwrap();
            }),
            server_stop,
        )
        .await
    });
    let observed = Arc::new(Mutex::new(None));
    let copy = observed.clone();
    let mut trust = None;
    for _ in 0..30 {
        match pair(
            "127.0.0.1".into(),
            port,
            "test-client".into(),
            Arc::new({
                let copy = copy.clone();
                move |h| *copy.lock() = Some(h.fingerprint)
            }),
            stop.child_token(),
        )
        .await
        {
            Ok(t) => {
                trust = Some(t);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(30)).await,
        }
    }
    let trust = trust.expect("pairing");
    assert_eq!(trust.fingerprint, fingerprint);
    assert_eq!(*observed.lock(), Some(fingerprint));
    // Pairing is not silently reopened once the first client was accepted.
    assert!(
        pair(
            "127.0.0.1".into(),
            port,
            "unknown".into(),
            Arc::new(|_| {}),
            stop.child_token()
        )
        .await
        .is_err()
    );
    let mut wrong = trust.clone();
    wrong.fingerprint = "0".repeat(64);
    assert!(
        connect(
            &wrong,
            true,
            client_catalog.clone(),
            client_sink.clone(),
            stop.child_token()
        )
        .await
        .is_err()
    );
    wrong = trust.clone();
    wrong.secret = Bytes(vec![0; 32]);
    assert!(
        connect(
            &wrong,
            true,
            client_catalog.clone(),
            client_sink.clone(),
            stop.child_token()
        )
        .await
        .is_err()
    );
    let client = connect(
        &trust,
        true,
        client_catalog.clone(),
        client_sink.clone(),
        stop.child_token(),
    )
    .await
    .unwrap()
    .start();
    let remote = timeout(Duration::from_secs(5), peers.recv())
        .await
        .unwrap()
        .unwrap();
    let lease = Uuid::new_v4();
    client.send(Control::Begin {
        lease,
        position: Point::new(3, 4),
    });
    assert!(matches!(
        control(&mut server_events).await,
        Control::Begin {
            position: Point { x: 3, y: 4 },
            ..
        }
    ));
    for _ in 0..40 {
        if client.transport() == "Hybride actif" && remote.transport() == "Hybride actif" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(client.transport(), "Hybride actif");
    assert_eq!(remote.transport(), "Hybride actif");
    for n in 0..1000 {
        client.send(Control::Input {
            lease,
            input: Input::Move {
                position: Point::new(n, 50),
            },
        });
    }
    client.send(Control::Input {
        lease,
        input: Input::Button {
            button: 1,
            up: false,
        },
    });
    let mut position = Point::default();
    loop {
        match control(&mut server_events).await {
            Control::Input {
                input: Input::Move { position: p },
                ..
            } => position = p,
            Control::Input {
                input: Input::Button { up: false, .. },
                ..
            } => {
                assert_eq!(position, Point::new(999, 50));
                break;
            }
            _ => (),
        }
    }
    assert!(client.stats().1 > 0);
    assert!(client.stats().2 > 0);
    client.send(Control::Input {
        lease,
        input: Input::Button {
            button: 1,
            up: true,
        },
    });
    assert!(matches!(
        control(&mut server_events).await,
        Control::Input {
            input: Input::Button { up: true, .. },
            ..
        }
    ));
    // Observe actual control frames AND UDP probes, not only motion counters.
    // Keeping the session connected must not keep 100 ms/1 s timers transmitting.
    tokio::time::sleep(Duration::from_millis(3000)).await;
    assert!(!client.active() && !remote.active());
    assert_eq!(client.transport(), "Hybride au repos");
    let before = (client.sent_packets(), remote.sent_packets());
    tokio::time::sleep(Duration::from_millis(2100)).await;
    assert_eq!(before, (client.sent_packets(), remote.sent_packets()));
    assert!(client.connected() && remote.connected());
    // A resumed operation is delivered immediately over TCP while UDP reopens.
    client.send(Control::Input {
        lease,
        input: Input::Move {
            position: Point::new(1001, 51),
        },
    });
    let resumed = timeout(Duration::from_secs(1), control(&mut server_events))
        .await
        .unwrap();
    assert!(matches!(
        resumed,
        Control::Input {
            input: Input::Move {
                position: Point { x: 1001, y: 51 }
            },
            ..
        }
    ));
    for _ in 0..30 {
        if client.transport() == "Hybride actif" && remote.transport() == "Hybride actif" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(client.transport(), "Hybride actif");
    assert_eq!(remote.transport(), "Hybride actif");
    let temp = std::env::temp_dir().join(format!("mmr-net-{}", Uuid::new_v4()));
    fs::create_dir(&temp).unwrap();
    let file = temp.join("transfer.bin");
    let mut output = fs::File::create(&file).unwrap();
    let pattern = (0..CHUNK).map(|n| (n % 251) as u8).collect::<Vec<_>>();
    for _ in 0..256 {
        output.write_all(&pattern).unwrap();
    }
    drop(output);
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let entry_a = server_catalog
        .register(a, std::slice::from_ref(&file))
        .unwrap()[0]
        .clone();
    let entry_b = client_catalog
        .register(b, std::slice::from_ref(&file))
        .unwrap()[0]
        .clone();
    let mut tasks = vec![];
    for task in 0..32 {
        let peer = if task % 2 == 0 {
            client.clone()
        } else {
            remote.clone()
        };
        let (offer, entry) = if task % 2 == 0 {
            (a, entry_a.clone())
        } else {
            (b, entry_b.clone())
        };
        let expected = pattern.clone();
        tasks.push(tokio::spawn(async move {
            for i in 0..16 {
                let bytes = peer
                    .read(
                        offer,
                        entry.id,
                        ((task * 16 + i) % 256 * CHUNK) as u64,
                        CHUNK as u32,
                    )
                    .await
                    .unwrap();
                assert_eq!(bytes, expected);
            }
        }));
    }
    remote.send(Control::Input {
        lease,
        input: Input::Wheel {
            horizontal: true,
            delta: -1,
        },
    });
    assert!(matches!(
        control(&mut client_events).await,
        Control::Input {
            input: Input::Wheel {
                horizontal: true,
                delta: -1
            },
            ..
        }
    ));
    for task in tasks {
        task.await.unwrap();
    }
    assert!(client.connected() && remote.connected());
    assert!(client.read(a, Uuid::new_v4(), 0, 1).await.is_err());
    assert!(file.exists());
    // Keep old Peer objects alive (as an outstanding COM stream might). The new
    // session must still obtain the same UDP port after disconnect.
    client.close("test");
    timeout(Duration::from_secs(5), remote.closed())
        .await
        .unwrap();
    for hybrid in [false, true, true] {
        let new = connect(
            &trust,
            hybrid,
            client_catalog.clone(),
            client_sink.clone(),
            stop.child_token(),
        )
        .await
        .unwrap()
        .start();
        let other = timeout(Duration::from_secs(5), peers.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(new.hybrid(), hybrid);
        assert_eq!(other.hybrid(), hybrid);
        if hybrid {
            for _ in 0..40 {
                if new.transport() == "Hybride actif" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            assert_eq!(new.transport(), "Hybride actif");
        }
        new.close("test");
        timeout(Duration::from_secs(5), other.closed())
            .await
            .unwrap();
    }
    stop.cancel();
    server.await.unwrap().unwrap();
    fs::remove_dir_all(temp).unwrap();
}
#[tokio::test]
async fn invalid_frame_lengths_are_rejected_before_allocation() {
    use tokio::io::AsyncWriteExt;
    for count in [0u32, 256 * 1024 + 1, u32::MAX] {
        let (mut tx, mut rx) = tokio::io::duplex(16);
        tx.write_u32(count).await.unwrap();
        assert!(
            read_frame::<_, Control>(&mut rx, CONTROL_FRAME)
                .await
                .is_err()
        );
    }
}
