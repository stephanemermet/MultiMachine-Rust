use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

pub const PORT: u16 = 24882;
pub const DISCOVERY_PORT: u16 = 24883;
pub const CHUNK: usize = 128 * 1024;
pub const MAX_FILES: usize = 4096;
pub const MAX_CLIPBOARD: usize = 32 * 1024 * 1024;
pub const CONTROL_FRAME: usize = 256 * 1024;
pub const BULK_FRAME: usize = 48 * 1024 * 1024;
pub const INPUT_TAG: usize = 0x4d525331;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}
impl Point {
    pub fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Monitor {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}
impl Monitor {
    pub fn right(&self) -> i32 {
        self.x + self.width
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.height
    }
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }
    pub fn clamp(&self, p: Point) -> Point {
        Point::new(
            p.x.clamp(self.x, self.right() - 1),
            p.y.clamp(self.y, self.bottom() - 1),
        )
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Desktop {
    pub name: String,
    pub monitors: Vec<Monitor>,
}
impl Default for Desktop {
    fn default() -> Self {
        Self {
            name: "PC".into(),
            monitors: vec![],
        }
    }
}
impl Desktop {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.name.is_empty() && self.name.len() <= 1024 && self.monitors.len() <= 32,
            "Description d’écran invalide"
        );
        for m in &self.monitors {
            ensure!(
                (1..=32768).contains(&m.width)
                    && (1..=32768).contains(&m.height)
                    && (m.x as i64).abs() <= 100000
                    && (m.y as i64).abs() <= 100000
                    && m.name.len() <= 1024,
                "Géométrie invalide"
            );
        }
        Ok(())
    }
    pub fn left(&self) -> i32 {
        self.monitors.iter().map(|m| m.x).min().unwrap_or(0)
    }
    pub fn top(&self) -> i32 {
        self.monitors.iter().map(|m| m.y).min().unwrap_or(0)
    }
    pub fn width(&self) -> i32 {
        self.monitors.iter().map(Monitor::right).max().unwrap_or(0) - self.left()
    }
    pub fn height(&self) -> i32 {
        self.monitors.iter().map(Monitor::bottom).max().unwrap_or(0) - self.top()
    }
    pub fn clamp(&self, p: Point) -> Point {
        self.monitors
            .iter()
            .map(|m| m.clamp(p))
            .min_by_key(|q| (q.x as i64 - p.x as i64).pow(2) + (q.y as i64 - p.y as i64).pow(2))
            .unwrap_or(p)
    }
    pub fn geometry(&self) -> Vec<(i32, i32, i32, i32)> {
        let mut v: Vec<_> = self
            .monitors
            .iter()
            .map(|m| (m.x, m.y, m.width, m.height))
            .collect();
        v.sort();
        v
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Placement {
    pub right: bool,
    pub offset: i32,
}
impl Default for Placement {
    fn default() -> Self {
        Self {
            right: true,
            offset: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Input {
    Move {
        position: Point,
    },
    Button {
        button: u8,
        up: bool,
    },
    Key {
        key: u16,
        scan: u16,
        extended: bool,
        up: bool,
    },
    Wheel {
        horizontal: bool,
        delta: i16,
    },
}
impl Input {
    pub fn position(&self) -> Option<Point> {
        if let Self::Move { position } = self {
            Some(*position)
        } else {
            None
        }
    }
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Move { position } => ensure!(
                (position.x as i64).abs() <= 150000 && (position.y as i64).abs() <= 150000,
                "Position invalide"
            ),
            Self::Button { button, .. } => ensure!((1..=5).contains(button), "Bouton invalide"),
            Self::Key { key, scan, .. } => {
                ensure!((1..=254).contains(key) && *scan <= 255, "Touche invalide")
            }
            Self::Wheel { .. } => (),
        }
        Ok(())
    }
    pub fn release(&self) -> Option<Self> {
        match self {
            Self::Button { button, .. } => Some(Self::Button {
                button: *button,
                up: true,
            }),
            Self::Key {
                key,
                scan,
                extended,
                ..
            } => Some(Self::Key {
                key: *key,
                scan: *scan,
                extended: *extended,
                up: true,
            }),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Format {
    Text,
    Html,
    Rtf,
    Png,
    Dib,
    DibV5,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bytes(#[serde(with = "base64_bytes")] pub Vec<u8>);
mod base64_bytes {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use serde::{Deserialize, Deserializer, Serializer, de::Error};
    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        STANDARD
            .decode(String::deserialize(d)?)
            .map_err(D::Error::custom)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    pub id: Uuid,
    pub name: String,
    pub length: u64,
    pub directory: bool,
    pub modified: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub id: Uuid,
    pub clock: u64,
    pub origin: Uuid,
    pub formats: BTreeMap<Format, Bytes>,
    pub files: Vec<FileEntry>,
}
impl Offer {
    pub fn empty(clock: u64, origin: Uuid) -> Self {
        Self {
            id: Uuid::new_v4(),
            clock,
            origin,
            formats: BTreeMap::new(),
            files: vec![],
        }
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.id.is_nil()
                && !self.origin.is_nil()
                && self.clock < u64::MAX - 1_000_000
                && self.files.len() <= MAX_FILES,
            "Offre invalide"
        );
        ensure!(
            self.formats.values().map(|b| b.0.len()).sum::<usize>() <= MAX_CLIPBOARD,
            "Presse-papiers trop volumineux"
        );
        let mut ids = std::collections::HashSet::new();
        let mut names = BTreeMap::new();
        for f in &self.files {
            ensure!(
                !f.id.is_nil()
                    && ids.insert(f.id)
                    && crate::files::safe_name(&f.name)
                    && f.length <= i64::MAX as u64
                    && f.modified <= 2_650_467_743_999_999_999,
                "Descripteur invalide"
            );
            ensure!(
                names
                    .insert(f.name.replace('/', "\\").to_uppercase(), f.directory)
                    .is_none(),
                "Nom dupliqué"
            );
        }
        for name in names.keys() {
            for (i, _) in name.match_indices('\\') {
                ensure!(
                    names.get(&name[..i]) != Some(&false),
                    "Un fichier ne peut pas être un dossier parent"
                );
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Control {
    Desktop {
        desktop: Desktop,
    },
    Placement {
        placement: Placement,
    },
    Begin {
        lease: Uuid,
        position: Point,
    },
    Grant {
        lease: Uuid,
    },
    End {
        lease: Uuid,
    },
    Input {
        lease: Uuid,
        input: Input,
    },
    DragPause {
        offer: Uuid,
        lease: Uuid,
        position: Point,
    },
    DragReady {
        offer: Uuid,
    },
    DragStart {
        offer: Uuid,
        lease: Uuid,
        position: Point,
    },
    DragHome {
        offer: Uuid,
        lease: Uuid,
        position: Point,
    },
    DragRunning {
        offer: Uuid,
    },
    DragDropped {
        offer: Uuid,
    },
    DragCancel {
        offer: Uuid,
    },
    DragFinished {
        offer: Uuid,
        dropped: bool,
    },
    Ping {
        id: u64,
        reply: bool,
    },
}
impl Control {
    pub fn motion(&self) -> Option<(Uuid, Point)> {
        if let Self::Input {
            lease,
            input: Input::Move { position },
        } = self
        {
            Some((*lease, *position))
        } else {
            None
        }
    }
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Desktop { desktop } => desktop.validate()?,
            Self::Placement { placement } => ensure!(
                placement.offset.unsigned_abs() <= 20000,
                "Décalage invalide"
            ),
            Self::Input { lease, input } => {
                ensure!(!lease.is_nil(), "Contrôle invalide");
                input.validate()?;
            }
            Self::Begin { lease, position } => {
                ensure!(!lease.is_nil(), "Contrôle invalide");
                Input::Move {
                    position: *position,
                }
                .validate()?;
            }
            Self::Grant { lease } | Self::End { lease } => {
                ensure!(!lease.is_nil(), "Contrôle invalide")
            }
            Self::DragPause {
                offer,
                lease,
                position,
            }
            | Self::DragStart {
                offer,
                lease,
                position,
            }
            | Self::DragHome {
                offer,
                lease,
                position,
            } => {
                ensure!(!offer.is_nil() && !lease.is_nil(), "Geste invalide");
                Input::Move {
                    position: *position,
                }
                .validate()?;
            }
            Self::DragReady { offer }
            | Self::DragRunning { offer }
            | Self::DragDropped { offer }
            | Self::DragCancel { offer }
            | Self::DragFinished { offer, .. } => ensure!(!offer.is_nil(), "Geste invalide"),
            Self::Ping { .. } => (),
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Bulk {
    Clipboard {
        offer: Offer,
    },
    Drag {
        offer: Offer,
    },
    Read {
        request: u64,
        offer: Uuid,
        file: Uuid,
        offset: u64,
        count: u32,
    },
    Data {
        request: u64,
        bytes: Bytes,
        error: Option<String>,
    },
}
impl Bulk {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Clipboard { offer } | Self::Drag { offer } => offer.validate(),
            Self::Read { count, offset, .. } => {
                ensure!(
                    *count > 0 && *count as usize <= CHUNK && *offset <= i64::MAX as u64,
                    "Lecture invalide"
                );
                Ok(())
            }
            Self::Data { bytes, error, .. } => {
                ensure!(
                    bytes.0.len() <= CHUNK && error.as_ref().is_none_or(|s| s.len() <= 2048),
                    "Réponse invalide"
                );
                Ok(())
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub server: bool,
    pub host: String,
    pub port: u16,
    pub hybrid: bool,
    pub plain: bool,
    pub trails: bool,
    pub auto_start: bool,
    pub auto_connect: bool,
    pub minimized: bool,
    pub placement: Placement,
    pub profiles: BTreeMap<String, Placement>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            server: true,
            host: String::new(),
            port: PORT,
            hybrid: true,
            plain: false,
            trails: false,
            auto_start: false,
            auto_connect: false,
            minimized: false,
            placement: Placement::default(),
            profiles: BTreeMap::new(),
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        if self.port < 1024
            || self.host.len() > 253
            || self.placement.offset.unsigned_abs() > 20000
            || self.profiles.len() > 256
        {
            bail!("Réglages invalides")
        }
        Ok(())
    }
}
