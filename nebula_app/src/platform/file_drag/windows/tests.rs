use super::*;
mod ole_probe;
use ::windows::Win32::System::Ole::ReleaseStgMedium;

fn object(name: &str, contents: &[u8]) -> (IDataObject, Arc<tempfile::TempDir>, u16, u16) {
    let directory = Arc::new(tempfile::tempdir().unwrap());
    let path = directory.path().join(name);
    std::fs::write(&path, contents).unwrap();
    let descriptor = unsafe { RegisterClipboardFormatW(w!("FileGroupDescriptorW")) } as u16;
    let content = unsafe { RegisterClipboardFormatW(w!("FileContents")) } as u16;
    let owner = directory.clone();
    let offered = vec![DragFile {
        relative_path: name.into(),
        directory: false,
        size: contents.len() as u64,
    }];
    let object = DataObject {
        files: OnceLock::new(),
        describe: Arc::new(move || Ok(offered.clone())),
        materialize: Arc::new(move |_| {
            Ok(MaterializedFile {
                path: path.clone(),
                lease: owner.clone(),
                cancelled: Arc::new(|| false),
            })
        }),
        cancelled: Arc::new(|| false),
        failure: Arc::new(Mutex::new(None)),
        descriptors_format: descriptor,
        contents_format: content,
    }
    .into();
    (object, directory, descriptor, content)
}

#[test]
fn descriptors_preserve_unicode_and_sizes_and_reject_bad_indices() {
    let (object, _directory, descriptor, content) = object("配置 文件.txt", b"hello");
    let format = FORMATETC {
        cfFormat: descriptor,
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
        ..Default::default()
    };
    unsafe {
        let mut medium = object.GetData(&format).unwrap();
        let memory = medium.u.hGlobal;
        let pointer = GlobalLock(memory).cast::<u8>();
        assert_eq!(pointer.cast::<u32>().read_unaligned(), 1);
        let file = pointer.add(4).cast::<FILEDESCRIPTORW>().read_unaligned();
        let name = file.cFileName;
        let length = name.iter().position(|v| *v == 0).unwrap();
        assert_eq!(String::from_utf16(&name[..length]).unwrap(), "配置 文件.txt");
        let size = file.nFileSizeLow;
        assert_eq!(size, 5);
        let _ = GlobalUnlock(memory);
        ReleaseStgMedium(&mut medium);
        let invalid =
            FORMATETC { cfFormat: content, lindex: 1, tymed: TYMED_ISTREAM.0 as u32, ..format };
        assert_eq!(object.QueryGetData(&invalid), DV_E_LINDEX);
    }
}

#[test]
fn streams_and_clones_keep_temp_files_alive_and_have_independent_cursors() {
    let (object, directory, _, content) = object("a.txt", b"abcdef");
    let path = directory.path().to_owned();
    let format = FORMATETC {
        cfFormat: content,
        dwAspect: DVASPECT_CONTENT.0,
        lindex: 0,
        tymed: TYMED_ISTREAM.0 as u32,
        ..Default::default()
    };
    unsafe {
        let mut medium = object.GetData(&format).unwrap();
        let stream = medium.u.pstm.as_ref().unwrap().clone();
        let mut first = [0u8; 2];
        stream.Read(first.as_mut_ptr().cast(), 2, None).ok().unwrap();
        let cloned = stream.Clone().unwrap();
        ReleaseStgMedium(&mut medium);
        drop(object);
        drop(directory);
        assert!(path.exists());
        let mut from_clone = [0u8; 2];
        cloned.Read(from_clone.as_mut_ptr().cast(), 2, None).ok().unwrap();
        assert_eq!(&from_clone, b"cd");
        let mut from_original = [0u8; 2];
        stream.Read(from_original.as_mut_ptr().cast(), 2, None).ok().unwrap();
        assert_eq!(&from_original, b"cd");
        drop(stream);
        assert!(path.exists());
        drop(cloned);
        assert!(!path.exists());
    }
}

#[test]
fn cancelled_source_does_not_materialize_data() {
    let object: IDataObject = DataObject {
        files: OnceLock::new(),
        describe: Arc::new(|| panic!("cancelled description")),
        materialize: Arc::new(|_| panic!("cancelled materialization")),
        cancelled: Arc::new(|| true),
        failure: Arc::new(Mutex::new(None)),
        descriptors_format: 300,
        contents_format: 301,
    }
    .into();
    let format = FORMATETC {
        cfFormat: 301,
        dwAspect: DVASPECT_CONTENT.0,
        lindex: 0,
        tymed: TYMED_ISTREAM.0 as u32,
        ..Default::default()
    };
    assert_eq!(unsafe { object.GetData(&format) }.err().unwrap().code(), E_ABORT);
}

#[test]
fn format_queries_do_not_wait_for_network_preparation() {
    let object: IDataObject = DataObject {
        files: OnceLock::new(),
        describe: Arc::new(|| panic!("format queries must not load the manifest")),
        materialize: Arc::new(|_| panic!("format queries must not load contents")),
        cancelled: Arc::new(|| false),
        failure: Arc::new(Mutex::new(None)),
        descriptors_format: 300,
        contents_format: 301,
    }
    .into();
    let format = FORMATETC {
        cfFormat: 301,
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_ISTREAM.0 as u32,
        ..Default::default()
    };
    unsafe {
        assert_eq!(object.QueryGetData(&format), S_OK);
        assert_eq!(object.GetData(&format).err().unwrap().code(), DV_E_LINDEX);
    }
}

#[test]
fn an_open_stream_stops_when_the_transfer_is_cancelled() {
    let directory = Arc::new(tempfile::tempdir().unwrap());
    let path = directory.path().join("stream.txt");
    std::fs::write(&path, b"data").unwrap();
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = cancelled.clone();
    let stream: IStream = stream::FileStream::open(MaterializedFile {
        path,
        lease: directory,
        cancelled: Arc::new(move || flag.load(std::sync::atomic::Ordering::Acquire)),
    })
    .unwrap()
    .into();
    cancelled.store(true, std::sync::atomic::Ordering::Release);
    let mut byte = 0u8;
    let mut read = 99;
    unsafe {
        assert_eq!(stream.Read((&mut byte as *mut u8).cast(), 1, Some(&mut read)), E_ABORT);
    }
    assert_eq!(read, 0);
}
