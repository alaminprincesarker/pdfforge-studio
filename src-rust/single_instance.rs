use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

#[derive(Serialize, Deserialize)]
struct InstanceEndpoint {
    port: u16,
    pid: u32,
    #[serde(default)]
    hwnd: isize,
}

#[derive(Serialize, Deserialize)]
struct ForwardPayload {
    paths: Vec<String>,
}

pub struct InstanceGuard {
    listener: Option<TcpListener>,
    info_path: PathBuf,
    port: u16,
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.info_path);
    }
}

#[cfg(target_os = "windows")]
mod win {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use tao::platform::windows::WindowExtWindows;

    const SW_RESTORE: i32 = 9;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_ABANDONED: u32 = 0x00000080;
    const COINIT_APARTMENTTHREADED: u32 = 0x2;
    const CLSCTX_INPROC_SERVER: u32 = 0x1;
    const RPC_E_CHANGED_MODE: i32 = -2147417850; // 0x80010106

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    const CLSID_VIRTUAL_DESKTOP_MANAGER: Guid = Guid {
        data1: 0xaa509086,
        data2: 0x5ca9,
        data3: 0x4c25,
        data4: [0x8f, 0x95, 0x58, 0x9d, 0x3c, 0x07, 0xb4, 0x8a],
    };

    const IID_IVIRTUAL_DESKTOP_MANAGER: Guid = Guid {
        data1: 0xa5cd92ff,
        data2: 0x29be,
        data3: 0x454c,
        data4: [0x8d, 0x04, 0xd8, 0x28, 0x79, 0xfb, 0x3f, 0x1b],
    };

    #[repr(C)]
    struct VirtualDesktopManagerVtbl {
        query_interface: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32,
        add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
        is_window_on_current_virtual_desktop:
            unsafe extern "system" fn(*mut c_void, isize, *mut i32) -> i32,
    }

    #[link(name = "ole32")]
    extern "system" {
        fn CoInitializeEx(pvReserved: *const c_void, dwCoInit: u32) -> i32;
        fn CoUninitialize();
        fn CoCreateInstance(
            rclsid: *const Guid,
            pUnkOuter: *const c_void,
            dwClsContext: u32,
            riid: *const Guid,
            ppv: *mut *mut c_void,
        ) -> i32;
    }

    extern "system" {
        fn CreateMutexW(
            lpMutexAttributes: *const c_void,
            bInitialOwner: i32,
            lpName: *const u16,
        ) -> isize;
        fn WaitForSingleObject(hHandle: isize, dwMilliseconds: u32) -> u32;
        fn ReleaseMutex(hMutex: isize) -> i32;
        fn CloseHandle(hObject: isize) -> i32;
        fn OpenProcess(dwDesiredAccess: u32, bInheritHandle: i32, dwProcessId: u32) -> isize;
        fn GetExitCodeProcess(hProcess: isize, lpExitCode: *mut u32) -> i32;
        fn AllowSetForegroundWindow(dwProcessId: u32) -> i32;
        fn IsWindow(hWnd: isize) -> i32;
        fn IsIconic(hWnd: isize) -> i32;
        fn ShowWindow(hWnd: isize, nCmdShow: i32) -> i32;
        fn SetForegroundWindow(hWnd: isize) -> i32;
        fn BringWindowToTop(hWnd: isize) -> i32;
    }

    pub struct BootLock(isize);

    impl Drop for BootLock {
        fn drop(&mut self) {
            if self.0 != 0 {
                unsafe {
                    let _ = ReleaseMutex(self.0);
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    pub fn acquire_boot_lock() -> Option<BootLock> {
        let wide: Vec<u16> = std::ffi::OsStr::new("Local\\PDFForgeStudio_BootSync_v1")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            let h = CreateMutexW(core::ptr::null(), 0, wide.as_ptr());
            if h == 0 {
                return None;
            }
            let wait = WaitForSingleObject(h, 2500);
            if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
                Some(BootLock(h))
            } else {
                let _ = CloseHandle(h);
                None
            }
        }
    }

    pub fn is_process_alive(pid: u32) -> bool {
        if pid == 0 {
            return false;
        }
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h == 0 {
                return false;
            }
            let mut code: u32 = 0;
            let ok = GetExitCodeProcess(h, &mut code) != 0 && code == STILL_ACTIVE;
            let _ = CloseHandle(h);
            ok
        }
    }

    pub fn is_window_alive(hwnd: isize) -> bool {
        hwnd != 0 && unsafe { IsWindow(hwnd) != 0 }
    }

    pub fn is_window_on_current_virtual_desktop(hwnd: isize) -> bool {
        if hwnd == 0 || unsafe { IsWindow(hwnd) == 0 } {
            return false;
        }
        unsafe {
            let init_hr = CoInitializeEx(core::ptr::null(), COINIT_APARTMENTTHREADED);
            let should_uninit = init_hr >= 0;
            let mut mgr: *mut c_void = core::ptr::null_mut();
            let hr = CoCreateInstance(
                &CLSID_VIRTUAL_DESKTOP_MANAGER,
                core::ptr::null(),
                CLSCTX_INPROC_SERVER,
                &IID_IVIRTUAL_DESKTOP_MANAGER,
                &mut mgr,
            );
            let result = if hr >= 0 && !mgr.is_null() {
                let vtbl = *(mgr as *const *const VirtualDesktopManagerVtbl);
                let mut on_current: i32 = 1;
                let chk_hr = ((*vtbl).is_window_on_current_virtual_desktop)(
                    mgr,
                    hwnd,
                    &mut on_current,
                );
                ((*vtbl).release)(mgr);
                if chk_hr >= 0 {
                    on_current != 0
                } else {
                    true
                }
            } else if init_hr == RPC_E_CHANGED_MODE {
                true
            } else {
                true
            };
            if should_uninit {
                CoUninitialize();
            }
            result
        }
    }

    pub fn allow_foreground(pid: u32) {
        if pid != 0 {
            unsafe {
                let _ = AllowSetForegroundWindow(pid);
            }
        }
    }

    pub fn bring_to_front(window: &tao::window::Window) {
        let hwnd = window.hwnd() as isize;
        if hwnd != 0 {
            unsafe {
                if IsIconic(hwnd) != 0 {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                }
                let _ = BringWindowToTop(hwnd);
                let _ = SetForegroundWindow(hwnd);
            }
        }
    }

    pub fn window_hwnd(window: &tao::window::Window) -> isize {
        window.hwnd() as isize
    }

    pub fn wait_for_process_exit(pid: u32, timeout_ms: u32) {
        let start = std::time::Instant::now();
        let max = std::time::Duration::from_millis(timeout_ms as u64);
        while is_process_alive(pid) && start.elapsed() < max {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

#[cfg(target_os = "windows")]
pub use win::wait_for_process_exit;

#[cfg(not(target_os = "windows"))]
pub fn wait_for_process_exit(_pid: u32, _timeout_ms: u32) {}

fn send_paths_to_endpoint(ep: &InstanceEndpoint, pdf_paths: &[String]) -> bool {
    let payload_line = match serde_json::to_string(&ForwardPayload {
        paths: pdf_paths.to_vec(),
    }) {
        Ok(json) => format!("{}\n", json),
        Err(_) => return false,
    };

    #[cfg(target_os = "windows")]
    win::allow_foreground(ep.pid);

    if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", ep.port)) {
        let _ = stream.set_read_timeout(Some(Duration::from_millis(800)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(800)));
        if stream.write_all(payload_line.as_bytes()).is_ok() {
            let _ = stream.flush();
            let mut reader = BufReader::new(stream);
            let mut ack = String::new();
            let _ = reader.read_line(&mut ack);
            return true;
        }
    }
    false
}

#[cfg(target_os = "windows")]
fn try_forward_on_current_desktop(instances_dir: &Path, pdf_paths: &[String]) -> bool {
    let Ok(entries) = fs::read_dir(instances_dir) else {
        return false;
    };

    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|x| x.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    files.sort();

    for path in files {
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(mut ep) = serde_json::from_str::<InstanceEndpoint>(&raw) else {
            let _ = fs::remove_file(&path);
            continue;
        };

        if !win::is_process_alive(ep.pid) {
            let _ = fs::remove_file(&path);
            continue;
        }

        // If a sibling instance on Windows just booted (<1.5s) and hasn't written hwnd yet, wait briefly.
        if ep.hwnd == 0 {
            for _ in 0..30 {
                thread::sleep(Duration::from_millis(50));
                if let Ok(raw2) = fs::read_to_string(&path) {
                    if let Ok(ep2) = serde_json::from_str::<InstanceEndpoint>(&raw2) {
                        ep = ep2;
                        if ep.hwnd != 0 {
                            break;
                        }
                    }
                }
                if !win::is_process_alive(ep.pid) {
                    break;
                }
            }
        }

        if ep.hwnd != 0 && !win::is_window_alive(ep.hwnd) {
            continue;
        }

        // Virtual Desktop check: ONLY forward if the existing window lives on the CURRENT virtual desktop.
        if ep.hwnd != 0 && !win::is_window_on_current_virtual_desktop(ep.hwnd) {
            continue;
        }

        if send_paths_to_endpoint(&ep, pdf_paths) {
            return true;
        }
    }
    false
}

#[cfg(not(target_os = "windows"))]
fn try_forward_on_current_desktop(instances_dir: &Path, pdf_paths: &[String]) -> bool {
    let Ok(entries) = fs::read_dir(instances_dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(ep) = serde_json::from_str::<InstanceEndpoint>(&raw) {
                if send_paths_to_endpoint(&ep, pdf_paths) {
                    return true;
                }
            }
        }
    }
    false
}

pub fn try_acquire_or_forward(app_data_dir: &Path, pdf_paths: &[String]) -> Option<InstanceGuard> {
    let instances_dir = app_data_dir.join("instances");
    let _ = fs::create_dir_all(&instances_dir);
    let _ = fs::remove_file(app_data_dir.join("instance.json"));

    #[cfg(target_os = "windows")]
    let _boot_lock = win::acquire_boot_lock();

    if try_forward_on_current_desktop(&instances_dir, pdf_paths) {
        return None;
    }

    let pid = std::process::id();
    let info_path = instances_dir.join(format!("{}.json", pid));
    let listener = TcpListener::bind("127.0.0.1:0").ok();
    let port = listener
        .as_ref()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
        .unwrap_or(0);

    if port != 0 {
        let ep = InstanceEndpoint { port, pid, hwnd: 0 };
        if let Ok(json) = serde_json::to_string(&ep) {
            let _ = fs::write(&info_path, json);
        }
    }

    Some(InstanceGuard {
        listener,
        info_path,
        port,
    })
}

pub fn register_window(guard: &InstanceGuard, window: &tao::window::Window) {
    if guard.port == 0 {
        return;
    }
    #[cfg(target_os = "windows")]
    let hwnd = win::window_hwnd(window);
    #[cfg(not(target_os = "windows"))]
    let hwnd: isize = 0;

    let ep = InstanceEndpoint {
        port: guard.port,
        pid: std::process::id(),
        hwnd,
    };
    if let Ok(json) = serde_json::to_string(&ep) {
        let _ = fs::write(&guard.info_path, json);
    }
}

pub fn start_listener<F>(guard: &mut InstanceGuard, on_open_files: F)
where
    F: Fn(Vec<String>) + Send + 'static,
{
    let Some(listener) = guard.listener.take() else {
        return;
    };
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).is_ok() {
                if let Ok(payload) = serde_json::from_str::<ForwardPayload>(line.trim()) {
                    on_open_files(payload.paths);
                }
                let _ = reader.into_inner().write_all(b"OK\n");
            }
        }
    });
}

pub fn bring_window_to_front(window: &tao::window::Window) {
    window.set_minimized(false);
    window.set_visible(true);
    #[cfg(target_os = "windows")]
    win::bring_to_front(window);
    window.set_focus();
}

