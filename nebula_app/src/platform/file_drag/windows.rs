//! OLE virtual files let Explorer choose the destination and handle copy conflicts.

mod stream;
#[cfg(test)]
mod tests;

use super::*;
use ::windows::Win32::Foundation::*;
use ::windows::Win32::System::Com::*;
use ::windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use ::windows::Win32::System::Memory::*;
use ::windows::Win32::System::Ole::*;
use ::windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use ::windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use ::windows::Win32::UI::Shell::{
    FD_ATTRIBUTES, FD_FILESIZE, FILEDESCRIPTORW, SHCreateStdEnumFmtEtc,
};
use ::windows::core::{BOOL, HRESULT, Ref, implement, w};
use std::mem::{ManuallyDrop, size_of};
use std::sync::{Mutex, OnceLock};

#[cfg(test)]
static DRAG_PROBE_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn error(code: HRESULT) -> ::windows::core::Error {
    code.into()
}

#[implement(IDropSource)]
struct DropSource {
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        #[cfg(test)]
        {
            DRAG_PROBE_READY.store(true, std::sync::atomic::Ordering::Release);
        }
        if escape.as_bool() || (self.cancelled)() {
            DRAGDROP_S_CANCEL
        } else if keys.0 & MK_LBUTTON.0 == 0
            || unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } >= 0
        {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

#[implement(IDataObject)]
struct DataObject {
    files: OnceLock<Result<Vec<DragFile>, String>>,
    describe: Manifest,
    materialize: Materialize,
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
    failure: Arc<Mutex<Option<String>>>,
    descriptors_format: u16,
    contents_format: u16,
}

impl DataObject {
    fn formats(&self) -> [FORMATETC; 2] {
        [
            FORMATETC {
                cfFormat: self.descriptors_format,
                dwAspect: DVASPECT_CONTENT.0,
                lindex: -1,
                tymed: TYMED_HGLOBAL.0 as u32,
                ..Default::default()
            },
            FORMATETC {
                cfFormat: self.contents_format,
                dwAspect: DVASPECT_CONTENT.0,
                lindex: -1,
                tymed: TYMED_ISTREAM.0 as u32,
                ..Default::default()
            },
        ]
    }

    fn manifest(&self) -> ::windows::core::Result<&[DragFile]> {
        let result = self.files.get_or_init(|| {
            (self.describe)()
                .and_then(|files| {
                    super::validate_manifest(&files)?;
                    Ok(files)
                })
                .map_err(|failure| failure.to_string())
        });
        match result {
            Ok(files) => Ok(files),
            Err(message) => {
                *self.failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(message.clone());
                Err(error(E_FAIL))
            },
        }
    }

    fn query(&self, format: &FORMATETC) -> HRESULT {
        if format.dwAspect != DVASPECT_CONTENT.0 {
            return DV_E_DVASPECT;
        }
        if !format.ptd.is_null() {
            return DV_E_DVTARGETDEVICE;
        }
        if format.cfFormat == self.descriptors_format && format.lindex == -1 {
            return if format.tymed & TYMED_HGLOBAL.0 as u32 != 0 { S_OK } else { DV_E_TYMED };
        }
        if format.cfFormat == self.contents_format {
            // Querying an advertised format must stay cheap and accept lindex -1.
            // The concrete stream index is validated by GetData against the manifest.
            if format.lindex < -1 || format.lindex >= 100_000 {
                return DV_E_LINDEX;
            }
            if format.lindex >= 0
                && self.files.get().is_some_and(|files| {
                    files.as_ref().is_ok_and(|files| {
                        files.get(format.lindex as usize).is_none_or(|file| file.directory)
                    })
                })
            {
                return DV_E_LINDEX;
            }
            return if format.tymed & TYMED_ISTREAM.0 as u32 != 0 { S_OK } else { DV_E_TYMED };
        }
        DV_E_FORMATETC
    }

    fn descriptors(&self) -> ::windows::core::Result<STGMEDIUM> {
        let files = self.manifest()?;
        let length = size_of::<u32>() + files.len() * size_of::<FILEDESCRIPTORW>();
        unsafe {
            let memory = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, length)?;
            let data = GlobalLock(memory).cast::<u8>();
            if data.is_null() {
                let _ = GlobalFree(Some(memory));
                return Err(error(E_OUTOFMEMORY));
            }
            data.cast::<u32>().write_unaligned(files.len() as u32);
            for (index, file) in files.iter().enumerate() {
                let mut descriptor = FILEDESCRIPTORW::default();
                descriptor.dwFlags = FD_ATTRIBUTES.0 as u32;
                descriptor.dwFileAttributes = if file.directory { 0x10 } else { 0x80 };
                if !file.directory && file.size > 0 {
                    descriptor.dwFlags |= FD_FILESIZE.0 as u32;
                    descriptor.nFileSizeHigh = (file.size >> 32) as u32;
                    descriptor.nFileSizeLow = file.size as u32;
                }
                let name: Vec<_> = file
                    .relative_path
                    .to_string_lossy()
                    .replace('/', "\\")
                    .encode_utf16()
                    .collect();
                if name.len() >= 260 {
                    let _ = GlobalUnlock(memory);
                    let _ = GlobalFree(Some(memory));
                    return Err(error(E_INVALIDARG));
                }
                let mut file_name = [0u16; 260];
                file_name[..name.len()].copy_from_slice(&name);
                descriptor.cFileName = file_name;
                data.add(size_of::<u32>() + index * size_of::<FILEDESCRIPTORW>())
                    .cast::<FILEDESCRIPTORW>()
                    .write_unaligned(descriptor);
            }
            let _ = GlobalUnlock(memory);
            Ok(STGMEDIUM {
                tymed: TYMED_HGLOBAL.0 as u32,
                u: STGMEDIUM_0 { hGlobal: memory },
                ..Default::default()
            })
        }
    }
}

impl IDataObject_Impl for DataObject_Impl {
    fn GetData(&self, requested: *const FORMATETC) -> ::windows::core::Result<STGMEDIUM> {
        let requested = unsafe { requested.as_ref() }.ok_or_else(|| error(E_POINTER))?;
        self.query(requested).ok()?;
        if (self.cancelled)() {
            return Err(error(E_ABORT));
        }
        if requested.cfFormat == self.descriptors_format {
            return self.descriptors();
        }
        if requested.lindex < 0
            || self.manifest()?.get(requested.lindex as usize).is_none_or(|file| file.directory)
        {
            return Err(error(DV_E_LINDEX));
        }
        let materialized = (self.materialize)(requested.lindex as usize).and_then(|file| {
            stream::FileStream::open(file).map(|stream| -> IStream { stream.into() })
        });
        match materialized {
            Ok(stream) => Ok(STGMEDIUM {
                tymed: TYMED_ISTREAM.0 as u32,
                u: STGMEDIUM_0 { pstm: ManuallyDrop::new(Some(stream)) },
                ..Default::default()
            }),
            Err(failure) => {
                *self.failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(failure.to_string());
                Err(error(E_FAIL))
            },
        }
    }

    fn QueryGetData(&self, requested: *const FORMATETC) -> HRESULT {
        unsafe { requested.as_ref() }.map(|f| self.query(f)).unwrap_or(E_POINTER)
    }

    fn GetDataHere(&self, _: *const FORMATETC, _: *mut STGMEDIUM) -> ::windows::core::Result<()> {
        Err(error(E_NOTIMPL))
    }
    fn GetCanonicalFormatEtc(&self, _: *const FORMATETC, out: *mut FORMATETC) -> HRESULT {
        if let Some(out) = unsafe { out.as_mut() } {
            out.ptd = std::ptr::null_mut();
        }
        DATA_S_SAMEFORMATETC
    }
    fn SetData(
        &self,
        _: *const FORMATETC,
        _: *const STGMEDIUM,
        _: BOOL,
    ) -> ::windows::core::Result<()> {
        Err(error(E_NOTIMPL))
    }
    fn EnumFormatEtc(&self, direction: u32) -> ::windows::core::Result<IEnumFORMATETC> {
        if direction != DATADIR_GET.0 as u32 {
            return Err(error(E_NOTIMPL));
        }
        unsafe { SHCreateStdEnumFmtEtc(&self.formats()) }
    }
    fn DAdvise(
        &self,
        _: *const FORMATETC,
        _: u32,
        _: Ref<'_, IAdviseSink>,
    ) -> ::windows::core::Result<u32> {
        Err(error(OLE_E_ADVISENOTSUPPORTED))
    }
    fn DUnadvise(&self, _: u32) -> ::windows::core::Result<()> {
        Err(error(OLE_E_ADVISENOTSUPPORTED))
    }
    fn EnumDAdvise(&self) -> ::windows::core::Result<IEnumSTATDATA> {
        Err(error(OLE_E_ADVISENOTSUPPORTED))
    }
}

struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { OleUninitialize() };
    }
}

struct InputAttachment {
    source: u32,
    worker: u32,
}

impl Drop for InputAttachment {
    fn drop(&mut self) {
        if self.source != self.worker {
            unsafe {
                windows_sys::Win32::System::Threading::AttachThreadInput(
                    self.worker,
                    self.source,
                    0,
                );
            }
        }
    }
}

pub(super) fn run(
    origin: DragOrigin,
    files: Manifest,
    materialize: Materialize,
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
) -> io::Result<DragOutcome> {
    if cancelled() || unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } >= 0 {
        return Ok(DragOutcome::Cancelled);
    }
    unsafe { OleInitialize(None) }.map_err(io::Error::other)?;
    let _apartment = Apartment;
    let worker = unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() };
    if worker != origin.thread
        && unsafe {
            windows_sys::Win32::System::Threading::AttachThreadInput(worker, origin.thread, 1)
        } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let _input = InputAttachment { source: origin.thread, worker };
    // OLE initialization can outlive a very short gesture. Do not enter its
    // modal loop with a button-up event that was delivered before it started.
    if cancelled() || unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } >= 0 {
        return Ok(DragOutcome::Cancelled);
    }
    let descriptors_format = unsafe { RegisterClipboardFormatW(w!("FileGroupDescriptorW")) } as u16;
    let contents_format = unsafe { RegisterClipboardFormatW(w!("FileContents")) } as u16;
    if descriptors_format == 0 || contents_format == 0 {
        return Err(io::Error::last_os_error());
    }
    let failure = Arc::new(Mutex::new(None));
    let source: IDropSource = DropSource { cancelled: cancelled.clone() }.into();
    let data: IDataObject = DataObject {
        files: OnceLock::new(),
        describe: files,
        materialize,
        cancelled: cancelled.clone(),
        failure: failure.clone(),
        descriptors_format,
        contents_format,
    }
    .into();
    let mut effect = DROPEFFECT_NONE;
    let result = unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect) };
    drop(data);
    drop(source);
    if cancelled() {
        return Ok(DragOutcome::Cancelled);
    }
    if let Some(failure) = failure.lock().unwrap_or_else(|e| e.into_inner()).take() {
        return Err(io::Error::other(failure));
    }
    if result == DRAGDROP_S_CANCEL {
        return Ok(DragOutcome::Cancelled);
    }
    result.ok().map_err(io::Error::other)?;
    Ok(if effect.0 & DROPEFFECT_COPY.0 != 0 { DragOutcome::Copied } else { DragOutcome::Cancelled })
}
