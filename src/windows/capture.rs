use super::{data::*, platform::*, storage};
use anyhow::{Result, ensure};
use multimachine_rust::{
    files::{Catalog, no_links},
    model::*,
    virtuals,
};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};
use uuid::Uuid;
use windows::{
    Win32::{
        System::{Com::*, Memory::*},
        UI::{Controls::RichEdit::*, Shell::*, WindowsAndMessaging::*},
    },
    core::*,
};
pub fn supports(data: &IDataObject) -> bool {
    unsafe {
        data.QueryGetData(&format(15, TYMED_HGLOBAL.0 as u32, -1))
            .is_ok()
            || data
                .QueryGetData(&format(
                    registered("FileGroupDescriptorW"),
                    TYMED_HGLOBAL.0 as u32,
                    -1,
                ))
                .is_ok()
    }
}
pub fn paths(data: &IDataObject) -> Result<Vec<PathBuf>> {
    unsafe {
        let medium = get(data, &format(15, TYMED_HGLOBAL.0 as u32, -1))?;
        ensure!(
            medium.0.tymed == TYMED_HGLOBAL.0 as u32,
            "Liste OLE invalide"
        );
        let h = HDROP(medium.0.u.hGlobal.0);
        let n = DragQueryFileW(h, u32::MAX, None);
        ensure!(n > 0 && n as usize <= MAX_FILES, "Liste trop grande");
        let mut paths = vec![];
        for i in 0..n {
            let n = DragQueryFileW(h, i, None);
            ensure!(n > 0 && n <= 32768, "Chemin invalide");
            let mut name = vec![0; n as usize + 1];
            ensure!(
                DragQueryFileW(h, i, Some(&mut name)) == n,
                "Chemin incomplet"
            );
            paths.push(PathBuf::from(String::from_utf16(&name[..n as usize])?));
        }
        Ok(paths)
    }
}
fn cancelled(stop: &AtomicBool) -> Result<()> {
    ensure!(!stop.load(Ordering::Acquire), "Geste annulé");
    Ok(())
}
pub fn files(data: &IDataObject, catalog: &Arc<Catalog>, stop: &AtomicBool) -> Result<Offer> {
    let paths = unsafe {
        if data
            .QueryGetData(&format(15, TYMED_HGLOBAL.0 as u32, -1))
            .is_ok()
        {
            paths(data)?
        } else {
            virtual_files(data, stop)?
        }
    };
    cancelled(stop)?;
    let mut offer = Offer::empty(0, Uuid::new_v4());
    offer.files = catalog.register(offer.id, &paths)?;
    Ok(offer)
}
fn virtual_files(data: &IDataObject, stop: &AtomicBool) -> Result<Vec<PathBuf>> {
    unsafe {
        let medium = get(
            data,
            &format(
                registered("FileGroupDescriptorW"),
                TYMED_HGLOBAL.0 as u32,
                -1,
            ),
        )?;
        ensure!(
            medium.0.tymed == TYMED_HGLOBAL.0 as u32,
            "Descripteur invalide"
        );
        let entries = virtuals::parse(&read_global(
            medium.0.u.hGlobal,
            4 + MAX_FILES * virtuals::DESCRIPTOR,
        )?)?;
        let cache = storage::root().join("DragCache");
        fs::create_dir_all(&cache)?;
        no_links(&cache)?;
        let mut used = 0;
        for entry in fs::read_dir(&cache)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if Uuid::parse_str(&name).is_err() {
                continue;
            }
            if no_links(&entry.path()).is_err() {
                continue;
            }
            if entry
                .metadata()?
                .modified()
                .ok()
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .is_some_and(|d| d > Duration::from_secs(2 * 86400))
            {
                let _ = fs::remove_dir_all(entry.path());
            } else {
                used += cache_size(&entry.path())?;
            }
        }
        let budget =
            (2u64 * 1024 * 1024 * 1024).min((4u64 * 1024 * 1024 * 1024).saturating_sub(used));
        ensure!(budget > 0, "Cache de fichiers virtuels plein");
        let root = cache.join(Uuid::new_v4().to_string());
        fs::create_dir(&root)?;
        let mut total = 0u64;
        let result = (|| -> Result<Vec<PathBuf>> {
            for (index, entry) in entries.iter().enumerate() {
                cancelled(stop)?;
                let path = entry
                    .name
                    .split(['\\', '/'])
                    .fold(root.clone(), |p, s| p.join(s));
                ensure!(path.starts_with(&root), "Chemin hors cache");
                if entry.directory {
                    fs::create_dir_all(&path)?;
                    continue;
                }
                fs::create_dir_all(path.parent().unwrap())?;
                no_links(path.parent().unwrap())?;
                let medium = get(
                    data,
                    &format(
                        registered("FileContents"),
                        (TYMED_ISTREAM.0 | TYMED_HGLOBAL.0) as u32,
                        index as i32,
                    ),
                )?;
                let mut output = fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&path)?;
                let mut length = 0u64;
                if medium.0.tymed == TYMED_ISTREAM.0 as u32 {
                    let stream = medium
                        .0
                        .u
                        .pstm
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Flux absent"))?;
                    let mut buffer = vec![0; CHUNK];
                    loop {
                        cancelled(stop)?;
                        let mut n = 0;
                        stream
                            .Read(
                                buffer.as_mut_ptr().cast(),
                                buffer.len() as u32,
                                Some(&mut n),
                            )
                            .ok()?;
                        ensure!(n as usize <= buffer.len(), "Flux invalide");
                        if n == 0 {
                            break;
                        }
                        total += n as u64;
                        length += n as u64;
                        ensure!(
                            total <= budget,
                            "Cache limité à 2 Gio par geste et 4 Gio au total"
                        );
                        output.write_all(&buffer[..n as usize])?;
                    }
                } else if medium.0.tymed == TYMED_HGLOBAL.0 as u32 {
                    let h = medium.0.u.hGlobal;
                    let size = GlobalSize(h) as u64;
                    length = entry.length.unwrap_or(size);
                    ensure!(length <= size, "Fichier incomplet");
                    total = total
                        .checked_add(length)
                        .ok_or_else(|| anyhow::anyhow!("Taille invalide"))?;
                    ensure!(total <= budget, "Cache de fichiers virtuels plein");
                    let p = GlobalLock(h);
                    ensure!(!p.is_null(), "Contenu inaccessible");
                    let copied = (|| -> Result<()> {
                        let mut offset = 0;
                        while offset < length {
                            cancelled(stop)?;
                            let n = (length - offset).min(CHUNK as u64) as usize;
                            output.write_all(std::slice::from_raw_parts(
                                p.cast::<u8>().add(offset as usize),
                                n,
                            ))?;
                            offset += n as u64;
                        }
                        Ok(())
                    })();
                    let _ = GlobalUnlock(h);
                    copied?;
                } else {
                    anyhow::bail!("Le fournisseur ne propose pas de flux de fichier")
                }
                ensure!(
                    entry.length.is_none_or(|n| n == length),
                    "Fichier virtuel incomplet"
                );
                output.sync_all()?;
                if entry.modified != 0 {
                    let epoch = 116_444_736_000_000_000u64;
                    let nanos = entry.modified.abs_diff(epoch) as u128 * 100;
                    let duration = Duration::new(
                        (nanos / 1_000_000_000) as u64,
                        (nanos % 1_000_000_000) as u32,
                    );
                    let modified = if entry.modified >= epoch {
                        std::time::UNIX_EPOCH.checked_add(duration)
                    } else {
                        std::time::UNIX_EPOCH.checked_sub(duration)
                    };
                    if let Some(modified) = modified {
                        output.set_times(fs::FileTimes::new().set_modified(modified))?;
                    }
                }
            }
            Ok(fs::read_dir(&root)?
                .map(|e| e.map(|e| e.path()))
                .collect::<std::io::Result<Vec<_>>>()?)
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&root);
        }
        result
    }
}
fn cache_size(p: &std::path::Path) -> Result<u64> {
    no_links(p)?;
    let meta = fs::metadata(p)?;
    if meta.is_file() {
        return Ok(meta.len());
    }
    let mut sum = 0u64;
    for e in fs::read_dir(p)? {
        sum = sum.saturating_add(cache_size(&e?.path())?);
    }
    Ok(sum)
}
pub fn formats(data: &IDataObject) -> Result<std::collections::BTreeMap<Format, Bytes>> {
    let mut formats = std::collections::BTreeMap::new();
    let mut total = 0;
    for kind in [
        Format::Text,
        Format::Html,
        Format::Rtf,
        Format::Png,
        Format::Dib,
        Format::DibV5,
    ] {
        if let Ok(m) = get(data, &format(format_id(kind), TYMED_HGLOBAL.0 as u32, -1)) {
            if m.0.tymed != TYMED_HGLOBAL.0 as u32 {
                continue;
            }
            let bytes = unsafe { read_global(m.0.u.hGlobal, MAX_CLIPBOARD - total)? };
            total += bytes.len();
            formats.insert(kind, Bytes(bytes));
        }
    }
    if !formats.contains_key(&Format::Text) {
        let text = if let Some(html) = formats.get(&Format::Html) {
            multimachine_rust::files::html_to_text(&html.0)
        } else if let Some(rtf) = formats.get(&Format::Rtf) {
            rtf_text(&rtf.0).unwrap_or_default()
        } else {
            String::new()
        };
        if !text.is_empty() {
            let b = wide(&text)
                .into_iter()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>();
            if total + b.len() <= MAX_CLIPBOARD {
                formats.insert(Format::Text, Bytes(b));
            }
        }
    }
    Ok(formats)
}
fn rtf_text(bytes: &[u8]) -> Result<String> {
    unsafe {
        use windows::Win32::{Foundation::*, System::LibraryLoader::*};
        struct Source<'a> {
            data: &'a [u8],
            offset: usize,
        }
        unsafe extern "system" fn feed(
            cookie: usize,
            p: *mut u8,
            n: i32,
            written: *mut i32,
        ) -> u32 {
            unsafe {
                let src = &mut *(cookie as *mut Source<'_>);
                if n < 0 || p.is_null() || written.is_null() {
                    return 1;
                }
                let n = (n as usize).min(src.data.len() - src.offset);
                std::ptr::copy_nonoverlapping(src.data.as_ptr().add(src.offset), p, n);
                src.offset += n;
                *written = n as i32;
                0
            }
        }
        static MODULE: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
        ensure!(
            MODULE
                .get_or_init(|| LoadLibraryW(w!("Msftedit.dll")).ok().map(|h| h.0 as usize))
                .is_some(),
            "RichEdit indisponible"
        );
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("RICHEDIT50W"),
            w!(""),
            WINDOW_STYLE(ES_MULTILINE as u32),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(module()),
            None,
        )?;
        let mut source = Source {
            data: bytes,
            offset: 0,
        };
        let mut stream = EDITSTREAM {
            dwCookie: &mut source as *mut _ as usize,
            pfnCallback: Some(feed),
            ..Default::default()
        };
        SendMessageW(
            hwnd,
            EM_STREAMIN,
            Some(WPARAM(SF_RTF as usize)),
            Some(LPARAM(&mut stream as *mut _ as isize)),
        );
        let value = text(hwnd);
        let _ = DestroyWindow(hwnd);
        ensure!(stream.dwError == 0, "Texte RTF non lisible");
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rich_text_fallback_uses_windows_parser() {
        let text = rtf_text(br"{\rtf1\ansi\b Bonjour\b0\par Monde}").unwrap();
        assert!(text.contains("Bonjour"));
        assert!(text.contains("Monde"));
        assert!(!text.contains("\\rtf"));
    }
}
