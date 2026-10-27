use crate::model::*;
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Motion {
    pub lease: Uuid,
    pub sequence: u64,
    pub after: u64,
    pub position: Point,
}
impl Motion {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.lease.is_nil() && self.sequence > 0,
            "Mouvement invalide"
        );
        Input::Move {
            position: self.position,
        }
        .validate()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "wire", rename_all = "snake_case")]
pub enum Frame {
    Plain {
        event: Control,
    },
    Reliable {
        sequence: u64,
        position: Option<Motion>,
        event: Control,
    },
    Final {
        motion: Motion,
    },
    // Used only in the local transmit queue. Never accepted on the TCP reader.
    Motion {
        motion: Motion,
    },
}
impl Frame {
    pub fn motion_key(&self) -> Option<Uuid> {
        match self {
            Self::Motion { motion } => Some(motion.lease),
            Self::Plain { event } => event.motion().map(|m| m.0),
            _ => None,
        }
    }
}
#[derive(Default)]
pub struct Sender {
    actions: u64,
    positions: u64,
    latest: Option<Motion>,
    dirty: bool,
    changed: u64,
}
impl Sender {
    pub fn prepare(&mut self, event: Control, now: u64) -> Result<Frame> {
        event.validate()?;
        if let Some((lease, position)) = event.motion() {
            self.positions = self
                .positions
                .checked_add(1)
                .ok_or_else(|| anyhow!("Compteur épuisé"))?;
            let motion = Motion {
                lease,
                position,
                sequence: self.positions,
                after: self.actions,
            };
            self.latest = Some(motion.clone());
            self.dirty = true;
            self.changed = now;
            return Ok(Frame::Motion { motion });
        }
        if matches!(event, Control::Ping { .. }) {
            return Ok(Frame::Plain { event });
        }
        self.actions = self
            .actions
            .checked_add(1)
            .ok_or_else(|| anyhow!("Compteur épuisé"))?;
        self.dirty = false;
        let position = self.latest.clone();
        if let Control::End { lease } = event {
            if self.latest.as_ref().is_some_and(|m| m.lease == lease) {
                self.latest = None
            }
        }
        Ok(Frame::Reliable {
            sequence: self.actions,
            position,
            event,
        })
    }
    pub fn checkpoint(&mut self, now: u64) -> Option<Frame> {
        if self.dirty && now.saturating_sub(self.changed) >= 60 {
            self.dirty = false;
            self.latest.clone().map(|motion| Frame::Final { motion })
        } else {
            None
        }
    }
    pub fn checkpoint_deadline(&self) -> Option<u64> {
        self.dirty.then_some(self.changed.saturating_add(60))
    }
}
#[derive(Default)]
pub struct Receiver {
    actions: u64,
    position: u64,
    pending: Option<Motion>,
}
impl Receiver {
    pub fn accept(&mut self, frame: Frame) -> Result<Vec<Control>> {
        let mut events = vec![];
        match frame {
            Frame::Reliable {
                sequence,
                position,
                event,
            } => {
                ensure!(
                    Some(sequence) == self.actions.checked_add(1)
                        && !matches!(event, Control::Ping { .. }),
                    "Ordre fiable invalide"
                );
                event.validate()?;
                if let Some(p) = position {
                    p.validate()?;
                    ensure!(p.after <= self.actions, "Ancre future invalide");
                    self.apply(p, &mut events);
                }
                events.push(event);
                self.actions = sequence;
                self.drain(&mut events);
            }
            Frame::Motion { motion } | Frame::Final { motion } => {
                motion.validate()?;
                if motion.sequence <= self.position || motion.after < self.actions {
                    return Ok(events);
                }
                if self
                    .pending
                    .as_ref()
                    .is_none_or(|m| m.sequence < motion.sequence)
                {
                    self.pending = Some(motion)
                }
                self.drain(&mut events);
            }
            Frame::Plain {
                event: Control::Ping { id, reply },
            } => events.push(Control::Ping { id, reply }),
            _ => anyhow::bail!("Message hybride invalide"),
        }
        Ok(events)
    }
    fn apply(&mut self, m: Motion, out: &mut Vec<Control>) {
        if m.sequence > self.position {
            self.position = m.sequence;
            out.push(Control::Input {
                lease: m.lease,
                input: Input::Move {
                    position: m.position,
                },
            })
        }
    }
    fn drain(&mut self, out: &mut Vec<Control>) {
        if let Some(p) = self.pending.take() {
            if p.sequence <= self.position || p.after < self.actions {
                return;
            }
            if p.after == self.actions {
                self.apply(p, out)
            } else {
                self.pending = Some(p)
            }
        }
    }
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }
}

pub const DATAGRAM_SIZE: usize = 85;
pub struct Datagram {
    session: Uuid,
    cipher: Aes256Gcm,
    sent: u64,
    received: u64,
    replay: u64,
}
impl Datagram {
    pub fn new(session: Uuid, key: &[u8; 32]) -> Self {
        Self {
            session,
            cipher: Aes256Gcm::new(key.into()),
            sent: 0,
            received: 0,
            replay: 0,
        }
    }
    pub fn encode(&mut self, kind: u8, data: &Motion) -> Result<Vec<u8>> {
        self.sent = self
            .sent
            .checked_add(1)
            .ok_or_else(|| anyhow!("Compteur UDP épuisé"))?;
        let mut header = Vec::with_capacity(DATAGRAM_SIZE);
        header.extend_from_slice(b"MRU1");
        header.extend_from_slice(self.session.as_bytes());
        header.extend_from_slice(&self.sent.to_be_bytes());
        let mut plain = vec![kind];
        plain.extend_from_slice(data.lease.as_bytes());
        plain.extend_from_slice(&data.sequence.to_be_bytes());
        plain.extend_from_slice(&data.after.to_be_bytes());
        plain.extend_from_slice(&data.position.x.to_be_bytes());
        plain.extend_from_slice(&data.position.y.to_be_bytes());
        let mut nonce = [0u8; 12];
        nonce[4..].copy_from_slice(&self.sent.to_be_bytes());
        let encrypted = self
            .cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &plain,
                    aad: &header,
                },
            )
            .map_err(|_| anyhow!("Chiffrement UDP impossible"))?;
        header.extend(encrypted);
        Ok(header)
    }
    pub fn decode(&mut self, packet: &[u8]) -> Option<(u8, Motion)> {
        if packet.len() != DATAGRAM_SIZE
            || &packet[..4] != b"MRU1"
            || packet[4..20] != *self.session.as_bytes()
        {
            return None;
        }
        let count = u64::from_be_bytes(packet[20..28].try_into().ok()?);
        if count == 0
            || (count <= self.received
                && (self.received - count >= 64
                    || (self.replay & (1u64 << (self.received - count))) != 0))
        {
            return None;
        }
        let mut nonce = [0u8; 12];
        nonce[4..].copy_from_slice(&count.to_be_bytes());
        let plain = self
            .cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &packet[28..],
                    aad: &packet[..28],
                },
            )
            .ok()?;
        if !(1..=3).contains(&plain[0]) {
            return None;
        }
        if count > self.received {
            let shift = count - self.received;
            self.replay = if shift >= 64 {
                1
            } else {
                (self.replay << shift) | 1
            };
            self.received = count
        } else {
            self.replay |= 1 << (self.received - count)
        }
        Some((
            plain[0],
            Motion {
                lease: Uuid::from_slice(&plain[1..17]).ok()?,
                sequence: u64::from_be_bytes(plain[17..25].try_into().ok()?),
                after: u64::from_be_bytes(plain[25..33].try_into().ok()?),
                position: Point::new(
                    i32::from_be_bytes(plain[33..37].try_into().ok()?),
                    i32::from_be_bytes(plain[37..41].try_into().ok()?),
                ),
            },
        ))
    }
}
