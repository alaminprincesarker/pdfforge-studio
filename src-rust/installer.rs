use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const APP_ICO_BYTES: &[u8] = include_bytes!("icon.ico");
const DOC_ICO_BYTES: &[u8] = include_bytes!("pdf_doc.ico");

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[cfg(target_os = "windows")]
fn run_hidden(program: &str, args: &[&str]) {
    use std::os::windows::process::CommandExt;
    let _ = Command::new(program)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

#[cfg(target_os = "windows")]
fn notify_shell_icon_change() {
    #[link(name = "shell32")]
    extern "system" {
        fn SHChangeNotify(wEventId: i32, uFlags: u32, dwItem1: *const core::ffi::c_void, dwItem2: *const core::ffi::c_void);
    }
    const SHCNE_ASSOCCHANGED: i32 = 0x08000000;
    const SHCNF_IDLIST: u32 = 0x0000;
    unsafe {
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, core::ptr::null(), core::ptr::null());
    }
}

pub fn get_install_dir() -> PathBuf {
    let base = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| env::var_os("APPDATA").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("Programs").join("PDFForge")
}

pub fn check_and_run_installer_mode() -> bool {
    let args: Vec<String> = env::args().collect();
    let exe_path = env::current_exe().unwrap_or_default();
    let exe_name = exe_path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    let is_update_silent = args.iter().any(|a| a.eq_ignore_ascii_case("--update-silent"));
    if is_update_silent {
        let _ = perform_install(&exe_path, true);
        return true;
    }

    let is_uninstall = args.iter().any(|a| {
        a.eq_ignore_ascii_case("--uninstall")
            || a.eq_ignore_ascii_case("--uninstall-silent")
    }) || exe_name.starts_with("uninstall");
    if is_uninstall {
        run_uninstall();
        return true;
    }

    let is_install_only = args.iter().any(|a| a.eq_ignore_ascii_case("--install-silent"));
    if is_install_only {
        let _ = perform_install(&exe_path, false);
        return true;
    }

    let is_setup = args.iter().any(|a| a.eq_ignore_ascii_case("--install"))
        || exe_name.contains("setup")
        || exe_name.contains("installer");
    if is_setup {
        let _ = perform_install(&exe_path, true);
        return true;
    }

    false
}

pub fn perform_install(current_exe: &Path, launch_after: bool) -> Result<PathBuf, String> {
    let install_dir = get_install_dir();
    fs::create_dir_all(&install_dir).map_err(|e| e.to_string())?;

    let target_exe = install_dir.join("PDFForge.exe");
    let uninstaller_exe = install_dir.join("Uninstall-PDFForge.exe");
    let app_ico_path = install_dir.join("PDFForge.ico");
    let doc_ico_path = install_dir.join("PDFForge-Document.ico");

    if current_exe != target_exe {
        let _ = fs::copy(current_exe, &target_exe);
    }
    let _ = fs::copy(current_exe, &uninstaller_exe);
    let _ = fs::write(&app_ico_path, APP_ICO_BYTES);
    let _ = fs::write(&doc_ico_path, DOC_ICO_BYTES);
    // Ship the WebView2 loader next to the installed executable so it always starts.
    let _ = fs::write(install_dir.join("WebView2Loader.dll"), crate::WEBVIEW2_LOADER_BYTES);

    #[cfg(target_os = "windows")]
    {
        let exe_str = target_exe.to_string_lossy().to_string();
        let uninst_cmd = format!("\"{}\" --uninstall", uninstaller_exe.to_string_lossy());
        let quiet_uninst_cmd = format!("\"{}\" --uninstall-silent", uninstaller_exe.to_string_lossy());
        let install_dir_str = install_dir.to_string_lossy().to_string();
        let display_icon = format!("{},0", exe_str);
        let doc_icon = format!("{},1", exe_str);
        let open_cmd = format!("\"{}\" \"%1\"", exe_str);
        let version_str = env!("CARGO_PKG_VERSION");
        let size_kb = fs::metadata(&target_exe)
            .map(|m| (m.len() / 1024).max(1))
            .unwrap_or(6400)
            .to_string();

        // 1. Register in Windows Control Panel (Programs and Features & Settings -> Installed Apps)
        let uninst_key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\PDFForge";
        run_hidden("reg", &["add", uninst_key, "/v", "DisplayName", "/t", "REG_SZ", "/d", "PDFForge Studio", "/f"]);
        run_hidden("reg", &["add", uninst_key, "/v", "DisplayVersion", "/t", "REG_SZ", "/d", version_str, "/f"]);
        run_hidden("reg", &["add", uninst_key, "/v", "Publisher", "/t", "REG_SZ", "/d", "Md. Alamin (PDFForge Studio)", "/f"]);
        run_hidden("reg", &["add", uninst_key, "/v", "Contact", "/t", "REG_SZ", "/d", "alaminprincesarker@gmail.com", "/f"]);
        run_hidden("reg", &["add", uninst_key, "/v", "HelpLink", "/t", "REG_SZ", "/d", "mailto:alaminprincesarker@gmail.com", "/f"]);
        run_hidden("reg", &["add", uninst_key, "/v", "Comments", "/t", "REG_SZ", "/d", "Offline Native Rust PDF Reader & Editor by Md. Alamin", "/f"]);
        run_hidden("reg", &["add", uninst_key, "/v", "DisplayIcon", "/t", "REG_SZ", "/d", &display_icon, "/f"]);
        run_hidden("reg", &["add", uninst_key, "/v", "InstallLocation", "/t", "REG_SZ", "/d", &install_dir_str, "/f"]);
        run_hidden("reg", &["add", uninst_key, "/v", "UninstallString", "/t", "REG_SZ", "/d", &uninst_cmd, "/f"]);
        run_hidden("reg", &["add", uninst_key, "/v", "QuietUninstallString", "/t", "REG_SZ", "/d", &quiet_uninst_cmd, "/f"]);
        run_hidden("reg", &["add", uninst_key, "/v", "EstimatedSize", "/t", "REG_DWORD", "/d", &size_kb, "/f"]);
        run_hidden("reg", &["add", uninst_key, "/v", "NoModify", "/t", "REG_DWORD", "/d", "1", "/f"]);
        run_hidden("reg", &["add", uninst_key, "/v", "NoRepair", "/t", "REG_DWORD", "/d", "1", "/f"]);

        // 2. Register .pdf File Association & Document Icon with small PDFForge logo
        let progid_key = r"HKCU\Software\Classes\PDFForge.Document";
        let progid_icon_key = r"HKCU\Software\Classes\PDFForge.Document\DefaultIcon";
        let progid_cmd_key = r"HKCU\Software\Classes\PDFForge.Document\shell\open\command";
        let pdf_openwith_key = r"HKCU\Software\Classes\.pdf\OpenWithProgids";
        let app_paths_key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\App Paths\PDFForge.exe";

        run_hidden("reg", &["add", progid_key, "/ve", "/t", "REG_SZ", "/d", "PDFForge PDF Document", "/f"]);
        run_hidden("reg", &["add", progid_icon_key, "/ve", "/t", "REG_SZ", "/d", &doc_icon, "/f"]);
        run_hidden("reg", &["add", progid_cmd_key, "/ve", "/t", "REG_SZ", "/d", &open_cmd, "/f"]);
        run_hidden("reg", &["add", pdf_openwith_key, "/v", "PDFForge.Document", "/t", "REG_NONE", "/f"]);
        run_hidden("reg", &["add", app_paths_key, "/ve", "/t", "REG_SZ", "/d", &exe_str, "/f"]);
        run_hidden("reg", &["add", app_paths_key, "/v", "Path", "/t", "REG_SZ", "/d", &install_dir_str, "/f"]);

        // 3. Create Desktop & Start Menu shortcuts (.lnk) with PDFForge icon
        let ps_script = format!(
            "$ws = New-Object -ComObject WScript.Shell; \
             $desk = [Environment]::GetFolderPath('Desktop'); \
             $s1 = $ws.CreateShortcut((Join-Path $desk 'PDFForge.lnk')); \
             $s1.TargetPath = '{exe}'; \
             $s1.WorkingDirectory = '{dir}'; \
             $s1.IconLocation = '{exe},0'; \
             $s1.Description = 'PDFForge - Fast Native Rust PDF Reader & Editor'; \
             $s1.Save(); \
             $sm = [Environment]::GetFolderPath('Programs'); \
             $s2 = $ws.CreateShortcut((Join-Path $sm 'PDFForge.lnk')); \
             $s2.TargetPath = '{exe}'; \
             $s2.WorkingDirectory = '{dir}'; \
             $s2.IconLocation = '{exe},0'; \
             $s2.Description = 'PDFForge - Fast Native Rust PDF Reader & Editor'; \
             $s2.Save();",
            exe = exe_str.replace('\'', "''"),
            dir = install_dir_str.replace('\'', "''")
        );
        run_hidden("powershell", &["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", &ps_script]);

        notify_shell_icon_change();

        if launch_after {
            let _ = Command::new(&target_exe).spawn();
        }
    }

    Ok(target_exe)
}

pub fn get_default_install_dir() -> std::path::PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(local_app) = std::env::var_os("LOCALAPPDATA") {
            return std::path::PathBuf::from(local_app).join("Programs").join("PDFForge");
        }
        return std::path::PathBuf::from(r"C:\Programs\PDFForge");
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::temp_dir().join("PDFForge")
    }
}

pub fn repair_installation() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("Cannot locate running executable: {}", e))?;
    perform_install(&exe, false)
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|e| format!("Failed to repair installation: {}", e))
}

fn parse_semver_triple(raw: &str) -> Option<(u64, u64, u64)> {
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let mut j = i;
            let mut nums = Vec::new();
            while j < bytes.len() && nums.len() < 3 {
                let start = j;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                if j == start {
                    break;
                }
                if let Ok(val) = raw[start..j].parse::<u64>() {
                    nums.push(val);
                }
                if j < bytes.len() && bytes[j] == b'.' {
                    j += 1;
                } else {
                    break;
                }
            }
            if nums.len() >= 2 {
                return Some((nums[0], nums[1], *nums.get(2).unwrap_or(&0)));
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    None
}

pub fn is_newer_semver(candidate: &str, current: &str) -> bool {
    match (parse_semver_triple(candidate), parse_semver_triple(current)) {
        (Some(c), Some(cur)) => c > cur,
        _ => false,
    }
}

pub fn scan_local_update_packages() -> serde_json::Value {
    let current_version = env!("CARGO_PKG_VERSION");
    let mut search_dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        let p = std::path::PathBuf::from(profile);
        search_dirs.push(p.join("Downloads"));
        search_dirs.push(p.join("Desktop"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            search_dirs.push(parent.to_path_buf());
        }
    }

    let mut best: Option<((u64, u64, u64), String, String, String)> = None;
    for dir in search_dirs {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let lower = name.to_ascii_lowercase();
                if lower.contains("pdfforge") && lower.ends_with(".exe") && !lower.starts_with("uninstall") {
                    if let Some(ver_tuple) = parse_semver_triple(&name) {
                        let ver_str = format!("{}.{}.{}", ver_tuple.0, ver_tuple.1, ver_tuple.2);
                        if is_newer_semver(&ver_str, current_version) {
                            let replace = best.as_ref().map(|b| ver_tuple > b.0).unwrap_or(true);
                            if replace {
                                best = Some((ver_tuple, ver_str, path.to_string_lossy().to_string(), name));
                            }
                        }
                    }
                }
            }
        }
    }

    match best {
        Some((_, ver, path, name)) => serde_json::json!({
            "found": true,
            "currentVersion": current_version,
            "latestVersion": ver,
            "filePath": path,
            "fileName": name,
        }),
        None => serde_json::json!({
            "found": false,
            "currentVersion": current_version,
            "latestVersion": current_version,
            "filePath": "",
            "fileName": "",
        }),
    }
}

pub fn apply_update_package(package_path: &str) -> Result<(), String> {
    let src = std::path::Path::new(package_path);
    if !src.is_file() {
        return Err("Selected update package does not exist.".to_string());
    }
    #[cfg(target_os = "windows")]
    {
        use std::io::Read;
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        const DETACHED_PROCESS: u32 = 0x00000008;

        let meta = fs::metadata(src).map_err(|e| format!("Cannot read update file: {}", e))?;
        if meta.len() < 1_000_000 {
            return Err("Selected .exe file is too small to be a valid PDFForge package.".to_string());
        }
        let mut header = [0u8; 2];
        let mut f = fs::File::open(src).map_err(|e| format!("Cannot open update file: {}", e))?;
        f.read_exact(&mut header).map_err(|e| format!("Invalid executable header: {}", e))?;
        if header != *b"MZ" {
            return Err("Selected file is not a valid Windows executable (.exe).".to_string());
        }

        let staged = std::env::temp_dir().join(format!("PDFForge-Update-{}.exe", std::process::id()));
        fs::copy(src, &staged).map_err(|e| format!("Failed to stage update package: {}", e))?;

        let target_exe = get_default_install_dir().join("PDFForge.exe");
        let bat_path = std::env::temp_dir().join(format!("pdfforge_update_{}.cmd", std::process::id()));
        let bat_content = format!(
            "@echo off\r\nping 127.0.0.1 -n 2 >nul\r\n\"{staged}\" --install-silent\r\ndel /f /q \"{staged}\" >nul 2>&1\r\nif exist \"{target}\" start \"\" \"{target}\"\r\ndel /f /q \"%~f0\" >nul 2>&1\r\n",
            staged = staged.to_string_lossy(),
            target = target_exe.to_string_lossy()
        );
        fs::write(&bat_path, bat_content).map_err(|e| format!("Failed to write update helper: {}", e))?;
        Command::new("cmd")
            .current_dir(std::env::temp_dir())
            .arg("/C")
            .arg(&bat_path)
            .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
            .spawn()
            .map_err(|e| format!("Failed to start background updater: {}", e))?;
        return Ok(());
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = src;
        Err("In-app binary self-update requires Windows or replacing the macOS app bundle.".to_string())
    }
}

pub fn check_release_manifest(manifest_url: &str) -> serde_json::Value {
    let current_version = env!("CARGO_PKG_VERSION");
    let trimmed = manifest_url.trim();
    if trimmed.is_empty() {
        let local = scan_local_update_packages();
        return serde_json::json!({
            "ok": true,
            "mode": "local",
            "currentVersion": current_version,
            "latestVersion": local["latestVersion"].as_str().unwrap_or(current_version),
            "hasUpdate": local["found"].as_bool().unwrap_or(false),
            "localFilePath": local["filePath"].as_str().unwrap_or(""),
            "localFileName": local["fileName"].as_str().unwrap_or(""),
            "releaseUrl": "",
            "notes": if local["found"].as_bool().unwrap_or(false) {
                format!("Found newer local package: {}", local["fileName"].as_str().unwrap_or(""))
            } else {
                "Running latest offline release. No newer local package found in Downloads.".to_string()
            },
            "error": ""
        });
    }
    if !(trimmed.starts_with("https://") || trimmed.starts_with("http://")) {
        return serde_json::json!({
            "ok": false,
            "currentVersion": current_version,
            "hasUpdate": false,
            "error": "Release URL must start with https:// or http://"
        });
    }

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let ps = format!(
            "[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12; \
             $r = Invoke-RestMethod -Uri '{url}' -Headers @{{'User-Agent'='PDFForge-Updater'}} -TimeoutSec 6; \
             $ver = if ($r.version) {{ $r.version }} elseif ($r.tag_name) {{ $r.tag_name }} else {{ '' }}; \
             $dl = if ($r.downloadUrl) {{ $r.downloadUrl }} elseif ($r.html_url) {{ $r.html_url }} else {{ '' }}; \
             $notes = if ($r.notes) {{ $r.notes }} elseif ($r.body) {{ $r.body }} else {{ '' }}; \
             @{{ version=[string]$ver; releaseUrl=[string]$dl; notes=[string]$notes }} | ConvertTo-Json -Compress",
            url = trimmed.replace('\'', "''")
        );
        match Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", &ps])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            Ok(out) if out.status.success() => {
                let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
                    let latest = parsed["version"].as_str().unwrap_or("").trim().trim_start_matches('v').to_string();
                    let rel_url = parsed["releaseUrl"].as_str().unwrap_or("").to_string();
                    let notes = parsed["notes"].as_str().unwrap_or("").to_string();
                    let has_update = !latest.is_empty() && is_newer_semver(&latest, current_version);
                    return serde_json::json!({
                        "ok": true,
                        "mode": "remote",
                        "currentVersion": current_version,
                        "latestVersion": if latest.is_empty() { current_version.to_string() } else { latest },
                        "hasUpdate": has_update,
                        "releaseUrl": rel_url,
                        "notes": notes,
                        "error": ""
                    });
                }
                return serde_json::json!({
                    "ok": false,
                    "currentVersion": current_version,
                    "hasUpdate": false,
                    "error": "Release endpoint returned non-JSON or unrecognized response."
                });
            }
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                return serde_json::json!({
                    "ok": false,
                    "currentVersion": current_version,
                    "hasUpdate": false,
                    "error": if err.is_empty() { "Failed to reach update URL (device offline or endpoint unreachable).".to_string() } else { err }
                });
            }
            Err(e) => {
                return serde_json::json!({
                    "ok": false,
                    "currentVersion": current_version,
                    "hasUpdate": false,
                    "error": format!("Failed to invoke update check: {}", e)
                });
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        serde_json::json!({
            "ok": false,
            "currentVersion": current_version,
            "hasUpdate": false,
            "error": "Online manifest check is only enabled on Windows builds."
        })
    }
}

pub fn run_uninstall() {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        const DETACHED_PROCESS: u32 = 0x00000008;

        let args: Vec<String> = std::env::args().collect();
        let silent = args.iter().any(|a| {
            a.eq_ignore_ascii_case("--uninstall-silent")
                || a.eq_ignore_ascii_case("--silent")
                || a.eq_ignore_ascii_case("/S")
                || a.eq_ignore_ascii_case("-s")
        });

        // Release any current working directory lock inside the install directory before deleting files.
        let temp_dir = std::env::temp_dir();
        let _ = std::env::set_current_dir(&temp_dir);

        // Terminate any running PDFForge instances (excluding our own PID).
        let pid_filter = format!("PID ne {}", std::process::id());
        run_hidden("taskkill", &["/F", "/FI", &pid_filter, "/IM", "PDFForge.exe"]);
        run_hidden("taskkill", &["/F", "/FI", &pid_filter, "/IM", "Uninstall-PDFForge.exe"]);
        std::thread::sleep(std::time::Duration::from_millis(350));

        // 1. Clean Windows Registry uninstall entry, ProgID, file associations, and App Paths.
        let uninst_key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\PDFForge";
        let progid_key = r"HKCU\Software\Classes\PDFForge.Document";
        let pdf_openwith_key = r"HKCU\Software\Classes\.pdf\OpenWithProgids";
        let app_paths_key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\App Paths\PDFForge.exe";

        run_hidden("reg", &["delete", uninst_key, "/f"]);
        run_hidden("reg", &["delete", progid_key, "/f"]);
        run_hidden("reg", &["delete", pdf_openwith_key, "/v", "PDFForge.Document", "/f"]);
        run_hidden("reg", &["delete", app_paths_key, "/f"]);

        // 2. Remove Desktop and Start Menu shortcuts.
        let ps_cleanup = "\
            $desk = Join-Path ([Environment]::GetFolderPath('Desktop')) 'PDFForge.lnk'; \
            if (Test-Path $desk) { Remove-Item $desk -Force -ErrorAction SilentlyContinue }; \
            $sm = Join-Path ([Environment]::GetFolderPath('Programs')) 'PDFForge.lnk'; \
            if (Test-Path $sm) { Remove-Item $sm -Force -ErrorAction SilentlyContinue };";
        run_hidden("powershell", &["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", ps_cleanup]);

        // 3. Remove unlocked application files from %LOCALAPPDATA%\Programs\PDFForge (and exe parent if applicable).
        let current_exe = std::env::current_exe().ok();
        let install_dir = get_default_install_dir();
        let mut dirs_to_clean = vec![install_dir.clone()];
        if let Some(ref exe) = current_exe {
            if let Some(parent) = exe.parent() {
                let parent_name = parent
                    .file_name()
                    .map(|n| n.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                if parent_name == "pdfforge" && !dirs_to_clean.iter().any(|d| d == parent) {
                    dirs_to_clean.push(parent.to_path_buf());
                }
            }
        }

        let current_exe_canon = current_exe.as_ref().and_then(|p| fs::canonicalize(p).ok());
        for dir in &dirs_to_clean {
            if !dir.exists() {
                continue;
            }
            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_dir() {
                        let _ = fs::remove_dir_all(&p);
                        continue;
                    }
                    let is_self = match (&current_exe_canon, fs::canonicalize(&p).ok()) {
                        (Some(a), Some(b)) => a == &b,
                        _ => current_exe.as_ref().map(|ce| ce == &p).unwrap_or(false),
                    };
                    if !is_self {
                        for _ in 0..4 {
                            if fs::remove_file(&p).is_ok() {
                                break;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(120));
                        }
                    }
                }
            }
            let _ = fs::remove_dir(dir);
        }

        notify_shell_icon_change();

        if !silent {
            let _ = rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Info)
                .set_title("PDFForge Studio Uninstaller")
                .set_description("PDFForge Studio has been completely uninstalled.\n\nAll program executables, shortcuts, file associations, and registry entries have been removed.")
                .set_buttons(rfd::MessageButtons::Ok)
                .show();
        }

        // 4. Spawn detached cmd.exe right before exit to delete the running uninstaller binary and remaining install folder.
        let self_in_install_dir = current_exe
            .as_ref()
            .and_then(|exe| exe.parent())
            .map(|p| dirs_to_clean.iter().any(|d| d.eq_ignore_ascii_case_win(p)))
            .unwrap_or(false);

        if self_in_install_dir || dirs_to_clean.iter().any(|d| d.exists()) {
            let exe_del = current_exe
                .as_ref()
                .filter(|_| self_in_install_dir)
                .map(|p| format!("del /f /q \"{}\" >nul 2>&1\r\n", p.to_string_lossy()))
                .unwrap_or_default();
            let dir_path_str = install_dir.to_string_lossy().to_string();
            let bat_path = temp_dir.join(format!("pdfforge_cleanup_{}.cmd", std::process::id()));
            let bat_content = format!(
                "@echo off\r\nfor /L %%i in (1,1,15) do (\r\n  ping 127.0.0.1 -n 2 >nul\r\n  {exe_del}  rmdir /s /q \"{dir}\" >nul 2>&1\r\n  if not exist \"{dir}\" goto done\r\n)\r\n:done\r\ndel /f /q \"%~f0\" >nul 2>&1\r\n",
                exe_del = exe_del,
                dir = dir_path_str
            );
            if fs::write(&bat_path, bat_content).is_ok() {
                let _ = Command::new("cmd")
                    .current_dir(&temp_dir)
                    .arg("/C")
                    .arg(&bat_path)
                    .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
                    .spawn();
            }
        }
    }
}

trait PathEqWin {
    fn eq_ignore_ascii_case_win(&self, other: &std::path::Path) -> bool;
}

impl PathEqWin for std::path::PathBuf {
    fn eq_ignore_ascii_case_win(&self, other: &std::path::Path) -> bool {
        self.to_string_lossy().eq_ignore_ascii_case(&other.to_string_lossy())
    }
}

