use crate::model::*;
use anyhow::{Context, Result, ensure};
use std::{
    collections::{HashMap, VecDeque},
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub fn safe_name(name: &str) -> bool {
    if name.is_empty() || name.encode_utf16().count() > 259 {
        return false;
    }
    name.split(['\\', '/']).all(|s| {
        if s.is_empty()
            || s == "."
            || s == ".."
            || s.ends_with([' ', '.'])
            || s.chars().any(|c| c < ' ' || "<>:\"|?*".contains(c))
        {
            return false;
        }
        let stem = s.split('.').next().unwrap_or("").to_uppercase();
        !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            && !(stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && matches!(stem.as_bytes()[3], b'1'..=b'9'))
    })
}
fn reparse(m: &fs::Metadata) -> bool {
    if m.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        m.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    false
}
pub fn no_links(path: &Path) -> Result<()> {
    for p in path.ancestors() {
        if p.as_os_str().is_empty() {
            continue;
        }
        ensure!(
            !reparse(&fs::symlink_metadata(p)?),
            "Liens et jonctions refusés"
        );
    }
    Ok(())
}
pub fn filetime(time: SystemTime) -> u64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(d) => 116_444_736_000_000_000u64
            .saturating_add(d.as_secs().saturating_mul(10_000_000))
            .saturating_add((d.subsec_nanos() / 100) as u64),
        Err(_) => 116_444_736_000_000_000,
    }
}
#[derive(Clone)]
struct Authorized {
    path: PathBuf,
    entry: FileEntry,
}
#[derive(Default)]
pub struct Catalog {
    registry: parking_lot::Mutex<Registry>,
}
#[derive(Default)]
struct Registry {
    offers: HashMap<Uuid, HashMap<Uuid, Authorized>>,
    order: VecDeque<Uuid>,
}
impl Catalog {
    pub fn register(&self, id: Uuid, paths: &[PathBuf]) -> Result<Vec<FileEntry>> {
        ensure!(
            !id.is_nil() && paths.len() <= MAX_FILES,
            "Liste trop grande"
        );
        let mut found = HashMap::new();
        let mut entries = vec![];
        for path in paths {
            no_links(path)?;
            let path = fs::canonicalize(path)?;
            let name = path
                .file_name()
                .context("Copie d’un volume entier refusée")?
                .to_str()
                .context("Nom Unicode invalide")?
                .to_owned();
            Self::walk(&path, name, &mut found, &mut entries)?;
        }
        let offer = Offer {
            id,
            clock: 0,
            origin: Uuid::new_v4(),
            formats: Default::default(),
            files: entries.clone(),
        };
        offer.validate()?;
        let mut registry = self.registry.lock();
        registry.offers.remove(&id);
        registry.order.retain(|i| *i != id);
        registry.offers.insert(id, found);
        registry.order.push_back(id);
        while registry.order.len() > 16 {
            if let Some(old) = registry.order.pop_front() {
                registry.offers.remove(&old);
            }
        }
        Ok(entries)
    }
    fn walk(
        path: &Path,
        name: String,
        found: &mut HashMap<Uuid, Authorized>,
        entries: &mut Vec<FileEntry>,
    ) -> Result<()> {
        ensure!(
            entries.len() < MAX_FILES && safe_name(&name),
            "Nom trop long, réservé ou trop d’éléments"
        );
        let meta = fs::symlink_metadata(path)?;
        ensure!(
            !reparse(&meta) && (meta.is_file() || meta.is_dir()),
            "Fichier spécial, lien ou jonction refusé"
        );
        let entry = FileEntry {
            id: Uuid::new_v4(),
            name: name.clone(),
            length: if meta.is_file() { meta.len() } else { 0 },
            directory: meta.is_dir(),
            modified: filetime(meta.modified()?),
        };
        if meta.is_file() {
            found.insert(
                entry.id,
                Authorized {
                    path: path.into(),
                    entry: entry.clone(),
                },
            );
        }
        entries.push(entry);
        if meta.is_dir() {
            let mut children = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
            children.sort_by_key(|e| e.file_name());
            for child in children {
                let child_name = child
                    .file_name()
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("Nom Unicode invalide"))?;
                Self::walk(
                    &child.path(),
                    format!("{name}\\{child_name}"),
                    found,
                    entries,
                )?;
            }
        }
        Ok(())
    }
    pub fn remove(&self, id: Uuid) {
        let mut registry = self.registry.lock();
        registry.offers.remove(&id);
        registry.order.retain(|i| *i != id);
    }
    pub fn read(&self, offer: Uuid, id: Uuid, offset: u64, count: usize) -> Result<Vec<u8>> {
        ensure!(count > 0 && count <= CHUNK, "Taille de lecture invalide");
        let f = self
            .registry
            .lock()
            .offers
            .get(&offer)
            .and_then(|m| m.get(&id))
            .cloned()
            .context("Fichier non autorisé ou offre expirée")?;
        no_links(&f.path)?;
        let mut input = File::open(&f.path)?;
        let meta = input.metadata()?;
        ensure!(
            meta.len() == f.entry.length && filetime(meta.modified()?) == f.entry.modified,
            "La source a changé depuis sa copie"
        );
        ensure!(offset <= f.entry.length, "Position de lecture invalide");
        input.seek(SeekFrom::Start(offset))?;
        let size = (f.entry.length - offset).min(count as u64) as usize;
        let mut bytes = vec![0; size];
        input.read_exact(&mut bytes)?;
        Ok(bytes)
    }
}

pub struct Clock {
    origin: Uuid,
    counter: u64,
    current: (u64, Uuid),
}
impl Clock {
    pub fn new(origin: Uuid) -> Self {
        Self {
            origin,
            counter: 0,
            current: (0, Uuid::nil()),
        }
    }
    pub fn next_clock(&mut self) -> Result<u64> {
        self.counter = self.counter.checked_add(1).context("Horloge épuisée")?;
        self.current = (self.counter, self.origin);
        Ok(self.counter)
    }
    pub fn accept(&mut self, offer: &Offer) -> bool {
        self.counter = self.counter.max(offer.clock);
        let next = (offer.clock, offer.origin);
        if next <= self.current {
            return false;
        }
        self.current = next;
        true
    }
}

pub fn html_to_text(data: &[u8]) -> String {
    let text = String::from_utf8_lossy(data);
    let text = text.trim_end_matches('\0');
    let lower = text.to_ascii_lowercase();
    let fragment = match (
        lower.find("<!--startfragment-->"),
        lower.find("<!--endfragment-->"),
    ) {
        (Some(a), Some(b)) if b > a => &text[a + 20..b],
        _ => match text.find('<') {
            Some(a) => &text[a..],
            None => return String::new(),
        },
    };
    // All expressions are fixed and the regex engine is linear-time on hostile input.
    let script =
        regex::Regex::new(r"(?is)<script\b[^>]*>.*?</script\s*>|<style\b[^>]*>.*?</style\s*>")
            .unwrap()
            .replace_all(fragment, "");
    let lines = regex::Regex::new(r"(?i)<\s*(br\b[^>]*|/(p|div|li|tr|h[1-6])\s*)>")
        .unwrap()
        .replace_all(&script, "\r\n")
        .into_owned();
    let clean = regex::Regex::new(r"<[^>]+>")
        .unwrap()
        .replace_all(&lines, "")
        .into_owned();
    html_escape::decode_html_entities(&clean).trim().to_owned()
}
