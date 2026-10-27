use crate::model::*;
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug)]
pub struct Layout {
    pub local: Desktop,
    pub remote: Desktop,
    pub placement: Placement,
    pub local_screens: Vec<Monitor>,
    pub remote_screens: Vec<Monitor>,
    origin: Point,
}
impl Layout {
    pub fn new(local: Desktop, remote: Desktop, placement: Placement) -> Result<Self> {
        local.validate()?;
        remote.validate()?;
        ensure!(
            placement.offset.unsigned_abs() <= 20000,
            "Décalage trop grand"
        );
        let origin = Point::new(
            if placement.right {
                local.width()
            } else {
                -remote.width()
            },
            placement.offset,
        );
        let local_screens = local
            .monitors
            .iter()
            .map(|m| Monitor {
                x: m.x - local.left(),
                y: m.y - local.top(),
                ..m.clone()
            })
            .collect();
        let remote_screens = remote
            .monitors
            .iter()
            .map(|m| Monitor {
                x: m.x - remote.left() + origin.x,
                y: m.y - remote.top() + origin.y,
                ..m.clone()
            })
            .collect();
        Ok(Self {
            local,
            remote,
            placement,
            local_screens,
            remote_screens,
            origin,
        })
    }
    pub fn world(&self, remote: bool, p: Point) -> Point {
        if remote {
            Point::new(
                p.x - self.remote.left() + self.origin.x,
                p.y - self.remote.top() + self.origin.y,
            )
        } else {
            Point::new(p.x - self.local.left(), p.y - self.local.top())
        }
    }
    pub fn native(&self, remote: bool, p: Point) -> Point {
        if remote {
            Point::new(
                p.x + self.remote.left() - self.origin.x,
                p.y + self.remote.top() - self.origin.y,
            )
        } else {
            Point::new(p.x + self.local.left(), p.y + self.local.top())
        }
    }
    pub fn direction(&self) -> i32 {
        if self.placement.right { 1 } else { -1 }
    }
    pub fn exit(&self, p: Point, dx: i32) -> Option<Point> {
        let p = self.world(false, p);
        let next = Point::new(p.x + self.direction(), p.y);
        (dx * self.direction() > 0
            && !self.local_screens.iter().any(|m| m.contains(next))
            && self.remote_screens.iter().any(|m| m.contains(next)))
        .then(|| self.native(true, next))
    }
    pub fn move_remote(&self, p: Point, dx: i32, dy: i32) -> (bool, Point) {
        let start = self.world(true, p);
        let end = Point::new(
            start.x + dx.clamp(-32768, 32768),
            start.y + dy.clamp(-32768, 32768),
        );
        if self
            .remote_screens
            .iter()
            .any(|m| m.contains(start) && m.contains(end))
        {
            return (true, self.native(true, end));
        }
        let steps = (end.x - start.x).abs().max((end.y - start.y).abs());
        let mut last = start;
        let mut previous = start;
        for i in 1..=steps {
            let interp = Point::new(
                start.x + (end.x - start.x) * i / steps,
                start.y + (end.y - start.y) * i / steps,
            );
            let p = Point::new(
                last.x + interp.x - previous.x,
                last.y + interp.y - previous.y,
            );
            previous = interp;
            if self.local_screens.iter().any(|m| m.contains(p)) {
                return (false, self.native(false, p));
            }
            if self.remote_screens.iter().any(|m| m.contains(p)) {
                last = p
            } else if let Some(m) = self.remote_screens.iter().find(|m| m.contains(last)) {
                last = m.clamp(p)
            }
        }
        (true, self.native(true, last))
    }
    pub fn inset(&self, p: Point) -> Point {
        self.remote
            .monitors
            .iter()
            .find(|m| m.contains(p))
            .map(|m| m.clamp(Point::new(p.x + self.direction() * 8, p.y)))
            .unwrap_or(p)
    }
    pub fn reverse(&self) -> Self {
        Self::new(
            self.remote.clone(),
            self.local.clone(),
            Placement {
                right: !self.placement.right,
                offset: -self.placement.offset,
            },
        )
        .expect("validated inverse layout")
    }
    pub fn shared(&self) -> bool {
        self.local_screens.iter().any(|a| {
            self.remote_screens.iter().any(|b| {
                (a.right() == b.x || b.right() == a.x) && a.y.max(b.y) < a.bottom().min(b.bottom())
            })
        })
    }
    pub fn portals(&self) -> Vec<Monitor> {
        let mut v = vec![];
        for a in &self.local_screens {
            for b in &self.remote_screens {
                if if self.placement.right {
                    a.right() != b.x
                } else {
                    b.right() != a.x
                } {
                    continue;
                }
                let top = a.y.max(b.y);
                let bottom = a.bottom().min(b.bottom());
                if top >= bottom {
                    continue;
                }
                v.push(Monitor {
                    name: a.name.clone(),
                    x: if self.placement.right {
                        a.right() - 6
                    } else {
                        a.x
                    } + self.local.left(),
                    y: top + self.local.top(),
                    width: 6,
                    height: bottom - top,
                });
            }
        }
        v
    }
}
pub fn key(a: &Desktop, b: &Desktop) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("{:?}|{:?}", a.geometry(), b.geometry()).as_bytes())
    )
}
pub fn adapt(a: &Desktop, b: &Desktop, p: Placement) -> Placement {
    if Layout::new(a.clone(), b.clone(), p).is_ok_and(|l| l.shared()) {
        return p;
    }
    let Ok(zero) = Layout::new(a.clone(), b.clone(), Placement { offset: 0, ..p }) else {
        return p;
    };
    let mut values = vec![];
    for a in &zero.local_screens {
        for b in &zero.remote_screens {
            if a.right() != b.x && b.right() != a.x {
                continue;
            }
            let min = (-20000).max(a.y - b.bottom() + 1);
            let max = 20000.min(a.bottom() - b.y - 1);
            if min <= max {
                values.push(p.offset.clamp(min, max))
            }
        }
    }
    Placement {
        offset: values
            .into_iter()
            .min_by_key(|v| (*v - p.offset).abs())
            .unwrap_or(p.offset),
        ..p
    }
}
