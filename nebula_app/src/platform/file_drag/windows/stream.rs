//! Each shell stream owns an independent cursor and a lease on its temporary file.

use super::*;
use std::ffi::c_void;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

#[implement(IStream)]
pub(super) struct FileStream {
    file: Mutex<File>,
    path: PathBuf,
    lease: Arc<dyn Send + Sync>,
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl FileStream {
    pub(super) fn open(source: MaterializedFile) -> io::Result<Self> {
        Ok(Self {
            file: Mutex::new(File::open(&source.path)?),
            path: source.path,
            lease: source.lease,
            cancelled: source.cancelled,
        })
    }
}

impl ISequentialStream_Impl for FileStream_Impl {
    fn Read(&self, target: *mut c_void, count: u32, read: *mut u32) -> HRESULT {
        if !read.is_null() {
            unsafe { read.write(0) };
        }
        if (self.cancelled)() {
            return E_ABORT;
        }
        if count == 0 {
            return S_OK;
        }
        if target.is_null() {
            return E_POINTER;
        }
        let target = unsafe { std::slice::from_raw_parts_mut(target.cast::<u8>(), count as usize) };
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let mut total = 0;
        while total < target.len() {
            match file.read(&mut target[total..]) {
                Ok(0) => break,
                Ok(bytes) => total += bytes,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return STG_E_READFAULT,
            }
        }
        if !read.is_null() {
            unsafe { read.write(total as u32) };
        }
        if total < target.len() { S_FALSE } else { S_OK }
    }
    fn Write(&self, _: *const c_void, _: u32, written: *mut u32) -> HRESULT {
        if !written.is_null() {
            unsafe { written.write(0) };
        }
        STG_E_ACCESSDENIED
    }
}

impl IStream_Impl for FileStream_Impl {
    fn Seek(
        &self,
        offset: i64,
        origin: STREAM_SEEK,
        position: *mut u64,
    ) -> ::windows::core::Result<()> {
        let from = if origin == STREAM_SEEK_SET && offset >= 0 {
            SeekFrom::Start(offset as u64)
        } else if origin == STREAM_SEEK_CUR {
            SeekFrom::Current(offset)
        } else if origin == STREAM_SEEK_END {
            SeekFrom::End(offset)
        } else {
            return Err(error(STG_E_INVALIDFUNCTION));
        };
        let value = self
            .file
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .seek(from)
            .map_err(|_| error(STG_E_SEEKERROR))?;
        if !position.is_null() {
            unsafe { position.write(value) };
        }
        Ok(())
    }
    fn SetSize(&self, _: u64) -> ::windows::core::Result<()> {
        Err(error(STG_E_ACCESSDENIED))
    }
    fn CopyTo(
        &self,
        target: Ref<'_, IStream>,
        count: u64,
        read: *mut u64,
        written: *mut u64,
    ) -> ::windows::core::Result<()> {
        let target = target.as_ref().ok_or_else(|| error(E_POINTER))?;
        let mut total = 0u64;
        let mut buffer = [0u8; 65536];
        if !read.is_null() {
            unsafe { read.write(0) };
        }
        if !written.is_null() {
            unsafe { written.write(0) };
        }
        while total < count {
            let length = buffer.len().min((count - total).min(usize::MAX as u64) as usize);
            let mut got = 0;
            self.Read(buffer.as_mut_ptr().cast(), length as u32, &mut got).ok()?;
            if got == 0 {
                break;
            }
            let mut sent = 0;
            unsafe { target.Write(buffer.as_ptr().cast(), got, Some(&mut sent)) }.ok()?;
            if sent != got {
                return Err(error(STG_E_WRITEFAULT));
            }
            total += u64::from(sent);
            if !read.is_null() {
                unsafe { read.write(total) };
            }
            if !written.is_null() {
                unsafe { written.write(total) };
            }
        }
        Ok(())
    }
    fn Commit(&self, _: &STGC) -> ::windows::core::Result<()> {
        Ok(())
    }
    fn Revert(&self) -> ::windows::core::Result<()> {
        Err(error(STG_E_INVALIDFUNCTION))
    }
    fn LockRegion(&self, _: u64, _: u64, _: &LOCKTYPE) -> ::windows::core::Result<()> {
        Err(error(STG_E_INVALIDFUNCTION))
    }
    fn UnlockRegion(&self, _: u64, _: u64, _: u32) -> ::windows::core::Result<()> {
        Err(error(STG_E_INVALIDFUNCTION))
    }
    fn Stat(&self, stat: *mut STATSTG, _: &STATFLAG) -> ::windows::core::Result<()> {
        let stat = unsafe { stat.as_mut() }.ok_or_else(|| error(E_POINTER))?;
        let size = self
            .file
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .metadata()
            .map_err(|_| error(STG_E_READFAULT))?
            .len();
        *stat = STATSTG {
            r#type: STGTY_STREAM.0 as u32,
            cbSize: size,
            grfMode: STGM_READ,
            ..Default::default()
        };
        Ok(())
    }
    fn Clone(&self) -> ::windows::core::Result<IStream> {
        let position = self
            .file
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stream_position()
            .map_err(|_| error(STG_E_SEEKERROR))?;
        let mut file = File::open(&self.path).map_err(|_| error(STG_E_READFAULT))?;
        file.seek(SeekFrom::Start(position)).map_err(|_| error(STG_E_SEEKERROR))?;
        Ok(FileStream {
            file: Mutex::new(file),
            path: self.path.clone(),
            lease: self.lease.clone(),
            cancelled: self.cancelled.clone(),
        }
        .into())
    }
}
