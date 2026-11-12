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

pub fn parse_cli_pdf_args() -> Vec<String> {
    let mut results = Vec::new();
    for arg in env::args().skip(1) {
        if !arg.starts_with("--") && arg.to_lowercase().ends_with(".pdf") {
            let path = PathBuf::from(&arg);
            if path.exists() {
                let normalized = if let Ok(abs) = fs::canonicalize(&path) {
                    let s = abs.to_string_lossy().to_string();
                    // Strip Windows extended-length path prefix \\?\ if present
                    s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
                } else {
                    arg
                };
                if !results
                    .iter()
                    .any(|p: &String| p.eq_ignore_ascii_case(&normalized))
                {
                    results.push(normalized);
                }
            }
        }
    }
    results
}

#[allow(dead_code)]
pub fn parse_cli_pdf_arg() -> Option<String> {
    parse_cli_pdf_args().into_iter().next()
}

