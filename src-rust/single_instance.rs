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
}

#[derive(Serialize, Deserialize)]
struct ForwardPayload {
    paths: Vec<String>,
}

pub struct InstanceGuard {
    #[cfg(target_os = "windows")]
    mutex_handle: isize,
    listener: Option<TcpListener>,
    info_path: PathBuf,
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.info_path);
        #[cfg(target_os = "windows")]
        if self.mutex_handle != 0 {
            extern "system" {
                fn CloseHandle(hObject: isize) -> i32;
            }
            unsafe {
                let _ = CloseHandle(self.mutex_handle);
            }
        }
    }
}

#[cfg(target_os = "windows")]
mod win {
    use std::os::windows::ffi::OsStrExt;
    use tao::platform::windows::WindowExtWindows;

    const ERROR_ALREADY_EXISTS: u32 = 183;
    const SW_RESTORE: i32 = 9;

    extern "system" {
        fn CreateMutexW(
            lpMutexAttributes: *const core::ffi::c_void,
            bInitialOwner: i32,
            lpName: *const u16,
        ) -> isize;
        fn GetLastError() -> u32;
        fn AllowSetForegroundWindow(dwProcessId: u32) -> i32;
        fn IsIconic(hWnd: isize) -> i32;
        fn ShowWindow(hWnd: isize, nCmdShow: i32) -> i32;
        fn SetForegroundWindow(hWnd: isize) -> i32;
        fn BringWindowToTop(hWnd: isize) -> i32;
    }

    pub fn try_create_mutex() -> Result<isize, ()> {
        let wide: Vec<u16> = std::ffi::OsStr::new("Local\\PDFForgeStudio_SingleInstance_v1")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            let handle = CreateMutexW(core::ptr::null(), 0, wide.as_ptr());
            let err = GetLastError();
            if handle == 0 || err == ERROR_ALREADY_EXISTS {
                if handle != 0 {
                    extern "system" {
                        fn CloseHandle(hObject: isize) -> i32;
                    }
                    let _ = CloseHandle(handle);
                }
                return Err(());
            }
            Ok(handle)
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
}

fn try_forward_to_existing(info_path: &Path, pdf_paths: &[String]) -> bool {
    let payload_line = match serde_json::to_string(&ForwardPayload {
        paths: pdf_paths.to_vec(),
    }) {
        Ok(json) => format!("{}\n", json),
        Err(_) => return false,
    };

    for _ in 0..30 {
        if let Ok(raw) = fs::read_to_string(info_path) {
            if let Ok(ep) = serde_json::from_str::<InstanceEndpoint>(&raw) {
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
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

pub fn try_acquire_or_forward(app_data_dir: &Path, pdf_paths: &[String]) -> Option<InstanceGuard> {
    let _ = fs::create_dir_all(app_data_dir);
    let info_path = app_data_dir.join("instance.json");

    #[cfg(target_os = "windows")]
    let mutex_handle = match win::try_create_mutex() {
        Ok(h) => h,
        Err(()) => {
            if try_forward_to_existing(&info_path, pdf_paths) {
                return None;
            }
            // Previous primary just exited between mutex check and connect; try once more to acquire.
            match win::try_create_mutex() {
                Ok(h) => h,
                Err(()) => return None,
            }
        }
    };

    #[cfg(not(target_os = "windows"))]
    {
        if info_path.exists() && try_forward_to_existing(&info_path, pdf_paths) {
            return None;
        }
    }

    let listener = TcpListener::bind("127.0.0.1:0").ok();
    if let Some(ref l) = listener {
        if let Ok(addr) = l.local_addr() {
            let ep = InstanceEndpoint {
                port: addr.port(),
                pid: std::process::id(),
            };
            if let Ok(json) = serde_json::to_string(&ep) {
                let _ = fs::write(&info_path, json);
            }
        }
    }

    Some(InstanceGuard {
        #[cfg(target_os = "windows")]
        mutex_handle,
        listener,
        info_path,
    })
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

