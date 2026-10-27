//! Pure input/drag state machine. Native operations are ordered effects; callbacks carry
//! a gesture id and injection epoch so a late completion cannot affect the next drag.
use crate::{layout::Layout, model::*};
use std::collections::{HashSet, VecDeque};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Barrier {
    Begin(Uuid),
    Start(Uuid),
    Finish(Uuid),
}
#[derive(Clone, Debug)]
pub enum Effect {
    Send(Control),
    Offer(Offer),
    Inject(Input),
    Reset(u64),
    SourceLeftUp,
    Barrier {
        epoch: u64,
        kind: Barrier,
    },
    Portals(bool),
    Layout(Option<Layout>),
    Indicator {
        monitor: Option<Monitor>,
        right: bool,
        receiving: bool,
    },
    Stage(Uuid),
    Start(Offer),
    CancelNative,
    Remove(Uuid),
    Status(&'static str),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    None,
    Preparing,
    Starting,
    Running,
    Committed,
    Cancelling,
    Finishing,
}
pub struct Engine {
    pub layout: Option<Layout>,
    pub connected: bool,
    pub server: bool,
    pub incoming: Uuid,
    pub outgoing: Uuid,
    pub granted: bool,
    pub local_drag: Uuid,
    pub phase: Phase,
    pub remote_drag: Uuid,
    pub hold: bool,
    pub epoch: u64,
    physical_keys: HashSet<u16>,
    physical_buttons: HashSet<u8>,
    injected_left: bool,
    last: Point,
    anchor: Point,
    remote_position: Point,
    last_return: u64,
    requested: u64,
    arrival: bool,
    discard_up: bool,
    drag_starting: bool,
    drag_remote: bool,
    grant_offer: Option<Uuid>,
    source: Option<Offer>,
    source_lease: Uuid,
    prepared: Option<Offer>,
    active: Option<Offer>,
    target: Point,
    exit: Point,
    pending: u64,
    committed: bool,
    cleanup_epoch: u64,
    closed: VecDeque<Uuid>,
    effects: Vec<Effect>,
}
impl Engine {
    pub fn new(server: bool, position: Point) -> Self {
        Self {
            layout: None,
            connected: false,
            server,
            incoming: Uuid::nil(),
            outgoing: Uuid::nil(),
            granted: false,
            local_drag: Uuid::nil(),
            phase: Phase::None,
            remote_drag: Uuid::nil(),
            hold: false,
            epoch: 0,
            physical_keys: HashSet::new(),
            physical_buttons: HashSet::new(),
            injected_left: false,
            last: position,
            anchor: position,
            remote_position: Point::default(),
            last_return: 0,
            requested: 0,
            arrival: false,
            discard_up: false,
            drag_starting: false,
            drag_remote: false,
            grant_offer: None,
            source: None,
            source_lease: Uuid::nil(),
            prepared: None,
            active: None,
            target: Point::default(),
            exit: Point::default(),
            pending: 0,
            committed: false,
            cleanup_epoch: 0,
            closed: VecDeque::new(),
            effects: vec![],
        }
    }
    pub fn effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }
    pub fn physical_left(&self) -> bool {
        self.physical_buttons.contains(&1)
    }
    pub fn left(&self) -> bool {
        self.physical_left() || self.injected_left
    }
    pub fn receiving(&self) -> bool {
        !self.incoming.is_nil()
    }
    pub fn seed(
        &mut self,
        keys: impl IntoIterator<Item = u16>,
        buttons: impl IntoIterator<Item = u8>,
    ) {
        self.physical_keys.extend(keys);
        self.physical_buttons.extend(buttons);
    }
    fn emit(&mut self, e: Effect) {
        self.effects.push(e)
    }
    fn send(&mut self, c: Control) {
        if self.connected {
            self.emit(Effect::Send(c))
        }
    }
    fn indicator(&mut self, monitor: Option<Monitor>, receiving: bool) {
        let right = self.layout.as_ref().is_some_and(|l| l.placement.right);
        self.emit(Effect::Indicator {
            monitor,
            right,
            receiving,
        });
    }
    pub fn set_layout(&mut self, layout: Option<Layout>, now: u64) {
        self.emergency(now);
        self.layout = layout.clone();
        self.emit(Effect::Layout(layout));
    }
    pub fn disconnect(&mut self, now: u64) {
        self.emergency(now);
        self.connected = false;
        self.layout = None;
        self.emit(Effect::Layout(None));
        self.physical_buttons.clear();
        self.physical_keys.clear();
        self.emit(Effect::Portals(false));
    }
    pub fn hardware(&mut self, position: Point, now: u64) {
        self.emergency(now);
        self.physical_buttons.clear();
        self.physical_keys.clear();
        self.discard_up = false;
        self.last = position;
    }
    pub fn physical(&mut self, input: Input, now: u64) -> bool {
        if self.receiving() {
            self.emergency(now)
        }
        match input.clone() {
            Input::Move { position: p } => {
                if self.hold {
                    return true;
                }
                if self.granted {
                    if self.drag_starting {
                        return true;
                    }
                    if let Some(layout) = self.layout.clone() {
                        let (remote, position) = layout.move_remote(
                            self.remote_position,
                            p.x - self.anchor.x,
                            p.y - self.anchor.y,
                        );
                        self.move_local(self.anchor);
                        if remote {
                            self.remote_position = position;
                            self.relay(Input::Move { position });
                        } else if self.physical_left() && !self.drag_remote {
                            // Let OLE on the receiving PC enter its own portal before returning home.
                            self.remote_position = layout
                                .remote
                                .clamp(layout.native(true, layout.world(false, position)));
                            self.relay(Input::Move {
                                position: self.remote_position,
                            });
                        } else {
                            if self.drag_remote {
                                self.cancel()
                            }
                            self.home(Some(position), false, now);
                        }
                    }
                    return true;
                }
                if !self.outgoing.is_nil() {
                    return true;
                }
                if self.connected
                    && now.saturating_sub(self.last_return) > 350
                    && self.physical_keys.is_empty()
                    && self.physical_buttons.is_empty()
                {
                    if let Some(target) = self
                        .layout
                        .as_ref()
                        .and_then(|l| l.exit(p, p.x - self.last.x))
                    {
                        self.begin(p, target, None, now);
                        return true;
                    }
                }
                self.last = p;
            }
            Input::Button { button, up } => {
                if up {
                    self.physical_buttons.remove(&button);
                } else {
                    self.physical_buttons.insert(button);
                }
                if button == 1 {
                    let preparing = self.hold;
                    if up && !self.local_drag.is_nil() && self.injected_left {
                        self.inject(Input::Button {
                            button: 1,
                            up: true,
                        });
                    }
                    self.buttons(!up);
                    if preparing {
                        self.discard_up = !up;
                        return true;
                    }
                    if up && self.discard_up {
                        self.discard_up = false;
                        return true;
                    }
                }
                if self.granted {
                    self.relay(input);
                    return true;
                }
                if !self.outgoing.is_nil() {
                    self.emergency(now);
                    return true;
                }
            }
            Input::Key { key, up, .. } => {
                if up {
                    self.physical_keys.remove(&key);
                } else {
                    self.physical_keys.insert(key);
                }
                let ctrl = [0x11, 0xa2, 0xa3]
                    .iter()
                    .any(|k| self.physical_keys.contains(k));
                let alt = [0x12, 0xa4, 0xa5]
                    .iter()
                    .any(|k| self.physical_keys.contains(k));
                if !up && key == 0x7b && ctrl && alt {
                    self.emergency(now);
                    return true;
                }
                if !up && key == 0x1b {
                    self.cancel()
                }
                if self.granted {
                    self.relay(input);
                    return true;
                }
                if !self.outgoing.is_nil() {
                    self.emergency(now)
                }
            }
            Input::Wheel { .. } => {
                if self.granted {
                    self.relay(input);
                    return true;
                }
                if !self.outgoing.is_nil() {
                    self.emergency(now);
                    return true;
                }
            }
        }
        false
    }
    fn buttons(&mut self, down: bool) {
        self.emit(Effect::Portals(down && !self.granted));
        if !down
            && (self.source.is_some()
                || matches!(self.phase, Phase::Preparing | Phase::Starting)
                || self.drag_starting)
        {
            self.cancel()
        }
    }
    fn relay(&mut self, input: Input) {
        self.send(Control::Input {
            lease: self.outgoing,
            input,
        })
    }
    pub fn inject(&mut self, input: Input) {
        if let Input::Move { position } = input {
            if !self
                .layout
                .as_ref()
                .is_some_and(|l| l.local.monitors.iter().any(|m| m.contains(position)))
            {
                return;
            }
        }
        self.emit(Effect::Inject(input.clone()));
        if let Input::Button { button: 1, up } = input {
            self.injected_left = !up;
            self.buttons(!up);
        }
    }
    fn move_local(&mut self, p: Point) {
        if let Some(l) = &self.layout {
            let position = l.local.clamp(p);
            self.last = position;
            self.emit(Effect::Inject(Input::Move { position }));
        }
    }
    fn reset(&mut self) {
        self.epoch += 1;
        self.injected_left = false;
        self.emit(Effect::Reset(self.epoch));
        self.emit(Effect::Portals(false));
    }
    fn barrier(&mut self, kind: Barrier) {
        self.emit(Effect::Barrier {
            epoch: self.epoch,
            kind,
        })
    }
    pub fn begin(&mut self, exit: Point, target: Point, offer: Option<Uuid>, now: u64) -> bool {
        if !self.connected || !self.outgoing.is_nil() || self.receiving() {
            return false;
        }
        let Some(l) = &self.layout else { return false };
        let p = l.local.clamp(exit);
        let Some(m) = l.local.monitors.iter().find(|m| m.contains(p)).cloned() else {
            return false;
        };
        self.outgoing = Uuid::new_v4();
        self.remote_position = target;
        self.grant_offer = offer;
        self.requested = now;
        self.anchor = Point::new(m.x + m.width / 2, m.y + m.height / 2);
        self.move_local(self.anchor);
        self.indicator(Some(m), false);
        self.barrier(Barrier::Begin(self.outgoing));
        true
    }
    pub fn control(&mut self, c: Control, now: u64) {
        match c {
            Control::Begin { lease, position } => {
                if lease.is_nil()
                    || self.layout.is_none()
                    || self.receiving()
                    || (!self.outgoing.is_nil() && self.server)
                {
                    self.send(Control::End { lease });
                    return;
                }
                if !self.outgoing.is_nil() {
                    self.home(None, false, now);
                    self.cancel();
                }
                self.incoming = lease;
                self.arrival = false;
                self.grant_offer = None;
                self.reset();
                self.send(Control::Grant { lease });
                self.incoming_input(Input::Move { position });
            }
            Control::Grant { lease } => {
                if lease == self.outgoing && !lease.is_nil() && !self.granted {
                    self.granted = true;
                    self.relay(Input::Move {
                        position: self.remote_position,
                    });
                    if let Some(id) = self.grant_offer.take() {
                        if self.source.as_ref().is_none_or(|s| s.id != id) || !self.physical_left()
                        {
                            self.cancel();
                            self.home(None, false, now)
                        } else {
                            self.remote_drag = id;
                            self.source = None;
                            self.drag_remote = true;
                            self.drag_starting = true;
                            self.send(Control::DragStart {
                                offer: id,
                                lease,
                                position: self.target,
                            });
                        }
                    }
                }
            }
            Control::End { lease } => {
                if lease == self.outgoing && !lease.is_nil() {
                    self.cancel();
                    self.home(None, false, now)
                } else if lease == self.incoming && !lease.is_nil() {
                    self.cancel();
                    self.incoming = Uuid::nil();
                    self.reset();
                    self.arrival = false;
                    self.indicator(None, false);
                }
            }
            Control::Input { lease, input } => {
                if !lease.is_nil() && lease == self.incoming {
                    self.incoming_input(input)
                }
            }
            Control::DragPause { offer, lease, .. } => {
                if !self.closed.contains(&offer) && lease == self.outgoing && !lease.is_nil() {
                    self.remote_drag = offer;
                    self.drag_starting = true;
                    self.pending = now
                }
            }
            Control::DragReady { offer: id } => {
                let Some(source) = self.source.clone().filter(|s| s.id == id) else {
                    return;
                };
                if !self.left() {
                    self.cancel();
                    return;
                }
                if !self.source_lease.is_nil() {
                    if self.incoming != self.source_lease {
                        self.cancel();
                        return;
                    }
                    self.send(Control::DragHome {
                        offer: id,
                        lease: self.source_lease,
                        position: self.target,
                    });
                    self.remote_drag = id;
                    self.source = None;
                    self.incoming = Uuid::nil();
                    self.reset();
                    self.arrival = false;
                    self.indicator(None, false);
                    self.last_return = now;
                } else {
                    self.injected_left = false;
                    self.emit(Effect::SourceLeftUp);
                    if !self.begin(self.exit, self.target, Some(source.id), now) {
                        self.cancel()
                    }
                }
            }
            Control::DragStart {
                offer,
                lease,
                position,
            } => {
                if self.prepared.as_ref().is_some_and(|s| s.id == offer)
                    && lease == self.incoming
                    && !lease.is_nil()
                {
                    self.start_prepared(position, now)
                }
            }
            Control::DragHome {
                offer,
                lease,
                position,
            } => {
                if self.prepared.as_ref().is_none_or(|s| s.id != offer) {
                    return;
                }
                if lease != self.outgoing || lease.is_nil() || !self.physical_left() {
                    self.cancel_id(offer, true);
                    return;
                }
                self.remote_drag = Uuid::nil();
                self.home(Some(position), true, now);
                self.start_prepared(position, now)
            }
            Control::DragRunning { offer } => {
                if self.remote_drag == offer {
                    self.drag_starting = false
                }
            }
            Control::DragDropped { offer } => {
                if self.remote_drag == offer {
                    self.remote_drag = Uuid::nil();
                    self.drag_remote = false;
                    self.drag_starting = false;
                }
            }
            Control::DragCancel { offer } => self.cancel_id(offer, false),
            Control::DragFinished { offer, dropped } => {
                if self.remote_drag == offer {
                    self.remote_drag = Uuid::nil();
                    self.drag_remote = false;
                    self.drag_starting = false
                }
                self.remember(offer);
                if !dropped {
                    self.emit(Effect::Remove(offer));
                }
            }
            Control::Desktop { .. } | Control::Placement { .. } | Control::Ping { .. } => {}
        }
    }
    fn incoming_input(&mut self, input: Input) {
        if !self.arrival {
            if let Input::Move { position } = input {
                if let Some(m) = self
                    .layout
                    .as_ref()
                    .and_then(|l| l.local.monitors.iter().find(|m| m.contains(position)))
                    .cloned()
                {
                    self.arrival = true;
                    self.indicator(Some(m), true);
                }
            }
        }
        if matches!(
            input,
            Input::Key {
                key: 0x1b,
                up: false,
                ..
            }
        ) {
            self.cancel()
        }
        // A home-return transition freezes both physical and relayed motion while staging.
        if self.hold && matches!(input, Input::Move { .. }) {
            return;
        }
        self.inject(input);
    }
    pub fn home(&mut self, destination: Option<Point>, keep_left: bool, now: u64) {
        if !self.outgoing.is_nil() {
            let lease = self.outgoing;
            self.outgoing = Uuid::nil();
            self.granted = false;
            self.grant_offer = None;
            self.send(Control::End { lease });
            if !keep_left && self.physical_left() {
                self.discard_up = true
            }
            self.move_local(destination.unwrap_or(self.anchor));
            self.indicator(None, false);
        }
        self.drag_starting = false;
        self.drag_remote = false;
        self.last_return = now;
    }
    pub fn emergency(&mut self, now: u64) {
        if !self.local_drag.is_nil() && self.physical_left() {
            self.discard_up = true
        }
        self.cancel();
        self.reset();
        self.home(None, false, now);
        if self.receiving() {
            let lease = self.incoming;
            self.incoming = Uuid::nil();
            self.send(Control::End { lease });
        }
        self.arrival = false;
        self.indicator(None, false);
        self.emit(Effect::Portals(false));
    }
    pub fn captured(&mut self, offer: Offer, point: Point, now: u64) {
        if !self.connected
            || !self.left()
            || self.source.is_some()
            || !self.local_drag.is_nil()
            || !self.remote_drag.is_nil()
        {
            self.emit(Effect::Remove(offer.id));
            return;
        }
        let Some(l) = &self.layout else {
            self.emit(Effect::Remove(offer.id));
            return;
        };
        let Some(target) = l.exit(point, l.direction()) else {
            self.emit(Effect::Remove(offer.id));
            return;
        };
        self.target = l.inset(target);
        self.exit = point;
        self.source_lease = self.incoming;
        self.pending = now;
        if self.receiving() {
            self.send(Control::DragPause {
                offer: offer.id,
                lease: self.incoming,
                position: self.target,
            });
        }
        self.source = Some(offer.clone());
        self.emit(Effect::Offer(offer));
    }
    pub fn receive_offer(&mut self, offer: Offer, now: u64) {
        if self.prepared.as_ref().is_some_and(|s| s.id == offer.id) {
            self.send(Control::DragReady { offer: offer.id });
            return;
        }
        if self.closed.contains(&offer.id)
            || self.prepared.is_some()
            || !self.local_drag.is_nil()
            || self.source.is_some()
            || (!self.remote_drag.is_nil() && self.remote_drag != offer.id)
        {
            self.cancel_id(offer.id, true);
            return;
        }
        let id = offer.id;
        self.prepared = Some(offer);
        self.pending = now;
        self.send(Control::DragReady { offer: id });
    }
    fn start_prepared(&mut self, position: Point, now: u64) {
        let Some(offer) = self.prepared.take() else {
            return;
        };
        self.local_drag = offer.id;
        self.active = Some(offer);
        self.phase = Phase::Preparing;
        self.committed = false;
        self.pending = now;
        self.hold = true;
        self.target = position;
        self.emit(Effect::Stage(self.local_drag));
    }
    pub fn staged(&mut self, id: Uuid) {
        if id != self.local_drag || self.phase != Phase::Preparing || !self.connected {
            return;
        }
        if !self.receiving() && !self.physical_left() {
            self.cancel();
            return;
        }
        let Some(l) = self.layout.as_ref() else {
            self.cancel();
            return;
        };
        let Some(m) = l.local.monitors.iter().find(|m| m.contains(self.target)) else {
            self.cancel();
            return;
        };
        let edge = Point::new(
            if l.placement.right {
                m.right() - 1
            } else {
                m.x
            },
            self.target.y,
        );
        self.move_local(edge);
        self.inject(Input::Button {
            button: 1,
            up: false,
        });
        self.move_local(self.target);
        self.barrier(Barrier::Start(id));
    }
    pub fn barrier_done(&mut self, epoch: u64, kind: Barrier) {
        if epoch != self.epoch {
            return;
        }
        match kind {
            Barrier::Begin(id) => {
                if id == self.outgoing && !id.is_nil() {
                    self.send(Control::Begin {
                        lease: id,
                        position: self.remote_position,
                    });
                }
            }
            Barrier::Start(id) => {
                if id != self.local_drag || self.phase != Phase::Preparing || !self.connected {
                    return;
                }
                if !self.left() || (!self.receiving() && !self.physical_left()) {
                    self.cancel();
                    return;
                }
                self.phase = Phase::Starting;
                if let Some(offer) = self.active.clone() {
                    self.emit(Effect::Start(offer));
                }
            }
            Barrier::Finish(id) => {
                if id != self.local_drag || self.phase != Phase::Finishing {
                    return;
                }
                let dropped = self.committed;
                self.local_drag = Uuid::nil();
                self.phase = Phase::None;
                self.committed = false;
                self.active = None;
                self.hold = false;
                self.remember(id);
                self.send(Control::DragFinished { offer: id, dropped });
            }
        }
    }
    pub fn native_ready(&mut self, id: Uuid) {
        if self.local_drag == id && self.phase == Phase::Starting {
            self.phase = Phase::Running;
            self.hold = false;
            self.send(Control::DragRunning { offer: id });
        }
    }
    pub fn native_drop(&mut self, id: Uuid) {
        if self.local_drag == id && matches!(self.phase, Phase::Starting | Phase::Running) {
            self.committed = true;
            self.phase = Phase::Committed;
            self.send(Control::DragDropped { offer: id });
        }
    }
    pub fn native_finished(&mut self, id: Uuid, dropped: bool) {
        if self.local_drag != id || self.phase == Phase::Finishing {
            return;
        }
        self.phase = Phase::Finishing;
        self.committed |= dropped;
        self.hold = true;
        self.release_drag_button();
        self.cleanup_epoch = self.epoch;
        self.barrier(Barrier::Finish(id));
    }
    fn release_drag_button(&mut self) {
        if self.injected_left {
            self.injected_left = false;
            self.emit(Effect::Inject(Input::Button {
                button: 1,
                up: true,
            }));
        }
        self.emit(Effect::Portals(false));
    }
    pub fn returning(&mut self, point: Point, now: u64) {
        if self.local_drag.is_nil() {
            return;
        }
        self.cancel();
        if !self.receiving() {
            if let Some(target) = self
                .layout
                .as_ref()
                .and_then(|l| l.exit(point, l.direction()))
            {
                self.discard_up = true;
                self.begin(point, target, None, now);
            }
        }
    }
    fn remember(&mut self, id: Uuid) {
        if id.is_nil() || self.closed.contains(&id) {
            return;
        }
        self.closed.push_back(id);
        if self.closed.len() > 128 {
            self.closed.pop_front();
        }
    }
    fn cancel_id(&mut self, id: Uuid, notify: bool) {
        if id.is_nil() || (self.local_drag == id && self.committed) {
            return;
        }
        self.remember(id);
        if notify {
            self.send(Control::DragCancel { offer: id });
        }
        if self.source.as_ref().is_some_and(|s| s.id == id) {
            self.emit(Effect::Remove(id));
            self.source = None
        }
        if self.prepared.as_ref().is_some_and(|s| s.id == id) {
            self.prepared = None
        }
        if self.remote_drag == id {
            self.remote_drag = Uuid::nil();
            self.drag_remote = false;
            self.drag_starting = false;
        }
        if self.local_drag == id && !matches!(self.phase, Phase::Cancelling | Phase::Finishing) {
            let started = self.phase != Phase::Preparing;
            self.phase = Phase::Cancelling;
            self.hold = true;
            self.emit(Effect::CancelNative);
            self.release_drag_button();
            if !started {
                self.native_finished(id, false)
            }
        }
    }
    pub fn cancel(&mut self) {
        self.emit(Effect::CancelNative);
        for id in [
            self.source.as_ref().map_or(Uuid::nil(), |s| s.id),
            self.prepared.as_ref().map_or(Uuid::nil(), |s| s.id),
            self.remote_drag,
            self.local_drag,
        ] {
            self.cancel_id(id, true)
        }
    }
    pub fn cancel_original(&mut self) {
        self.emit(Effect::Inject(Input::Key {
            key: 0x1b,
            scan: 0,
            extended: false,
            up: false,
        }));
        self.emit(Effect::Inject(Input::Key {
            key: 0x1b,
            scan: 0,
            extended: false,
            up: true,
        }));
    }
    pub fn poll(&mut self, now: u64) {
        if !self.outgoing.is_nil() && !self.granted && now.saturating_sub(self.requested) > 3000 {
            self.emergency(now)
        }
        if self.phase == Phase::Finishing && self.cleanup_epoch != self.epoch {
            self.cleanup_epoch = self.epoch;
            self.barrier(Barrier::Finish(self.local_drag));
        }
        if (self.source.is_some()
            || self.prepared.is_some()
            || self.drag_starting
            || matches!(self.phase, Phase::Preparing | Phase::Starting))
            && now.saturating_sub(self.pending) > 15000
        {
            self.emergency(now);
            self.emit(Effect::Status(
                "Passage de fichiers annulé : délai dépassé.",
            ));
        }
    }
}
