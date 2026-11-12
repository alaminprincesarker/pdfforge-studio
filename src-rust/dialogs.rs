use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
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

pub fn open_multiple_files_dialog_titled(title: Option<&str>) -> OpenMultipleFilesDialogResponse {
    let dialog_title = title
        .filter(|t| !t.trim().is_empty())
        .unwrap_or("Select PDF Documents to Combine");
    let picked = FileDialog::new()
        .set_title(dialog_title)
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

pub fn open_multiple_files_dialog() -> OpenMultipleFilesDialogResponse {
    open_multiple_files_dialog_titled(None)
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
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let ps = format!("Start-Process -FilePath '{}'", target.replace('\'', "''"));
        let _ = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", &ps])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg(&target).spawn();
    }
}

pub fn open_external_url(url: &str) -> bool {
    let trimmed = url.trim();
    if !(trimmed.starts_with("https://")
        || trimmed.starts_with("http://")
        || trimmed.starts_with("mailto:"))
    {
        return false;
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        // Use Start-Process via PowerShell so URLs containing '&', '?', '#', or spaces
        // open reliably in the default browser without cmd.exe shell parsing issues.
        let ps = format!("Start-Process -FilePath '{}'", trimmed.replace('\'', "''"));
        return Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", &ps])
            .creation_flags(CREATE_NO_WINDOW)
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
    let mut printers: Vec<String> = Vec::new();

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let ps = "$def = (Get-CimInstance Win32_Printer -ErrorAction SilentlyContinue | Where-Object { $_.Default } | Select-Object -First 1 -ExpandProperty Name); if ($def) { Write-Output $def }; Get-Printer -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Name";
        if let Ok(output) = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", ps])
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

    for fallback in [
        "Microsoft Print to PDF",
        "Microsoft XPS Document Writer",
        "Save as PDF (PDFForge)",
    ] {
        if !printers.iter().any(|p| p.eq_ignore_ascii_case(fallback)) {
            printers.push(fallback.to_string());
        }
    }

    printers
}

/// Prints pre-rendered PNG page images directly to the selected OS printer
/// using a headless print controller so no secondary OS print dialog appears.
pub fn print_images_direct(
    printer_name: &str,
    copies: u16,
    orientation: &str,
    paper_size: &str,
    output_pdf_path: Option<&str>,
    pages: &[String],
) -> Result<(), String> {
    if pages.is_empty() {
        return Err("No pages provided to print.".to_string());
    }

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let tmp_dir = std::env::temp_dir().join(format!("pdfforge_print_{}_{}", std::process::id(), stamp));
    std::fs::create_dir_all(&tmp_dir)
        .map_err(|e| format!("Failed to prepare temporary print spool folder: {}", e))?;

    let mut img_paths: Vec<std::path::PathBuf> = Vec::with_capacity(pages.len());
    for (idx, item) in pages.iter().enumerate() {
        let b64 = item
            .split_once("base64,")
            .map(|(_, rhs)| rhs)
            .unwrap_or(item.as_str())
            .trim();
        let bytes = BASE64
            .decode(b64)
            .map_err(|e| {
                let _ = std::fs::remove_dir_all(&tmp_dir);
                format!("Invalid rendered page image #{}: {}", idx + 1, e)
            })?;
        let p = tmp_dir.join(format!("page_{:04}.png", idx + 1));
        if let Err(e) = std::fs::write(&p, &bytes) {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(format!("Failed writing print spool page #{}: {}", idx + 1, e));
        }
        img_paths.push(p);
    }

    let copies_clamped = copies.clamp(1, 50);

    #[cfg(target_os = "windows")]
    let res = {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        let list_file = tmp_dir.join("pages.txt");
        let list_body = img_paths
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("\r\n");
        if let Err(e) = std::fs::write(&list_file, list_body) {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(format!("Failed writing print manifest: {}", e));
        }

        let ps1_path = tmp_dir.join("run_print.ps1");
        let out_pdf = output_pdf_path.unwrap_or("").trim().replace('\'', "''");
        let ps1_script = format!(
            r#"
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$files = Get-Content -LiteralPath '{list_path}' | Where-Object {{ $_ -and (Test-Path -LiteralPath $_) }}
if (-not $files -or $files.Count -eq 0) {{ throw 'No print images staged.' }}
$pd = New-Object System.Drawing.Printing.PrintDocument
$pd.DocumentName = 'PDFForge Studio Document'
$pd.PrintController = New-Object System.Drawing.Printing.StandardPrintController
$targetPrinter = '{printer}'
if ($targetPrinter -and $targetPrinter.Length -gt 0) {{
    $pd.PrinterSettings.PrinterName = $targetPrinter
}}
if (-not $pd.PrinterSettings.IsValid) {{
    throw ('Selected printer is not valid or unavailable: ' + $targetPrinter)
}}
$pd.PrinterSettings.Copies = [int16]{copies}
$outPdf = '{out_pdf}'
if ($outPdf -and $outPdf.Length -gt 0) {{
    $pd.PrinterSettings.PrintToFile = $true
    $pd.PrinterSettings.PrintFileName = $outPdf
}}
$paperKey = '{paper}'
if ($paperKey) {{
    foreach ($ps in $pd.PrinterSettings.PaperSizes) {{
        $n = $ps.PaperName.ToLowerInvariant()
        if (($paperKey -eq 'letter' -and $n -match 'letter') -or
            ($paperKey -eq 'a4' -and $n -match '\ba4\b') -or
            ($paperKey -eq 'legal' -and $n -match 'legal') -or
            ($paperKey -eq 'tabloid' -and ($n -match 'tabloid' -or $n -match '11\s*x\s*17')) -or
            ($paperKey -eq 'a3' -and $n -match '\ba3\b')) {{
            $pd.DefaultPageSettings.PaperSize = $ps
            break
        }}
    }}
}}
$orientMode = '{orient}'
$script:idx = 0
$pd.add_QueryPageSettings({{
    param($sender, $e)
    if ($script:idx -lt $files.Count) {{
        $img = [System.Drawing.Image]::FromFile($files[$script:idx])
        try {{
            if ($orientMode -eq 'landscape') {{
                $e.PageSettings.Landscape = $true
            }} elseif ($orientMode -eq 'portrait') {{
                $e.PageSettings.Landscape = $false
            }} else {{
                $e.PageSettings.Landscape = ($img.Width -gt $img.Height)
            }}
        }} finally {{
            $img.Dispose()
        }}
    }}
}})
$pd.add_PrintPage({{
    param($sender, $e)
    $path = $files[$script:idx]
    $img = [System.Drawing.Image]::FromFile($path)
    try {{
        $bounds = $e.PageBounds
        $e.Graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
        $e.Graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
        $e.Graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
        $e.Graphics.FillRectangle([System.Drawing.Brushes]::White, $bounds)
        $scale = [Math]::Min($bounds.Width / [double]$img.Width, $bounds.Height / [double]$img.Height)
        $drawW = [float]($img.Width * $scale)
        $drawH = [float]($img.Height * $scale)
        $drawX = [float]($bounds.Left + ($bounds.Width - $drawW) / 2.0)
        $drawY = [float]($bounds.Top + ($bounds.Height - $drawH) / 2.0)
        $e.Graphics.DrawImage($img, $drawX, $drawY, $drawW, $drawH)
    }} finally {{
        $img.Dispose()
    }}
    $script:idx++
    $e.HasMorePages = ($script:idx -lt $files.Count)
}})
try {{
    $pd.Print()
}} finally {{
    $pd.Dispose()
}}
"#,
            list_path = list_file.to_string_lossy().replace('\'', "''"),
            printer = printer_name.trim().replace('\'', "''"),
            copies = copies_clamped,
            out_pdf = out_pdf,
            paper = paper_size.trim().to_ascii_lowercase().replace('\'', "''"),
            orient = orientation.trim().to_ascii_lowercase().replace('\'', "''"),
        );

        if let Err(e) = std::fs::write(&ps1_path, ps1_script) {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(format!("Failed preparing print helper: {}", e));
        }

        let output = Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                &ps1_path.to_string_lossy(),
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| format!("Failed launching print spooler helper: {}", e));

        match output {
            Ok(out) if out.status.success() => Ok(()),
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                Err(if err.is_empty() {
                    "Printer driver rejected print job.".to_string()
                } else {
                    err
                })
            }
            Err(e) => Err(e),
        }
    };

    #[cfg(target_os = "macos")]
    let res = {
        let mut cmd = Command::new("lp");
        if !printer_name.trim().is_empty() && !printer_name.contains("PDFForge") {
            cmd.args(["-d", printer_name.trim()]);
        }
        cmd.args(["-n", &copies_clamped.to_string()]);
        for p in &img_paths {
            cmd.arg(p);
        }
        match cmd.output() {
            Ok(out) if out.status.success() => Ok(()),
            Ok(out) => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
            Err(e) => Err(format!("Failed invoking lp print command: {}", e)),
        }
    };

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let res = {
        let _ = (printer_name, copies, orientation, paper_size, output_pdf_path);
        Err("Direct printing is supported on Windows and macOS.".to_string())
    };

    let _ = std::fs::remove_dir_all(&tmp_dir);
    res
}

