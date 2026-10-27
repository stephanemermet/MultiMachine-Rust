// Idle connections need liveness checks, not a continuously running timer.
// A new user operation wakes this policy immediately; an unanswered probe keeps
// its original deadline even if more operations arrive in the meantime.
pub(super) const ACTIVE_GRACE_MS: u64 = 2_000;
const ACTIVE_INTERVAL_MS: u64 = 1_000;
const IDLE_INTERVAL_MS: u64 = 30_000;
const RESPONSE_TIMEOUT_MS: u64 = 5_000;

#[derive(Default)]
pub(super) struct Held {
    inputs: std::collections::HashSet<(uuid::Uuid, u32)>,
    gestures: std::collections::HashSet<uuid::Uuid>,
}
impl Held {
    pub fn observe(&mut self, event: &crate::model::Control) -> bool {
        use crate::model::{Control, Input};
        let item = match event {
            Control::Input {
                lease,
                input: Input::Key {
                    key, extended, up, ..
                },
            } => Some(((*lease, *key as u32 | ((*extended as u32) << 10)), *up)),
            Control::Input {
                lease,
                input: Input::Button { button, up },
            } => Some(((*lease, 256 + *button as u32), *up)),
            _ => None,
        };
        if let Some((key, up)) = item {
            if up {
                self.inputs.remove(&key);
            } else {
                self.inputs.insert(key);
            }
        }
        match event {
            Control::End { lease } => self.inputs.retain(|(id, _)| id != lease),
            Control::DragPause { offer, .. }
            | Control::DragStart { offer, .. }
            | Control::DragHome { offer, .. } => {
                self.gestures.insert(*offer);
            }
            Control::DragDropped { offer }
            | Control::DragCancel { offer }
            | Control::DragFinished { offer, .. } => {
                self.gestures.remove(offer);
            }
            _ => {}
        }
        !self.inputs.is_empty() || !self.gestures.is_empty()
    }
}

#[derive(Default)]
pub(super) struct Health {
    last_probe: Option<u64>,
    waiting: Option<u64>,
}
pub(super) struct Step {
    pub probe: bool,
    pub wake_at: u64,
}
impl Health {
    pub fn step(&mut self, now: u64, active: bool, received: u64) -> Option<Step> {
        if self.waiting.is_some_and(|sent| received >= sent) {
            self.waiting = None;
        }
        if self
            .waiting
            .is_some_and(|sent| now.saturating_sub(sent) >= RESPONSE_TIMEOUT_MS)
        {
            return None;
        }
        let interval = if active {
            ACTIVE_INTERVAL_MS
        } else {
            IDLE_INTERVAL_MS
        };
        let next = self
            .last_probe
            .map_or(now, |sent| sent.saturating_add(interval));
        let probe = self.waiting.is_none() && now >= next;
        if probe {
            self.last_probe = Some(now);
            self.waiting = Some(now);
        }
        Some(Step {
            probe,
            wake_at: self
                .waiting
                .map_or(next, |sent| sent.saturating_add(RESPONSE_TIMEOUT_MS)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn held_input_and_drag_keep_failure_detection_fast_until_released() {
        use crate::model::{Control, Input, Point};
        let mut held = Held::default();
        let lease = uuid::Uuid::new_v4();
        let offer = uuid::Uuid::new_v4();
        assert!(held.observe(&Control::Input {
            lease,
            input: Input::Button {
                button: 1,
                up: false
            }
        }));
        assert!(held.observe(&Control::Input {
            lease,
            input: Input::Key {
                key: 17,
                scan: 29,
                extended: false,
                up: false
            }
        }));
        assert!(held.observe(&Control::Input {
            lease,
            input: Input::Button {
                button: 1,
                up: true
            }
        }));
        assert!(!held.observe(&Control::End { lease }));
        assert!(held.observe(&Control::DragStart {
            lease,
            offer,
            position: Point::new(0, 0)
        }));
        assert!(!held.observe(&Control::DragCancel { offer }));
        assert!(held.observe(&Control::DragHome {
            lease,
            offer,
            position: Point::new(0, 0)
        }));
        assert!(!held.observe(&Control::DragFinished {
            offer,
            dropped: true
        }));
    }
    #[test]
    fn idle_waits_thirty_seconds_and_activity_wakes_it() {
        let mut health = Health::default();
        assert!(health.step(100, false, 99).unwrap().probe);
        let idle = health.step(120, false, 120).unwrap();
        assert!(!idle.probe);
        assert_eq!(idle.wake_at, 30_100);
        assert!(!health.step(1_100, false, 120).unwrap().probe);
        assert!(health.step(20_000, true, 120).unwrap().probe);
        assert!(!health.step(20_010, true, 20_010).unwrap().probe);
        assert!(health.step(21_000, true, 20_010).unwrap().probe);
        assert_eq!(health.step(21_010, false, 21_010).unwrap().wake_at, 51_000);
        assert!(health.step(51_000, false, 21_010).unwrap().probe);
    }
    #[test]
    fn unanswered_probe_expires_without_postponement_or_false_idle_timeout() {
        let mut health = Health::default();
        assert!(health.step(100, false, 99).unwrap().probe);
        for now in [200, 1_100, 2_100, 4_100] {
            let step = health.step(now, true, 99).unwrap();
            assert!(!step.probe);
            assert_eq!(step.wake_at, 5_100);
        }
        assert!(health.step(5_100, true, 99).is_none());
        let mut health = Health::default();
        health.step(100, false, 99).unwrap();
        health.step(120, false, 120).unwrap();
        assert!(!health.step(20_000, false, 120).unwrap().probe);
        assert!(health.step(30_100, false, 120).unwrap().probe);
        assert!(health.step(35_100, false, 120).is_none());
    }
}
