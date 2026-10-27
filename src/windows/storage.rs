use super::platform::*;
use anyhow::{Context, Result, ensure};
use multimachine_rust::{
    files::no_links,
    model::*,
    network::{Identity, Trust},
};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::OnceLock,
};
use windows::{
    Win32::{
        Foundation::*, Security::Cryptography::*, Storage::FileSystem::*, System::Registry::*,
    },
    core::*,
};

pub fn root() -> PathBuf {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("MultiMachineRust")
    })
    .clone()
}
pub fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("Dossier absent")?;
    fs::create_dir_all(parent)?;
    no_links(parent)?;
    let temp = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    let from = wide(&temp.to_string_lossy());
    let to = wide(&path.to_string_lossy());
    let result = unsafe {
        MoveFileExW(
            PCWSTR(from.as_ptr()),
            PCWSTR(to.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result?;
    Ok(())
}
pub fn read<T: DeserializeOwned>(name: &str, secret: bool) -> Result<T> {
    let p = root().join(name);
    no_links(&p)?;
    ensure!(
        fs::metadata(&p)?.len() <= 512 * 1024,
        "Configuration trop grande"
    );
    let bytes = fs::read(p)?;
    let bytes = if secret {
        protect(&bytes, false)?
    } else {
        bytes
    };
    Ok(serde_json::from_slice(&bytes)?)
}
pub fn save<T: Serialize>(name: &str, value: &T, secret: bool) -> Result<()> {
    let b = serde_json::to_vec_pretty(value)?;
    atomic(
        &root().join(name),
        &if secret { protect(&b, true)? } else { b },
    )
}
fn protect(bytes: &[u8], encrypt: bool) -> Result<Vec<u8>> {
    unsafe {
        let input = CRYPT_INTEGER_BLOB {
            cbData: bytes.len() as u32,
            pbData: bytes.as_ptr() as *mut _,
        };
        let mut output = CRYPT_INTEGER_BLOB::default();
        let entropy = b"MultiMachine-Rust/private/v1";
        let context = CRYPT_INTEGER_BLOB {
            cbData: entropy.len() as u32,
            pbData: entropy.as_ptr() as *mut _,
        };
        if encrypt {
            CryptProtectData(
                &input,
                w!("MultiMachine Rust"),
                Some(&context),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )?
        } else {
            CryptUnprotectData(
                &input,
                None,
                Some(&context),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )?
        }
        let result = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        Ok(result)
    }
}
pub fn settings() -> Result<Settings> {
    if !root().join("settings.json").exists() {
        return Ok(Settings::default());
    }
    let s: Settings = read("settings.json", false)?;
    s.validate()?;
    Ok(s)
}
pub fn identity() -> Result<Identity> {
    let p = root().join("identity.dpapi");
    if p.exists() {
        read("identity.dpapi", true)
    } else {
        let id = Identity::create()?;
        save("identity.dpapi", &id, true)?;
        Ok(id)
    }
}
pub fn trust() -> Result<Option<Trust>> {
    if root().join("trust.dpapi").exists() {
        read("trust.dpapi", true).map(Some)
    } else {
        Ok(None)
    }
}
pub fn autostart(enabled: bool) -> Result<()> {
    unsafe {
        let mut key = HKEY::default();
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
        .ok()?;
        let result = if enabled {
            let exe = std::env::current_exe()?;
            let command = wide(&format!("\"{}\" --startup", exe.display()));
            let bytes =
                std::slice::from_raw_parts(command.as_ptr().cast::<u8>(), command.len() * 2);
            RegSetValueExW(key, w!("MultiMachineRust"), None, REG_SZ, Some(bytes))
        } else {
            RegDeleteValueW(key, w!("MultiMachineRust"))
        };
        let _ = RegCloseKey(key);
        if result != ERROR_FILE_NOT_FOUND {
            result.ok()?
        }
        Ok(())
    }
}
pub fn log(event: &str) {
    // Call sites log operation categories and counts only, never clipboard text, paths or keys.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let Ok(_guard) = LOCK.lock() else { return };
    let root = root();
    let _ = fs::create_dir_all(&root);
    let path = root.join("diagnostic.log");
    if fs::metadata(&path).is_ok_and(|m| m.len() > 2 * 1024 * 1024) {
        let _ = fs::rename(&path, root.join("diagnostic.previous.log"));
    }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let _ = writeln!(f, "{time} rust/{} {event}", env!("CARGO_PKG_VERSION"));
    }
}
