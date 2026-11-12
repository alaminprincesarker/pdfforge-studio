use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveFileResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub struct RawPdfFile {
    pub bytes: Vec<u8>,
    pub file_name: String,
    pub size: u64,
}

pub fn read_pdf_file(file_path: &str) -> Result<RawPdfFile, String> {
    let path = Path::new(file_path);
    if !path.exists() {
        return Err(format!("File does not exist: {}", file_path));
    }
    let bytes = fs::read(path).map_err(|e| format!("Failed to read file: {}", e))?;
    let size = bytes.len() as u64;
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "document.pdf".to_string());

    Ok(RawPdfFile {
        bytes,
        file_name,
        size,
    })
}

pub fn save_pdf_file(file_path: &str, data: &[u8]) -> SaveFileResponse {
    match fs::write(file_path, data) {
        Ok(_) => SaveFileResponse {
            success: true,
            error: None,
        },
        Err(err) => SaveFileResponse {
            success: false,
            error: Some(err.to_string()),
        },
    }
}

fn normalize_cli_path_arg(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_matches('"');
    if trimmed.is_empty() || trimmed.starts_with("--") || trimmed.starts_with('-') {
        return None;
    }
    let decoded = if let Some(rest) = trimmed.strip_prefix("file:///") {
        let bytes = rest.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                if let (Some(h), Some(l)) = (
                    (bytes[i + 1] as char).to_digit(16),
                    (bytes[i + 2] as char).to_digit(16),
                ) {
                    out.push(((h << 4) | l) as u8);
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8_lossy(&out).replace('/', "\\")
    } else {
        trimmed.to_string()
    };
    if !decoded.to_lowercase().ends_with(".pdf") {
        return None;
    }
    let path = PathBuf::from(&decoded);
    if !path.exists() {
        return None;
    }
    Some(if let Ok(abs) = fs::canonicalize(&path) {
        let s = abs.to_string_lossy().to_string();
        s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
    } else {
        decoded
    })
}

pub fn parse_cli_pdf_args() -> Vec<String> {
    let mut results = Vec::new();
    for arg in env::args().skip(1) {
        if let Some(normalized) = normalize_cli_path_arg(&arg) {
            if !results
                .iter()
                .any(|p: &String| p.eq_ignore_ascii_case(&normalized))
            {
                results.push(normalized);
            }
        }
    }
    results
}

#[allow(dead_code)]
pub fn parse_cli_pdf_arg() -> Option<String> {
    parse_cli_pdf_args().into_iter().next()
}

