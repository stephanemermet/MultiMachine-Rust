#![allow(non_snake_case)]
//! COM objects expose stable descriptors from hover, and permit file reads only
//! after the target gesture commits. No CF_HDROP paths that could mask streams.
use super::platform::*;
use multimachine_rust::{model::*, network::Peer, virtuals};
use parking_lot::Mutex;
use std::{
    collections::BTreeMap,
    ffi::c_void,
    mem::ManuallyDrop,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::runtime::Handle;
use uuid::Uuid;
use windows::{
    Win32::{
        Foundation::*,
        System::{Com::*, DataExchange::*, Memory::*, Ole::*},
        UI::Shell::*,
    },
    core::*,
};
const FORMAT_ERROR: HRESULT = HRESULT(0x80040064u32 as i32);
const READ_ERROR: HRESULT = HRESULT(0x8003001Eu32 as i32);
const READ_ONLY: HRESULT = HRESULT(0x80030005u32 as i32);
fn err(hr: HRESULT) -> Error {
    Error::from_hresult(hr)
}
pub fn registered(s: &str) -> u16 {
    unsafe { RegisterClipboardFormatW(PCWSTR(wide(s).as_ptr())) as u16 }
}
pub fn format_id(f: Format) -> u16 {
    match f {
        Format::Text => 13,
        Format::Dib => 8,
        Format::DibV5 => 17,
        Format::Html => registered("HTML Format"),
        Format::Rtf => registered("Rich Text Format"),
        Format::Png => registered("PNG"),
    }
}
pub fn format(id: u16, tymed: u32, index: i32) -> FORMATETC {
    FORMATETC {
        cfFormat: id,
        dwAspect: DVASPECT_CONTENT.0,
        lindex: index,
        tymed,
        ..Default::default()
    }
}
pub fn global(bytes: &[u8]) -> Result<HGLOBAL> {
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1))?;
        let p = GlobalLock(h);
        if p.is_null() {
            let _ = GlobalFree(Some(h));
            return Err(Error::from_thread());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.cast(), bytes.len());
        let _ = GlobalUnlock(h);
        Ok(h)
    }
}
pub fn read_global(h: HGLOBAL, limit: usize) -> Result<Vec<u8>> {
    unsafe {
        let n = GlobalSize(h);
        if n > limit {
            return Err(err(E_OUTOFMEMORY));
        }
        let p = GlobalLock(h);
        if p.is_null() {
            return Err(err(E_FAIL));
        }
        let data = std::slice::from_raw_parts(p.cast::<u8>(), n).to_vec();
        let _ = GlobalUnlock(h);
        Ok(data)
    }
}
pub struct Medium(pub STGMEDIUM);
impl Drop for Medium {
    fn drop(&mut self) {
        unsafe { ReleaseStgMedium(&mut self.0) }
    }
}
pub fn get(data: &IDataObject, f: &FORMATETC) -> Result<Medium> {
    unsafe { data.GetData(f).map(Medium) }
}

#[implement(IDataObject, IDataObjectAsyncCapability)]
struct RemoteData {
    offer: Offer,
    peer: Arc<Peer>,
    runtime: Handle,
    formats: BTreeMap<u16, Vec<u8>>,
    allowed: Option<Arc<AtomicBool>>,
    async_mode: AtomicBool,
    in_operation: AtomicBool,
    notice: Notice,
}
pub fn remote(
    offer: Offer,
    peer: Arc<Peer>,
    runtime: Handle,
    plain: bool,
    allowed: Option<Arc<AtomicBool>>,
    notice: Notice,
) -> IDataObject {
    let mut formats = BTreeMap::new();
    for (kind, bytes) in &offer.formats {
        if !plain || !matches!(kind, Format::Html | Format::Rtf) {
            let mut native = bytes.0.clone();
            match kind {
                Format::Text => {
                    if native.len() % 2 != 0 {
                        native.push(0);
                    }
                    if !native.chunks_exact(2).any(|c| c == [0, 0]) {
                        native.extend_from_slice(&[0, 0]);
                    }
                }
                Format::Html | Format::Rtf if native.last() != Some(&0) => {
                    native.push(0);
                }
                _ => (),
            }
            formats.insert(format_id(*kind), native);
        }
    }
    if !offer.files.is_empty() {
        formats.insert(
            registered("FileGroupDescriptorW"),
            virtuals::descriptors(&offer.files),
        );
        formats.insert(
            registered("Preferred DropEffect"),
            1u32.to_le_bytes().to_vec(),
        );
    }
    RemoteData {
        offer,
        peer,
        runtime,
        formats,
        allowed,
        async_mode: AtomicBool::new(true),
        in_operation: AtomicBool::new(false),
        notice,
    }
    .into()
}
impl IDataObject_Impl for RemoteData_Impl {
    fn QueryGetData(&self, f: *const FORMATETC) -> HRESULT {
        unsafe {
            let Some(f) = f.as_ref() else {
                return E_POINTER;
            };
            if f.dwAspect != DVASPECT_CONTENT.0 {
                return FORMAT_ERROR;
            }
            if f.cfFormat == registered("FileContents")
                && !self.offer.files.is_empty()
                && f.tymed & TYMED_ISTREAM.0 as u32 != 0
                && (f.lindex == -1
                    || self
                        .offer
                        .files
                        .get(f.lindex as usize)
                        .is_some_and(|f| !f.directory))
            {
                return S_OK;
            }
            if self.formats.contains_key(&f.cfFormat) && f.tymed & TYMED_HGLOBAL.0 as u32 != 0 {
                S_OK
            } else {
                FORMAT_ERROR
            }
        }
    }
    fn GetData(&self, f: *const FORMATETC) -> Result<STGMEDIUM> {
        unsafe {
            self.QueryGetData(f).ok()?;
            let f = &*f;
            if f.cfFormat == registered("FileContents") {
                let file = self
                    .offer
                    .files
                    .get(f.lindex as usize)
                    .filter(|f| !f.directory)
                    .ok_or_else(|| err(FORMAT_ERROR))?
                    .clone();
                let stream: IStream = RemoteStream {
                    peer: self.peer.clone(),
                    runtime: self.runtime.clone(),
                    offer: self.offer.id,
                    file,
                    position: Mutex::new(0),
                    allowed: self.allowed.clone(),
                    notice: self.notice.clone(),
                }
                .into();
                Ok(STGMEDIUM {
                    tymed: TYMED_ISTREAM.0 as u32,
                    u: STGMEDIUM_0 {
                        pstm: ManuallyDrop::new(Some(stream)),
                    },
                    pUnkForRelease: ManuallyDrop::new(None),
                })
            } else {
                Ok(STGMEDIUM {
                    tymed: TYMED_HGLOBAL.0 as u32,
                    u: STGMEDIUM_0 {
                        hGlobal: global(&self.formats[&f.cfFormat])?,
                    },
                    pUnkForRelease: ManuallyDrop::new(None),
                })
            }
        }
    }
    fn GetDataHere(&self, _: *const FORMATETC, _: *mut STGMEDIUM) -> Result<()> {
        Err(err(E_NOTIMPL))
    }
    fn GetCanonicalFormatEtc(&self, input: *const FORMATETC, output: *mut FORMATETC) -> HRESULT {
        unsafe {
            if input.is_null() || output.is_null() {
                return E_POINTER;
            }
            *output = *input;
            (*output).ptd = std::ptr::null_mut();
            HRESULT(0x40130)
        }
    }
    fn SetData(&self, _: *const FORMATETC, m: *const STGMEDIUM, release: BOOL) -> Result<()> {
        if release.as_bool() && !m.is_null() {
            unsafe { ReleaseStgMedium(m as *mut _) }
        }
        Ok(())
    }
    fn EnumFormatEtc(&self, direction: u32) -> Result<IEnumFORMATETC> {
        if direction != DATADIR_GET.0 as u32 {
            return Err(err(E_NOTIMPL));
        }
        let mut values: Vec<_> = self
            .formats
            .keys()
            .map(|id| format(*id, TYMED_HGLOBAL.0 as u32, -1))
            .collect();
        if !self.offer.files.is_empty() {
            values.push(format(
                registered("FileContents"),
                TYMED_ISTREAM.0 as u32,
                -1,
            ));
        }
        Ok(FormatEnum {
            values,
            index: Mutex::new(0),
        }
        .into())
    }
    fn DAdvise(&self, _: *const FORMATETC, _: u32, _: Ref<IAdviseSink>) -> Result<u32> {
        Err(err(OLE_E_ADVISENOTSUPPORTED))
    }
    fn DUnadvise(&self, _: u32) -> Result<()> {
        Ok(())
    }
    fn EnumDAdvise(&self) -> Result<IEnumSTATDATA> {
        Err(err(OLE_E_ADVISENOTSUPPORTED))
    }
}
impl IDataObjectAsyncCapability_Impl for RemoteData_Impl {
    fn SetAsyncMode(&self, v: BOOL) -> Result<()> {
        self.async_mode.store(v.as_bool(), Ordering::Relaxed);
        Ok(())
    }
    fn GetAsyncMode(&self) -> Result<BOOL> {
        Ok(self.async_mode.load(Ordering::Relaxed).into())
    }
    fn StartOperation(&self, _: Ref<IBindCtx>) -> Result<()> {
        self.in_operation.store(true, Ordering::Relaxed);
        (self.notice)("Collage des fichiers en cours…".into());
        Ok(())
    }
    fn InOperation(&self) -> Result<BOOL> {
        Ok(self.in_operation.load(Ordering::Relaxed).into())
    }
    fn EndOperation(&self, result: HRESULT, _: Ref<IBindCtx>, _: u32) -> Result<()> {
        self.in_operation.store(false, Ordering::Relaxed);
        (self.notice)(
            if result.is_ok() {
                "Collage terminé."
            } else {
                "Collage interrompu ou refusé."
            }
            .into(),
        );
        Ok(())
    }
}
#[implement(IEnumFORMATETC)]
struct FormatEnum {
    values: Vec<FORMATETC>,
    index: Mutex<usize>,
}
impl IEnumFORMATETC_Impl for FormatEnum_Impl {
    fn Next(&self, count: u32, out: *mut FORMATETC, fetched: *mut u32) -> HRESULT {
        unsafe {
            if out.is_null() || (fetched.is_null() && count != 1) {
                return E_POINTER;
            }
            let mut i = self.index.lock();
            let n = (count as usize).min(self.values.len() - *i);
            for j in 0..n {
                *out.add(j) = self.values[*i + j];
            }
            *i += n;
            if !fetched.is_null() {
                *fetched = n as u32
            }
            if n == count as usize { S_OK } else { S_FALSE }
        }
    }
    fn Skip(&self, count: u32) -> Result<()> {
        let mut i = self.index.lock();
        let n = (count as usize).min(self.values.len() - *i);
        *i += n;
        if n == count as usize {
            Ok(())
        } else {
            Err(err(S_FALSE))
        }
    }
    fn Reset(&self) -> Result<()> {
        *self.index.lock() = 0;
        Ok(())
    }
    fn Clone(&self) -> Result<IEnumFORMATETC> {
        Ok(FormatEnum {
            values: self.values.clone(),
            index: Mutex::new(*self.index.lock()),
        }
        .into())
    }
}
#[implement(IStream)]
struct RemoteStream {
    peer: Arc<Peer>,
    runtime: Handle,
    offer: Uuid,
    file: FileEntry,
    position: Mutex<u64>,
    allowed: Option<Arc<AtomicBool>>,
    notice: Notice,
}
impl RemoteStream {
    fn read(&self, buffer: &mut [u8]) -> Result<usize> {
        if self
            .allowed
            .as_ref()
            .is_some_and(|a| !a.load(Ordering::Acquire))
        {
            return Err(err(READ_ERROR));
        }
        let mut pos = self.position.lock();
        let mut total = 0;
        while total < buffer.len() && *pos < self.file.length {
            let want = (buffer.len() - total)
                .min(CHUNK)
                .min((self.file.length - *pos) as usize);
            let bytes = self
                .runtime
                .block_on(self.peer.read(self.offer, self.file.id, *pos, want as u32))
                .map_err(|_| {
                    (self.notice)("Transfert interrompu : source inaccessible.".into());
                    err(READ_ERROR)
                })?;
            if bytes.is_empty() || bytes.len() > want {
                return Err(err(READ_ERROR));
            }
            buffer[total..total + bytes.len()].copy_from_slice(&bytes);
            total += bytes.len();
            *pos += bytes.len() as u64;
        }
        Ok(total)
    }
}
impl ISequentialStream_Impl for RemoteStream_Impl {
    fn Read(&self, p: *mut c_void, n: u32, read: *mut u32) -> HRESULT {
        unsafe {
            if !read.is_null() {
                *read = 0
            }
            if p.is_null() && n != 0 {
                return E_POINTER;
            }
            if n == 0 {
                return S_OK;
            }
            match self.read(std::slice::from_raw_parts_mut(p.cast(), n as usize)) {
                Ok(got) => {
                    if !read.is_null() {
                        *read = got as u32
                    }
                    if got == n as usize { S_OK } else { S_FALSE }
                }
                Err(e) => e.code(),
            }
        }
    }
    fn Write(&self, _: *const c_void, _: u32, put: *mut u32) -> HRESULT {
        unsafe {
            if !put.is_null() {
                *put = 0
            }
        }
        READ_ONLY
    }
}
impl IStream_Impl for RemoteStream_Impl {
    fn Seek(&self, offset: i64, origin: STREAM_SEEK, out: *mut u64) -> Result<()> {
        let mut p = self.position.lock();
        let base = match origin {
            STREAM_SEEK_SET => 0,
            STREAM_SEEK_CUR => *p,
            STREAM_SEEK_END => self.file.length,
            _ => return Err(err(STG_E_INVALIDFUNCTION)),
        };
        let value = (base as i128) + (offset as i128);
        if value < 0 || value > u64::MAX as i128 {
            return Err(err(STG_E_INVALIDFUNCTION));
        }
        *p = value as u64;
        unsafe {
            if !out.is_null() {
                *out = *p
            }
        }
        Ok(())
    }
    fn SetSize(&self, _: u64) -> Result<()> {
        Err(err(READ_ONLY))
    }
    fn CopyTo(
        &self,
        target: Ref<IStream>,
        count: u64,
        read: *mut u64,
        written: *mut u64,
    ) -> Result<()> {
        unsafe {
            if !read.is_null() {
                *read = 0
            }
            if !written.is_null() {
                *written = 0
            }
            let target = target.as_ref().ok_or_else(|| err(E_POINTER))?;
            let mut buffer = vec![0; CHUNK];
            let mut total = 0;
            while total < count {
                let wanted = (count - total).min(CHUNK as u64) as usize;
                let n = self.read(&mut buffer[..wanted])?;
                if n == 0 {
                    break;
                }
                if !read.is_null() {
                    *read += n as u64
                }
                let mut put = 0;
                target
                    .Write(buffer.as_ptr().cast(), n as u32, Some(&mut put))
                    .ok()?;
                if !written.is_null() {
                    *written += put as u64
                }
                if put as usize != n {
                    return Err(err(STG_E_MEDIUMFULL));
                }
                total += n as u64;
            }
            Ok(())
        }
    }
    fn Commit(&self, _: &STGC) -> Result<()> {
        Ok(())
    }
    fn Revert(&self) -> Result<()> {
        Err(err(READ_ONLY))
    }
    fn LockRegion(&self, _: u64, _: u64, _: &LOCKTYPE) -> Result<()> {
        Err(err(READ_ONLY))
    }
    fn UnlockRegion(&self, _: u64, _: u64, _: u32) -> Result<()> {
        Err(err(READ_ONLY))
    }
    fn Stat(&self, out: *mut STATSTG, flags: &STATFLAG) -> Result<()> {
        unsafe {
            if out.is_null() {
                return Err(err(E_POINTER));
            }
            *out = STATSTG {
                r#type: STGTY_STREAM.0 as u32,
                cbSize: self.file.length,
                ..Default::default()
            };
            if flags.0 & STATFLAG_NONAME.0 == 0 {
                let name = wide(&self.file.name);
                let p = CoTaskMemAlloc(name.len() * 2).cast::<u16>();
                if p.is_null() {
                    return Err(err(E_OUTOFMEMORY));
                }
                std::ptr::copy_nonoverlapping(name.as_ptr(), p, name.len());
                (*out).pwcsName = PWSTR(p);
            }
            Ok(())
        }
    }
    fn Clone(&self) -> Result<IStream> {
        Ok(RemoteStream {
            peer: self.peer.clone(),
            runtime: self.runtime.clone(),
            offer: self.offer,
            file: self.file.clone(),
            position: Mutex::new(*self.position.lock()),
            allowed: self.allowed.clone(),
            notice: self.notice.clone(),
        }
        .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use multimachine_rust::{
        files::Catalog,
        network::{self, Identity, ServerOptions, Trust},
    };
    use std::{sync::atomic::AtomicU64, time::Duration};
    use tokio_util::sync::CancellationToken;
    #[test]
    fn com_descriptors_stream_gate_seek_clone_and_copy_only() {
        unsafe {
            OleInitialize(None).unwrap();
        }
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let identity = Identity::create().unwrap();
        let listen = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listen.local_addr().unwrap().port();
        drop(listen);
        let trust = Trust {
            host: "127.0.0.1".into(),
            port,
            fingerprint: identity.fingerprint(),
            secret: identity.secret.clone(),
        };
        let stop = CancellationToken::new();
        let server_stop = stop.clone();
        let root = std::env::temp_dir().join(format!("mmr-native-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("résumé.txt");
        let bytes = vec![17; CHUNK + 19];
        std::fs::write(&path, &bytes).unwrap();
        let catalog = Arc::new(Catalog::default());
        let mut offer = Offer::empty(1, Uuid::new_v4());
        offer.files = catalog
            .register(offer.id, std::slice::from_ref(&path))
            .unwrap();
        let task = runtime.spawn(network::serve(
            identity,
            ServerOptions {
                port,
                name: "native-test".into(),
                hybrid: false,
                allow_pair: Arc::new(AtomicU64::new(0)),
                approve: Arc::new(|_| Box::pin(async { false })),
                paired: Arc::new(|| {}),
            },
            catalog,
            Arc::new(|_, _| {}),
            Arc::new(|_| {}),
            server_stop,
        ));
        let peer = runtime.block_on(async {
            let mut connection = None;
            for _ in 0..30 {
                if let Ok(c) = network::connect(
                    &trust,
                    false,
                    Arc::new(Catalog::default()),
                    Arc::new(|_, _| {}),
                    stop.child_token(),
                )
                .await
                {
                    connection = Some(c);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
            connection.unwrap().start()
        });
        let allowed = Arc::new(AtomicBool::new(false));
        let data = remote(
            offer.clone(),
            peer.clone(),
            runtime.handle().clone(),
            false,
            Some(allowed.clone()),
            Arc::new(|_| {}),
        );
        unsafe {
            assert!(
                data.QueryGetData(&format(15, TYMED_HGLOBAL.0 as u32, -1))
                    .is_err()
            );
            let descriptor = get(
                &data,
                &format(
                    registered("FileGroupDescriptorW"),
                    TYMED_HGLOBAL.0 as u32,
                    -1,
                ),
            )
            .unwrap();
            let parsed =
                virtuals::parse(&read_global(descriptor.0.u.hGlobal, 4096).unwrap()).unwrap();
            assert_eq!(parsed[0].name, "résumé.txt");
            let enum_formats = data.EnumFormatEtc(DATADIR_GET.0 as u32).unwrap();
            let mut list = [FORMATETC::default(); 8];
            let mut fetched = 0;
            assert_eq!(enum_formats.Next(&mut list, Some(&mut fetched)), S_FALSE);
            assert_eq!(fetched, 3);
            assert!(list[..fetched as usize].iter().all(|f| f.cfFormat != 15));
            let cap: IDataObjectAsyncCapability = data.cast().unwrap();
            assert!(cap.GetAsyncMode().unwrap().as_bool());
            cap.StartOperation(None).unwrap();
            assert!(cap.InOperation().unwrap().as_bool());
            cap.EndOperation(S_OK, None, 1).unwrap();
            assert!(!cap.InOperation().unwrap().as_bool());
            let medium = get(
                &data,
                &format(registered("FileContents"), TYMED_ISTREAM.0 as u32, 0),
            )
            .unwrap();
            let stream = medium.0.u.pstm.as_ref().unwrap();
            let mut output = vec![0; bytes.len()];
            let mut n = 99;
            assert!(
                stream
                    .Read(output.as_mut_ptr().cast(), 1, Some(&mut n))
                    .is_err()
            );
            assert_eq!(n, 0);
            allowed.store(true, Ordering::Release);
            stream
                .Read(
                    output.as_mut_ptr().cast(),
                    output.len() as u32,
                    Some(&mut n),
                )
                .ok()
                .unwrap();
            assert_eq!(n as usize, bytes.len());
            assert_eq!(output, bytes);
            let mut position = 0;
            stream
                .Seek(-19, STREAM_SEEK_END, Some(&mut position))
                .unwrap();
            assert_eq!(position, CHUNK as u64);
            let copy = stream.Clone().unwrap();
            copy.Read(output.as_mut_ptr().cast(), 64, Some(&mut n))
                .ok()
                .unwrap();
            assert_eq!(n, 19);
            stream
                .Seek(0, STREAM_SEEK_CUR, Some(&mut position))
                .unwrap();
            assert_eq!(position, CHUNK as u64);
            assert!(stream.Write(output.as_ptr().cast(), 1, None).is_err());
            let mut stat = STATSTG::default();
            stream.Stat(&mut stat, STATFLAG_NONAME).unwrap();
            assert_eq!(stat.cbSize, bytes.len() as u64);
            assert!(stat.pwcsName.is_null());
        }
        drop(data);
        peer.close("test complete");
        stop.cancel();
        runtime.block_on(task).unwrap().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::remove_dir_all(root).unwrap();
        unsafe {
            OleUninitialize();
        }
    }
    #[test]
    fn global_memory_roundtrip() {
        let bytes = [1, 2, 3, 4, 5, 0];
        let h = global(&bytes).unwrap();
        assert_eq!(&read_global(h, 1024).unwrap()[..bytes.len()], &bytes);
        assert!(read_global(h, 1).is_err());
        unsafe {
            let _ = GlobalFree(Some(h));
        }
    }
}
