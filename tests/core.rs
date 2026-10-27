use multimachine_rust::{
    engine::{Barrier, Effect, Engine, Phase},
    files::*,
    hybrid::*,
    layout::{self, Layout},
    model::*,
    virtuals,
};
use std::{collections::VecDeque, fs};
use uuid::Uuid;
fn desktop(x: i32, y: i32, w: i32, h: i32) -> Desktop {
    Desktop {
        name: "PC".into(),
        monitors: vec![Monitor {
            name: "screen".into(),
            x,
            y,
            width: w,
            height: h,
        }],
    }
}
fn offer() -> Offer {
    let mut o = Offer::empty(1, Uuid::new_v4());
    o.files.push(FileEntry {
        id: Uuid::new_v4(),
        name: "a.txt".into(),
        length: 42,
        directory: false,
        modified: 0,
    });
    o
}
fn mouse(x: i32, y: i32) -> Input {
    Input::Move {
        position: Point::new(x, y),
    }
}
fn left(up: bool) -> Input {
    Input::Button { button: 1, up }
}
fn movement(lease: Uuid, x: i32, y: i32) -> Control {
    Control::Input {
        lease,
        input: mouse(x, y),
    }
}

#[test]
fn layouts_negative_coordinates_and_inverse() {
    for right in [false, true] {
        for offset in [-99, 0, 99] {
            let l = Layout::new(
                desktop(-200, -50, 100, 200),
                desktop(300, 70, 150, 150),
                Placement { right, offset },
            )
            .unwrap();
            let inv = l.reverse();
            for p in [Point::new(300, 70), Point::new(449, 219)] {
                assert_eq!(l.native(true, l.world(true, p)), p)
            }
            assert_eq!(inv.reverse().local, l.local);
            assert!(l.shared());
            let portal = l.portals()[0].clone();
            let seam = Point::new(
                if right { -101 } else { -200 },
                portal.y + portal.height / 2,
            );
            let target = l.exit(seam, l.direction()).unwrap();
            assert!(l.remote.monitors.iter().any(|m| m.contains(target)));
            assert!(inv.exit(target, -l.direction()).is_some());
        }
    }
}
#[test]
fn geometry_does_not_jump_holes_or_internal_edges() {
    let mut a = desktop(0, 0, 100, 100);
    a.monitors.push(Monitor {
        name: "hdmi".into(),
        x: 100,
        y: 0,
        width: 100,
        height: 100,
    });
    let b = desktop(0, 0, 100, 100);
    let l = Layout::new(a, b, Placement::default()).unwrap();
    assert!(l.exit(Point::new(99, 50), 1).is_none());
    assert_eq!(l.exit(Point::new(199, 50), 1), Some(Point::new(0, 50)));
    let mut remote = desktop(0, 0, 100, 100);
    remote.monitors.push(Monitor {
        name: "lower".into(),
        x: 200,
        y: 200,
        width: 100,
        height: 100,
    });
    let l = Layout::new(desktop(0, 0, 100, 100), remote, Placement::default()).unwrap();
    assert_eq!(
        l.move_remote(Point::new(99, 50), 250, 0),
        (true, Point::new(99, 50))
    );
    assert!(!l.move_remote(Point::new(2, 50), -10, 0).0);
}
#[test]
fn topology_adapts_after_hdmi_switch() {
    let a = desktop(0, 0, 100, 100);
    let b = desktop(0, 0, 100, 100);
    let p = layout::adapt(
        &a,
        &b,
        Placement {
            right: true,
            offset: 2000,
        },
    );
    assert!(Layout::new(a.clone(), b.clone(), p).unwrap().shared());
    let old = layout::key(&a, &b);
    let mut changed = a.clone();
    changed.monitors.push(Monitor {
        name: "extra".into(),
        x: 100,
        y: 0,
        width: 100,
        height: 100,
    });
    assert_ne!(old, layout::key(&changed, &b));
}
#[test]
fn hybrid_action_anchor_and_future_motion() {
    let lease = Uuid::new_v4();
    let mut tx = Sender::default();
    let mut rx = Receiver::default();
    let first = tx.prepare(movement(lease, 20, 30), 1).unwrap();
    let action = tx
        .prepare(
            Control::Input {
                lease,
                input: left(false),
            },
            2,
        )
        .unwrap();
    let future = tx.prepare(movement(lease, 40, 50), 3).unwrap();
    assert!(rx.accept(future).unwrap().is_empty());
    assert!(rx.pending());
    let events = rx.accept(action).unwrap();
    assert_eq!(events.len(), 3);
    assert!(matches!(
        events[0],
        Control::Input {
            input: Input::Move {
                position: Point { x: 20, y: 30 }
            },
            ..
        }
    ));
    assert!(matches!(
        events[1],
        Control::Input {
            input: Input::Button { up: false, .. },
            ..
        }
    ));
    assert!(matches!(
        events[2],
        Control::Input {
            input: Input::Move {
                position: Point { x: 40, y: 50 }
            },
            ..
        }
    ));
    assert!(rx.accept(first).unwrap().is_empty());
}
#[test]
fn hybrid_idle_final_is_once_and_old_motion_is_ignored() {
    let lease = Uuid::new_v4();
    let mut tx = Sender::default();
    let mut rx = Receiver::default();
    let lost = tx.prepare(movement(lease, 123, 456), 100).unwrap();
    assert!(tx.checkpoint(159).is_none());
    let final_position = tx.checkpoint(160).unwrap();
    assert_eq!(rx.accept(final_position).unwrap().len(), 1);
    assert!(tx.checkpoint(10000).is_none());
    assert!(rx.accept(lost).unwrap().is_empty());
}
#[test]
fn hybrid_loss_reorder_and_clicks_are_exact() {
    let lease = Uuid::new_v4();
    let mut tx = Sender::default();
    let mut rx = Receiver::default();
    let mut reliable = VecDeque::new();
    let mut datagrams = VecDeque::new();
    let mut expected = VecDeque::new();
    let mut delivered = vec![];
    for n in 1..5000 {
        let event = if n % 23 == 0 {
            expected.push_back(Point::new(n - 1, n - 1));
            Control::Input {
                lease,
                input: Input::Wheel {
                    horizontal: false,
                    delta: 1,
                },
            }
        } else {
            movement(lease, n, n)
        };
        let f = tx.prepare(event, n as u64).unwrap();
        if matches!(f, Frame::Motion { .. }) {
            if n % 7 != 0 {
                datagrams.push_back(f)
            }
        } else {
            reliable.push_back(f)
        }
        if n % 3 == 0 {
            if let Some(f) = datagrams.pop_back() {
                delivered.extend(rx.accept(f).unwrap());
            }
        }
        if n % 31 == 0 {
            while let Some(f) = reliable.pop_front() {
                delivered.extend(rx.accept(f).unwrap());
            }
        }
    }
    while let Some(f) = reliable.pop_front() {
        delivered.extend(rx.accept(f).unwrap());
    }
    while let Some(f) = datagrams.pop_front() {
        delivered.extend(rx.accept(f).unwrap());
    }
    if let Some(f) = tx.checkpoint(6000) {
        delivered.extend(rx.accept(f).unwrap());
    }
    let mut position = Point::default();
    for event in delivered {
        match event {
            Control::Input {
                input: Input::Move { position: p },
                ..
            } => position = p,
            Control::Input {
                input: Input::Wheel { .. },
                ..
            } => assert_eq!(position, expected.pop_front().unwrap()),
            _ => (),
        }
    }
    assert!(expected.is_empty());
    assert_eq!(position, Point::new(4999, 4999));
}
#[test]
fn encrypted_motion_rejects_every_tampered_byte_and_replays() {
    let session = Uuid::new_v4();
    let key = [7; 32];
    let motion = Motion {
        lease: Uuid::new_v4(),
        sequence: 3,
        after: 2,
        position: Point::new(-18, 4000),
    };
    let mut sender = Datagram::new(session, &key);
    let packet = sender.encode(3, &motion).unwrap();
    assert_eq!(packet.len(), DATAGRAM_SIZE);
    for i in 0..packet.len() {
        let mut broken = packet.clone();
        broken[i] ^= 0x40;
        let mut receiver = Datagram::new(session, &key);
        assert!(receiver.decode(&broken).is_none(), "byte {i}");
        assert_eq!(receiver.decode(&packet), Some((3, motion.clone())));
        assert!(receiver.decode(&packet).is_none());
    }
    assert!(Datagram::new(session, &[8; 32]).decode(&packet).is_none());
    assert!(
        Datagram::new(Uuid::new_v4(), &key)
            .decode(&packet)
            .is_none()
    );
}
#[test]
fn encrypted_reordering_window_is_bounded() {
    let id = Uuid::new_v4();
    let mut tx = Datagram::new(id, &[1; 32]);
    let mut rx = Datagram::new(id, &[1; 32]);
    let m = Motion {
        lease: id,
        sequence: 1,
        after: 0,
        position: Point::default(),
    };
    let packets = (0..90)
        .map(|_| tx.encode(3, &m).unwrap())
        .collect::<Vec<_>>();
    assert!(rx.decode(&packets[89]).is_some());
    assert!(rx.decode(&packets[26]).is_some());
    assert!(rx.decode(&packets[25]).is_none());
    assert!(rx.decode(&packets[88]).is_some());
    assert!(rx.decode(&packets[89]).is_none());
}
#[test]
fn unsafe_files_and_malformed_offers_are_rejected() {
    for name in [
        "..\\a", "a\\..\\b", "a\\", "\\a", "C:\\a", "CON.txt", "nul", "LPT1.doc", "a.", "a ",
        "a/b/../c", "a\0b", "a*b",
    ] {
        assert!(!safe_name(name), "{name}")
    }
    for name in [
        "résumé.png",
        "folder\\子供.txt",
        "COM10.log",
        "a b",
        "a/b.txt",
    ] {
        assert!(safe_name(name), "{name}")
    }
    let mut o = offer();
    o.files.push(o.files[0].clone());
    assert!(o.validate().is_err());
    assert!(
        Control::Placement {
            placement: Placement {
                right: true,
                offset: i32::MIN
            }
        }
        .validate()
        .is_err()
    );
}
#[test]
fn virtual_descriptors_roundtrip_and_boundaries() {
    let mut o = offer();
    o.files[0].name = "dossier\\émoji 🐈.txt".into();
    o.files[0].length = 5_000_000_000;
    o.files[0].modified = 123456789;
    let bytes = virtuals::descriptors(&o.files);
    let parsed = virtuals::parse(&bytes).unwrap();
    assert_eq!(parsed[0].name, o.files[0].name);
    assert_eq!(parsed[0].length, Some(5_000_000_000));
    assert_eq!(parsed[0].modified, 123456789);
    for n in 0..bytes.len() {
        assert!(virtuals::parse(&bytes[..n]).is_err())
    }
    let mut bad = bytes;
    bad[72 + 4] = b'.';
    bad[74 + 4] = b'.';
    bad[76 + 4] = b'\\';
    assert!(virtuals::parse(&bad).is_err());
}
#[test]
fn clean_html_fallback_and_clipboard_clock() {
    assert_eq!(html_to_text(b"Version:1\r\n<html><!--StartFragment--><p>A &amp; B</p><script>secret()</script><p>suite</p><!--EndFragment--></html>"),"A & B\r\nsuite");
    let a = Uuid::from_u128(1);
    let b = Uuid::from_u128(2);
    let mut clock = Clock::new(a);
    assert_eq!(clock.next_clock().unwrap(), 1);
    let mut o = offer();
    o.clock = 1;
    o.origin = b;
    assert!(clock.accept(&o));
    assert!(!clock.accept(&o));
    assert_eq!(clock.next_clock().unwrap(), 2);
    assert!(!clock.accept(&o));
}
struct Temp(std::path::PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("mmr-test-{}", Uuid::new_v4()));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn catalog_copy_only_reads_and_rejects_modifications() {
    let temp = Temp::new();
    let dir = temp.0.join("folder");
    fs::create_dir(&dir).unwrap();
    let file = dir.join("large.bin");
    fs::write(&file, vec![9; CHUNK + 7]).unwrap();
    let id = Uuid::new_v4();
    let c = Catalog::default();
    let entries = c.register(id, &[dir]).unwrap();
    assert_eq!(entries.len(), 2);
    let f = entries.iter().find(|f| !f.directory).unwrap();
    assert_eq!(c.read(id, f.id, CHUNK as u64, 32).unwrap(), vec![9; 7]);
    assert!(c.read(id, f.id, 0, CHUNK + 1).is_err());
    assert!(c.read(id, Uuid::new_v4(), 0, 1).is_err());
    fs::write(&file, b"changed").unwrap();
    assert!(c.read(id, f.id, 0, 1).is_err());
    c.remove(id);
    assert_eq!(fs::read(file).unwrap(), b"changed");
}
#[cfg(unix)]
#[test]
fn catalog_refuses_symlinks_at_any_depth() {
    let t = Temp::new();
    fs::write(t.0.join("a"), b"secret").unwrap();
    std::os::unix::fs::symlink(t.0.join("a"), t.0.join("link")).unwrap();
    assert!(
        Catalog::default()
            .register(Uuid::new_v4(), &[t.0.join("link")])
            .is_err()
    );
    assert!(
        Catalog::default()
            .register(Uuid::new_v4(), std::slice::from_ref(&t.0))
            .is_err()
    );
}

struct Rig {
    pc: [Engine; 2],
    wire: VecDeque<(usize, Control)>,
    offers: VecDeque<(usize, Offer)>,
    native: [Option<Uuid>; 2],
    drops: usize,
    time: u64,
}
impl Rig {
    fn new(right: bool) -> Self {
        let l = Layout::new(
            desktop(0, 0, 100, 100),
            desktop(0, 0, 100, 100),
            Placement { right, offset: 0 },
        )
        .unwrap();
        let mut a = Engine::new(true, Point::new(50, 50));
        let mut b = Engine::new(false, Point::new(50, 50));
        a.connected = true;
        b.connected = true;
        a.set_layout(Some(l.clone()), 0);
        b.set_layout(Some(l.reverse()), 0);
        a.effects();
        b.effects();
        Self {
            pc: [a, b],
            wire: VecDeque::new(),
            offers: VecDeque::new(),
            native: [None, None],
            drops: 0,
            time: 1000,
        }
    }
    fn pump(&mut self) {
        for _ in 0..1000 {
            let mut progress = false;
            for side in 0..2 {
                for e in self.pc[side].effects() {
                    progress = true;
                    match e {
                        Effect::Send(c) => self.wire.push_back((1 - side, c)),
                        Effect::Offer(o) => self.offers.push_back((1 - side, o)),
                        Effect::Barrier { epoch, kind } => self.pc[side].barrier_done(epoch, kind),
                        Effect::Stage(id) => self.pc[side].staged(id),
                        Effect::Start(o) => {
                            assert!(self.native[side].is_none());
                            self.native[side] = Some(o.id);
                            self.pc[side].native_ready(o.id);
                        }
                        Effect::CancelNative => {
                            if let Some(id) = self.native[side].take() {
                                self.pc[side].native_finished(id, false)
                            }
                        }
                        _ => (),
                    }
                }
            }
            if let Some((side, c)) = self.wire.pop_front() {
                progress = true;
                self.pc[side].control(c, self.time)
            }
            if let Some((side, o)) = self.offers.pop_front() {
                progress = true;
                self.pc[side].receive_offer(o, self.time)
            }
            if !progress {
                return;
            }
        }
        panic!("state machine did not quiesce")
    }
    fn physical(&mut self, side: usize, input: Input) {
        self.time += 10;
        self.pc[side].physical(input, self.time);
        self.pump();
    }
    fn source(&mut self, physical: usize, source: usize) {
        if physical != source {
            let l = self.pc[physical].layout.as_ref().unwrap().clone();
            let p = Point::new(if l.placement.right { 99 } else { 0 }, 50);
            let target = l.exit(p, l.direction()).unwrap();
            assert!(self.pc[physical].begin(p, target, None, self.time));
            self.pump();
            assert!(self.pc[physical].granted);
        }
        self.physical(physical, left(false));
        assert!(self.pc[source].left());
        let l = self.pc[source].layout.as_ref().unwrap();
        let p = Point::new(if l.placement.right { 99 } else { 0 }, 50);
        self.pc[source].captured(offer(), p, self.time);
        self.pump();
        assert_eq!(self.pc[1 - source].phase, Phase::Running);
        assert!(!self.pc[1 - source].hold);
    }
    fn drop(&mut self, physical: usize, target: usize) {
        self.physical(physical, left(true));
        let id = self.native[target].take().unwrap();
        self.pc[target].native_drop(id);
        self.pc[target].native_finished(id, true);
        self.pump();
        self.drops += 1;
        assert_eq!(self.pc[target].phase, Phase::None);
        assert!(!self.pc[target].hold);
        assert!(!self.pc[target].left());
    }
    fn home(&mut self) {
        for s in 0..2 {
            self.pc[s].emergency(self.time)
        }
        self.pump();
        for s in 0..2 {
            assert!(self.pc[s].incoming.is_nil());
            assert!(self.pc[s].outgoing.is_nil());
            assert!(!self.pc[s].hold);
        }
        self.time += 500;
    }
}
#[test]
fn repeated_drag_all_four_directions_and_both_screen_sides() {
    for right in [false, true] {
        let mut rig = Rig::new(right);
        for _ in 0..50 {
            for physical in 0..2 {
                for source in 0..2 {
                    rig.source(physical, source);
                    rig.drop(physical, 1 - source);
                    rig.home();
                }
            }
        }
        assert_eq!(rig.drops, 200);
    }
}
#[test]
fn escape_disconnect_return_and_kvm_leave_next_drag_usable() {
    for physical in 0..2 {
        for source in 0..2 {
            for mode in 0..4 {
                let mut rig = Rig::new(true);
                rig.source(physical, source);
                let target = 1 - source;
                let saved_layout = rig.pc[target].layout.clone();
                match mode {
                    0 => rig.pc[target].cancel(),
                    1 => rig.pc[target].disconnect(rig.time),
                    2 => {
                        let l = rig.pc[target].layout.as_ref().unwrap();
                        let p = Point::new(if l.placement.right { 99 } else { 0 }, 50);
                        rig.pc[target].returning(p, rig.time)
                    }
                    _ => rig.pc[target].hardware(Point::new(50, 50), rig.time),
                }
                rig.pump();
                rig.physical(physical, left(true));
                rig.home();
                rig.pc[target].connected = true;
                rig.pc[target].set_layout(saved_layout, rig.time);
                rig.pump();
                rig.source(physical, source);
                rig.drop(physical, target);
            }
        }
    }
}
#[test]
fn cancelled_late_offer_never_reopens_a_gesture() {
    let mut rig = Rig::new(true);
    let o = offer();
    rig.pc[0].control(Control::DragCancel { offer: o.id }, 1000);
    rig.pc[0].receive_offer(o, 1001);
    rig.pump();
    assert_eq!(rig.pc[0].phase, Phase::None);
}
#[test]
fn native_finish_barrier_survives_reset_without_releasing_next_gesture() {
    let mut rig = Rig::new(true);
    rig.source(0, 1);
    let id = rig.native[0].take().unwrap();
    rig.pc[0].native_finished(id, false);
    let effects = rig.pc[0].effects();
    let epoch = effects
        .iter()
        .find_map(|e| {
            if let Effect::Barrier {
                epoch,
                kind: Barrier::Finish(_),
            } = e
            {
                Some(*epoch)
            } else {
                None
            }
        })
        .unwrap();
    rig.pc[0].emergency(1200);
    rig.pc[0].barrier_done(epoch, Barrier::Finish(id));
    assert_eq!(rig.pc[0].phase, Phase::Finishing);
    rig.pc[0].poll(1300);
    rig.pump();
    assert_eq!(rig.pc[0].phase, Phase::None);
    assert!(!rig.pc[0].hold);
}
#[test]
fn simultaneous_claims_have_one_winner() {
    let mut rig = Rig::new(true);
    assert!(rig.pc[0].begin(Point::new(99, 50), Point::new(0, 50), None, 1000));
    assert!(rig.pc[1].begin(Point::new(0, 50), Point::new(99, 50), None, 1000));
    rig.pump();
    assert!(rig.pc[0].granted);
    assert!(rig.pc[1].receiving());
    assert!(!rig.pc[1].granted);
}

#[test]
fn desktop_interruption_ends_both_leases_and_allows_fresh_control_in_either_role() {
    for right in [false, true] {
        for physical in 0..2 {
            for interrupted in 0..2 {
                let mut rig = Rig::new(right);
                let begin = |rig: &mut Rig| {
                    let l = rig.pc[physical].layout.as_ref().unwrap();
                    let exit = Point::new(if l.placement.right { 99 } else { 0 }, 50);
                    let target = l.exit(exit, l.direction()).unwrap();
                    assert!(rig.pc[physical].begin(exit, target, None, rig.time));
                    rig.pump();
                    assert!(rig.pc[physical].granted);
                };
                begin(&mut rig);
                let old = rig.pc[physical].outgoing;
                rig.physical(physical, left(false));
                rig.pc[interrupted].hardware(Point::new(50, 50), rig.time);
                rig.pump();
                assert!(
                    rig.pc
                        .iter()
                        .all(|p| p.incoming.is_nil() && p.outgoing.is_nil())
                );
                rig.pc[interrupted].hardware(Point::new(50, 50), rig.time + 100);
                rig.pump();
                rig.physical(physical, left(true));
                begin(&mut rig);
                assert_ne!(old, rig.pc[physical].outgoing);
                rig.pc[1 - physical].control(
                    Control::Input {
                        lease: old,
                        input: left(false),
                    },
                    rig.time,
                );
                assert!(
                    !rig.pc[1 - physical]
                        .effects()
                        .iter()
                        .any(|e| matches!(e, Effect::Inject(_)))
                );
                rig.physical(physical, left(false));
                assert!(rig.pc[1 - physical].left());
                rig.physical(physical, left(true));
                assert!(!rig.pc[1 - physical].left());
            }
        }
    }
}
