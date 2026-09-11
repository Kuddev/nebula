//! Desktop-only acceptance: actual OLE routing between two owned test windows.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HWND as RawHwnd, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEINPUT, SendInput,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

#[implement(IDropTarget)]
struct Target {
    directory: PathBuf,
    copied: Arc<AtomicBool>,
}

impl IDropTarget_Impl for Target_Impl {
    fn DragEnter(
        &self,
        _: Ref<'_, IDataObject>,
        _: MODIFIERKEYS_FLAGS,
        _: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> ::windows::core::Result<()> {
        eprintln!("OLE probe: target drag-enter");
        unsafe { effect.write(DROPEFFECT_COPY) };
        Ok(())
    }
    fn DragOver(
        &self,
        _: MODIFIERKEYS_FLAGS,
        _: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> ::windows::core::Result<()> {
        unsafe { effect.write(DROPEFFECT_COPY) };
        Ok(())
    }
    fn DragLeave(&self) -> ::windows::core::Result<()> {
        Ok(())
    }
    fn Drop(
        &self,
        object: Ref<'_, IDataObject>,
        _: MODIFIERKEYS_FLAGS,
        _: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> ::windows::core::Result<()> {
        eprintln!("OLE probe: target drop");
        let object = object.as_ref().ok_or_else(|| error(E_POINTER))?;
        let format = FORMATETC {
            cfFormat: unsafe { RegisterClipboardFormatW(w!("FileGroupDescriptorW")) } as u16,
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
            ..Default::default()
        };
        unsafe {
            let mut descriptors = object.GetData(&format)?;
            eprintln!("OLE probe: descriptors received");
            let memory = descriptors.u.hGlobal;
            let bytes = GlobalLock(memory).cast::<u8>();
            let count = bytes.cast::<u32>().read_unaligned();
            let descriptor = bytes.add(4).cast::<FILEDESCRIPTORW>().read_unaligned();
            let name = descriptor.cFileName;
            let _ = GlobalUnlock(memory);
            ReleaseStgMedium(&mut descriptors);
            let end = name.iter().position(|unit| *unit == 0).unwrap_or(name.len());
            // The probe only ever writes its one known fixture name.
            if count != 1 || String::from_utf16_lossy(&name[..end]) != "拖放 fixture.txt" {
                return Err(error(E_INVALIDARG));
            }
            let format = FORMATETC {
                cfFormat: RegisterClipboardFormatW(w!("FileContents")) as u16,
                dwAspect: DVASPECT_CONTENT.0,
                lindex: 0,
                tymed: TYMED_ISTREAM.0 as u32,
                ..Default::default()
            };
            let mut contents = object.GetData(&format)?;
            eprintln!("OLE probe: contents received");
            let stream = contents.u.pstm.as_ref().ok_or_else(|| error(E_POINTER))?;
            let mut result = Vec::new();
            loop {
                let mut buffer = [0u8; 1024];
                let mut count = 0;
                stream
                    .Read(buffer.as_mut_ptr().cast(), buffer.len() as u32, Some(&mut count))
                    .ok()?;
                if count == 0 {
                    break;
                }
                result.extend_from_slice(&buffer[..count as usize]);
                if result.len() > 65536 {
                    return Err(error(E_INVALIDARG));
                }
            }
            ReleaseStgMedium(&mut contents);
            std::fs::write(self.directory.join("拖放 fixture.txt"), result)
                .map_err(|_| error(E_FAIL))?;
            self.copied.store(true, Ordering::Release);
            effect.write(DROPEFFECT_COPY);
        }
        Ok(())
    }
}

unsafe extern "system" fn procedure(
    window: RawHwnd,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

fn mouse(flags: u32) {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: MOUSEINPUT { dwFlags: flags, ..unsafe { std::mem::zeroed() } } },
    };
    assert_eq!(unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) }, 1);
}

fn center(window: RawHwnd) -> POINT {
    let mut rectangle = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    assert_ne!(unsafe { GetWindowRect(window, &mut rectangle) }, 0);
    POINT { x: (rectangle.left + rectangle.right) / 2, y: (rectangle.top + rectangle.bottom) / 2 }
}

struct Windows {
    source: RawHwnd,
    target: RawHwnd,
    cursor: POINT,
    foreground: RawHwnd,
}

impl Drop for Windows {
    fn drop(&mut self) {
        unsafe {
            let _ = RevokeDragDrop(HWND(self.target));
            DestroyWindow(self.source);
            DestroyWindow(self.target);
            SetCursorPos(self.cursor.x, self.cursor.y);
            if !self.foreground.is_null() {
                SetForegroundWindow(self.foreground);
            }
        }
    }
}

#[test]
#[ignore = "requires an interactive Windows desktop; drives only isolated probe windows"]
fn native_worker_drag_reaches_an_ole_target_and_copies_unicode_file() {
    assert!(unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } >= 0, "Mouse is currently in use");
    unsafe {
        OleInitialize(None).unwrap();
    }
    let _apartment = Apartment;
    let class: Vec<u16> = "PebrelFileDragAcceptance\0".encode_utf16().collect();
    let title: Vec<u16> = "Pebrel isolated drag acceptance\0".encode_utf16().collect();
    let window_class = WNDCLASSW {
        lpfnWndProc: Some(procedure),
        lpszClassName: class.as_ptr(),
        ..unsafe { std::mem::zeroed() }
    };
    unsafe {
        RegisterClassW(&window_class);
    }
    let mut cursor = POINT { x: 0, y: 0 };
    unsafe {
        GetCursorPos(&mut cursor);
    }
    let foreground = unsafe { GetForegroundWindow() };
    let create = |x| unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            x,
            120,
            300,
            200,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    let source = create(80);
    let target = create(420);
    assert!(!source.is_null() && !target.is_null());
    let _windows = Windows { source, target, cursor, foreground };
    let input = Arc::new(tempfile::tempdir().unwrap());
    let output = tempfile::tempdir().unwrap();
    let input_path = input.path().join("拖放 fixture.txt");
    std::fs::write(&input_path, "Native OLE / 中文 / preserved contents").unwrap();
    let copied = Arc::new(AtomicBool::new(false));
    let drop_target: IDropTarget =
        Target { directory: output.path().to_owned(), copied: copied.clone() }.into();
    unsafe {
        RegisterDragDrop(HWND(target), &drop_target).unwrap();
    }
    let source_point = center(source);
    let target_point = center(target);
    let source_id = source as usize;
    let target_id = target as usize;
    DRAG_PROBE_READY.store(false, Ordering::Release);
    let (send, receive) = std::sync::mpsc::channel();
    let origin = DragOrigin::capture();
    std::thread::spawn(move || {
        unsafe {
            SetCursorPos(source_point.x, source_point.y);
        }
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(unsafe { WindowFromPoint(source_point) } as usize, source_id);
        mouse(MOUSEEVENTF_LEFTDOWN);
        eprintln!("OLE probe: mouse down");
        std::thread::sleep(Duration::from_millis(80));
        let motion = std::thread::spawn(move || {
            let ready_deadline = Instant::now() + Duration::from_secs(5);
            while !DRAG_PROBE_READY.load(Ordering::Acquire) {
                if Instant::now() >= ready_deadline {
                    mouse(MOUSEEVENTF_LEFTUP);
                    return false;
                }
                let input = INPUT {
                    r#type: INPUT_MOUSE,
                    Anonymous: INPUT_0 {
                        mi: MOUSEINPUT {
                            dx: 1,
                            dy: 0,
                            dwFlags:
                                windows_sys::Win32::UI::Input::KeyboardAndMouse::MOUSEEVENTF_MOVE,
                            ..unsafe { std::mem::zeroed() }
                        },
                    },
                };
                unsafe {
                    SendInput(1, &input, size_of::<INPUT>() as i32);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            unsafe {
                SetCursorPos(target_point.x, target_point.y);
            }
            std::thread::sleep(Duration::from_millis(180));
            let at_target = unsafe { WindowFromPoint(target_point) } as usize == target_id;
            mouse(MOUSEEVENTF_LEFTUP);
            eprintln!("OLE probe: mouse up, target={at_target}, state={}", unsafe {
                GetAsyncKeyState(VK_LBUTTON.0 as i32)
            });
            at_target
        });
        let result = super::super::run(
            origin,
            Arc::new(|| {
                std::thread::sleep(Duration::from_millis(120));
                Ok(vec![DragFile {
                    relative_path: "拖放 fixture.txt".into(),
                    directory: false,
                    size: 0,
                }])
            }),
            Arc::new(move |_| {
                Ok(MaterializedFile {
                    path: input_path.clone(),
                    lease: input.clone(),
                    cancelled: Arc::new(|| false),
                })
            }),
            Arc::new(|| false),
        );
        eprintln!("OLE probe: native returned {result:?}");
        let at_target = motion.join().unwrap();
        let _ = send.send((result, at_target));
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    let result = loop {
        let mut message: MSG = unsafe { std::mem::zeroed() };
        unsafe {
            while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        if let Ok(result) = receive.try_recv() {
            break result;
        }
        assert!(
            Instant::now() < deadline,
            "Native drag did not complete; copied={}",
            copied.load(Ordering::Acquire)
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(result.1, "Pointer must land on the owned target window");
    assert_eq!(result.0.unwrap(), DragOutcome::Copied);
    assert!(copied.load(Ordering::Acquire));
    assert_eq!(
        std::fs::read_to_string(output.path().join("拖放 fixture.txt")).unwrap(),
        "Native OLE / 中文 / preserved contents"
    );
}
