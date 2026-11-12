use rfd::FileDialog;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenFileDialogResponse {
    pub canceled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenMultipleFilesDialogResponse {
    pub canceled: bool,
    pub file_paths: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveFileDialogResponse {
    pub canceled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
}

pub fn open_file_dialog() -> OpenFileDialogResponse {
    let picked = FileDialog::new()
        .set_title("Open PDF Document")
        .add_filter("PDF Documents", &["pdf"])
        .pick_file();

    match picked {
        Some(path) => {
            let file_path = path.to_string_lossy().to_string();
            let file_name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "document.pdf".to_string());
            OpenFileDialogResponse {
                canceled: false,
                file_path: Some(file_path),
                file_name: Some(file_name),
            }
        }
        None => OpenFileDialogResponse {
            canceled: true,
            file_path: None,
            file_name: None,
        },
    }
}

pub fn open_multiple_files_dialog() -> OpenMultipleFilesDialogResponse {
    let picked = FileDialog::new()
        .set_title("Select PDF Documents to Combine")
        .add_filter("PDF Documents", &["pdf"])
        .pick_files();

    match picked {
        Some(paths) if !paths.is_empty() => OpenMultipleFilesDialogResponse {
            canceled: false,
            file_paths: paths
                .into_iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect(),
        },
        _ => OpenMultipleFilesDialogResponse {
            canceled: true,
            file_paths: Vec::new(),
        },
    }
}

pub fn save_file_dialog(default_name: Option<&str>) -> SaveFileDialogResponse {
    let name = default_name.unwrap_or("document.pdf");
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("pdf");

    let mut dialog = FileDialog::new()
        .set_title("Save Document")
        .set_file_name(name);

    if ext.eq_ignore_ascii_case("txt") {
        dialog = dialog.add_filter("Text File", &["txt"]);
    } else if ext.eq_ignore_ascii_case("png") {
        dialog = dialog.add_filter("PNG Image", &["png"]);
    } else {
        dialog = dialog.add_filter("PDF Documents", &["pdf"]);
    }

    match dialog.save_file() {
        Some(path) => SaveFileDialogResponse {
            canceled: false,
            file_path: Some(path.to_string_lossy().to_string()),
        },
        None => SaveFileDialogResponse {
            canceled: true,
            file_path: None,
        },
    }
}

pub fn show_in_folder(file_path: &str) {
    let path = Path::new(file_path);
    if !path.exists() {
        return;
    }
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("explorer.exe")
            .arg(format!("/select,{}", file_path))
            .spawn();
    }
}

pub fn open_mailto(subject: Option<&str>) {
    open_mailto_to(None, subject);
}

pub fn open_mailto_to(email: Option<&str>, subject: Option<&str>) {
    let addr = email.unwrap_or("").trim();
    let subj = subject.unwrap_or("PDF Document").replace(' ', "%20");
    let target = if addr.is_empty() {
        format!("mailto:?subject={}", subj)
    } else {
        format!("mailto:{}?subject={}", addr, subj)
    };
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("cmd")
            .args(["/C", "start", "", &target])
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg(&target).spawn();
    }
}

pub fn open_external_url(url: &str) -> bool {
    let trimmed = url.trim();
    if !(trimmed.starts_with("https://") || trimmed.starts_with("http://")) {
        return false;
    }
    #[cfg(target_os = "windows")]
    {
        return Command::new("cmd")
            .args(["/C", "start", "", trimmed])
            .spawn()
            .is_ok();
    }
    #[cfg(target_os = "macos")]
    {
        return Command::new("open").arg(trimmed).spawn().is_ok();
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        false
    }
}

pub fn pick_update_package() -> serde_json::Value {
    #[cfg(target_os = "windows")]
    let picked = rfd::FileDialog::new()
        .set_title("Select PDFForge Update Package (.exe)")
        .add_filter("PDFForge Installer / Executable", &["exe"])
        .pick_file();
    #[cfg(target_os = "macos")]
    let picked = rfd::FileDialog::new()
        .set_title("Select PDFForge Update Package (.dmg / .app)")
        .add_filter("PDFForge Update Package", &["dmg", "pkg", "zip", "app"])
        .pick_file();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let picked: Option<std::path::PathBuf> = None;

    match picked {
        Some(path) => {
            let file_name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            serde_json::json!({
                "canceled": false,
                "filePath": path.to_string_lossy().to_string(),
                "fileName": file_name
            })
        }
        None => serde_json::json!({
            "canceled": true,
            "filePath": "",
            "fileName": ""
        }),
    }
}

pub fn list_printers() -> Vec<String> {
    let mut printers = vec![
        "Microsoft Print to PDF".to_string(),
        "Microsoft XPS Document Writer".to_string(),
        "Save as PDF (PDFForge)".to_string(),
    ];

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        if let Ok(output) = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "Get-Printer | Select-Object -ExpandProperty Name",
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                for line in text.lines() {
                    let name = line.trim();
                    if !name.is_empty() && !printers.iter().any(|p| p.eq_ignore_ascii_case(name)) {
                        printers.push(name.to_string());
                    }
                }
            }
        }
    }

    printers
}

