//! Windows FILEGROUPDESCRIPTORW wire layout, independent of a COM apartment.
use crate::{files::safe_name, model::*};
use anyhow::{Result, ensure};
use std::collections::HashSet;
pub const DESCRIPTOR: usize = 592;
#[derive(Clone, Debug)]
pub struct VirtualFile {
    pub name: String,
    pub length: Option<u64>,
    pub directory: bool,
    pub modified: u64,
}
pub fn descriptors(files: &[FileEntry]) -> Vec<u8> {
    let mut data = vec![0; 4 + DESCRIPTOR * files.len()];
    data[..4].copy_from_slice(&(files.len() as u32).to_le_bytes());
    for (i, f) in files.iter().enumerate() {
        let b = &mut data[4 + i * DESCRIPTOR..4 + (i + 1) * DESCRIPTOR];
        b[..4].copy_from_slice(&0x80004064u32.to_le_bytes());
        b[36..40].copy_from_slice(&(if f.directory { 16u32 } else { 128 }).to_le_bytes());
        b[56..64].copy_from_slice(&f.modified.to_le_bytes());
        b[64..68].copy_from_slice(&((f.length >> 32) as u32).to_le_bytes());
        b[68..72].copy_from_slice(&(f.length as u32).to_le_bytes());
        for (j, c) in f.name.encode_utf16().enumerate() {
            b[72 + j * 2..74 + j * 2].copy_from_slice(&c.to_le_bytes());
        }
    }
    data
}
pub fn parse(bytes: &[u8]) -> Result<Vec<VirtualFile>> {
    ensure!(bytes.len() >= 4, "Descripteur absent");
    let n = u32::from_le_bytes(bytes[..4].try_into()?) as usize;
    ensure!(
        n > 0 && n <= MAX_FILES && bytes.len() >= 4 + n * DESCRIPTOR,
        "Descripteurs incomplets"
    );
    let mut files = vec![];
    let mut names = HashSet::new();
    for b in bytes[4..4 + n * DESCRIPTOR].chunks_exact(DESCRIPTOR) {
        let flags = u32::from_le_bytes(b[..4].try_into()?);
        let attr = u32::from_le_bytes(b[36..40].try_into()?);
        let utf: Vec<_> = b[72..]
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        let end = utf
            .iter()
            .position(|c| *c == 0)
            .ok_or_else(|| anyhow::anyhow!("Nom non terminé"))?;
        let name = String::from_utf16(&utf[..end])?.replace('/', "\\");
        ensure!(safe_name(&name), "Nom de fichier non sûr");
        ensure!(names.insert(name.to_uppercase()), "Noms dupliqués");
        ensure!(
            flags & 4 == 0 || attr & 0x400 == 0,
            "Point de réanalyse refusé"
        );
        let directory = flags & 4 != 0 && attr & 16 != 0;
        let length = (flags & 0x40 != 0).then(|| {
            ((u32::from_le_bytes(b[64..68].try_into().unwrap()) as u64) << 32)
                | u32::from_le_bytes(b[68..72].try_into().unwrap()) as u64
        });
        let modified = if flags & 0x20 != 0 {
            u64::from_le_bytes(b[56..64].try_into()?)
        } else {
            0
        };
        ensure!(modified <= 2_650_467_743_999_999_999, "Date invalide");
        files.push(VirtualFile {
            name,
            length,
            directory,
            modified,
        });
    }
    for f in &files {
        let mut parent = f.name.as_str();
        while let Some((prefix, _)) = parent.rsplit_once('\\') {
            ensure!(
                !files
                    .iter()
                    .any(|e| !e.directory && e.name.eq_ignore_ascii_case(prefix)),
                "Un parent est un fichier"
            );
            parent = prefix;
        }
    }
    Ok(files)
}
