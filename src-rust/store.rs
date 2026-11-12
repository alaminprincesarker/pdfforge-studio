use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentFile {
    pub path: String,
    pub name: String,
    pub last_opened: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_favorite: Option<bool>,
}

fn default_true() -> bool {
    true
}

fn default_false() -> bool {
    false
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub default_zoom: f64,
    pub theme: String,
    pub remember_last_page: bool,
    pub enable_autosave: bool,
    #[serde(default = "default_true")]
    pub auto_check_updates: bool,
    #[serde(default = "default_false")]
    pub auto_install_updates: bool,
    #[serde(default = "default_true")]
    pub auto_fit_on_open: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            default_zoom: 0.0,
            theme: "dark".to_string(),
            remember_last_page: true,
            enable_autosave: true,
            auto_check_updates: true,
            auto_install_updates: false,
            auto_fit_on_open: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartialAppSettings {
    pub default_zoom: Option<f64>,
    pub theme: Option<String>,
    pub remember_last_page: Option<bool>,
    pub enable_autosave: Option<bool>,
    pub auto_check_updates: Option<bool>,
    pub auto_install_updates: Option<bool>,
    pub auto_fit_on_open: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileState {
    pub last_page: u32,
    pub last_zoom: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_rotation: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureItem {
    pub id: String,
    pub data_url: String,
}

pub struct AppDataStore {
    user_data_dir: PathBuf,
    settings_file: PathBuf,
    recent_files_file: PathBuf,
    file_states_file: PathBuf,
    signatures_dir: PathBuf,
}

impl AppDataStore {
    pub fn new() -> Self {
        let app_data = env::var("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| env::temp_dir());
        let user_data_dir = app_data.join("pdfforge");
        let _ = fs::create_dir_all(&user_data_dir);

        let settings_file = user_data_dir.join("settings.json");
        let recent_files_file = user_data_dir.join("recent-files.json");
        let file_states_file = user_data_dir.join("file-states.json");
        let signatures_dir = user_data_dir.join("signatures");
        let autosave_dir = user_data_dir.join("autosave");

        let _ = fs::create_dir_all(&signatures_dir);
        let _ = fs::create_dir_all(&autosave_dir);

        Self {
            user_data_dir,
            settings_file,
            recent_files_file,
            file_states_file,
            signatures_dir,
        }
    }

    pub fn user_data_dir(&self) -> &PathBuf {
        &self.user_data_dir
    }

    pub fn get_settings(&self) -> AppSettings {
        if let Ok(raw) = fs::read_to_string(&self.settings_file) {
            if let Ok(parsed) = serde_json::from_str::<AppSettings>(&raw) {
                return parsed;
            }
        }
        AppSettings::default()
    }

    pub fn update_settings(&self, partial: PartialAppSettings) -> AppSettings {
        let mut current = self.get_settings();
        if let Some(zoom) = partial.default_zoom {
            current.default_zoom = zoom;
        }
        if let Some(theme) = partial.theme {
            current.theme = theme;
        }
        if let Some(rem) = partial.remember_last_page {
            current.remember_last_page = rem;
        }
        if let Some(auto) = partial.enable_autosave {
            current.enable_autosave = auto;
        }
        if let Some(auto_upd) = partial.auto_check_updates {
            current.auto_check_updates = auto_upd;
        }
        if let Some(auto_inst) = partial.auto_install_updates {
            current.auto_install_updates = auto_inst;
        }
        if let Some(auto_fit) = partial.auto_fit_on_open {
            current.auto_fit_on_open = auto_fit;
        }
        if let Ok(json) = serde_json::to_string_pretty(&current) {
            let _ = fs::write(&self.settings_file, json);
        }
        current
    }

    pub fn get_recent_files(&self) -> Vec<RecentFile> {
        if let Ok(raw) = fs::read_to_string(&self.recent_files_file) {
            if let Ok(list) = serde_json::from_str::<Vec<RecentFile>>(&raw) {
                return list;
            }
        }
        Vec::new()
    }

    pub fn add_recent_file(&self, mut file: RecentFile) -> Vec<RecentFile> {
        let mut list = self.get_recent_files();
        let target_lower = file.path.to_lowercase();

        // Preserve existing favorite flag if present
        let existing_fav = list
            .iter()
            .find(|item| item.path.to_lowercase() == target_lower)
            .and_then(|item| item.is_favorite);

        list.retain(|item| item.path.to_lowercase() != target_lower);

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(file.last_opened);

        file.last_opened = now_ms;
        if file.is_favorite.is_none() {
            file.is_favorite = existing_fav;
        }

        list.insert(0, file);
        if list.len() > 50 {
            list.truncate(50);
        }

        if let Ok(json) = serde_json::to_string_pretty(&list) {
            let _ = fs::write(&self.recent_files_file, json);
        }
        list
    }

    pub fn remove_recent_file(&self, file_path: &str) -> Vec<RecentFile> {
        let mut list = self.get_recent_files();
        let target_lower = file_path.to_lowercase();
        list.retain(|item| item.path.to_lowercase() != target_lower);
        if let Ok(json) = serde_json::to_string_pretty(&list) {
            let _ = fs::write(&self.recent_files_file, json);
        }
        list
    }

    pub fn toggle_favorite_file(&self, file_path: &str) -> Vec<RecentFile> {
        let mut list = self.get_recent_files();
        let target_lower = file_path.to_lowercase();
        for item in &mut list {
            if item.path.to_lowercase() == target_lower {
                let current = item.is_favorite.unwrap_or(false);
                item.is_favorite = Some(!current);
                break;
            }
        }
        if let Ok(json) = serde_json::to_string_pretty(&list) {
            let _ = fs::write(&self.recent_files_file, json);
        }
        list
    }

    pub fn clear_recent_files(&self) {
        let empty: Vec<RecentFile> = Vec::new();
        if let Ok(json) = serde_json::to_string_pretty(&empty) {
            let _ = fs::write(&self.recent_files_file, json);
        }
    }

    pub fn get_file_state(&self, file_path: &str) -> Option<FileState> {
        if let Ok(raw) = fs::read_to_string(&self.file_states_file) {
            if let Ok(map) = serde_json::from_str::<HashMap<String, FileState>>(&raw) {
                return map.get(&file_path.to_lowercase()).cloned();
            }
        }
        None
    }

    pub fn save_file_state(&self, file_path: &str, state: FileState) {
        let mut map: HashMap<String, FileState> = fs::read_to_string(&self.file_states_file)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();

        map.insert(file_path.to_lowercase(), state);
        if let Ok(json) = serde_json::to_string_pretty(&map) {
            let _ = fs::write(&self.file_states_file, json);
        }
    }

    pub fn list_signatures(&self) -> Vec<SignatureItem> {
        let mut items = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.signatures_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("png") {
                    if let Ok(bytes) = fs::read(&path) {
                        let id = path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("sig")
                            .to_string();
                        let encoded = BASE64.encode(&bytes);
                        items.push(SignatureItem {
                            id,
                            data_url: format!("data:image/png;base64,{}", encoded),
                        });
                    }
                }
            }
        }
        items
    }

    pub fn save_signature(&self, id: &str, base64_png: &str) -> bool {
        let clean_id: String = id
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if clean_id.is_empty() {
            return false;
        }
        let raw_b64 = base64_png
            .split(',')
            .nth(1)
            .unwrap_or(base64_png);
        if let Ok(bytes) = BASE64.decode(raw_b64) {
            let target = self.signatures_dir.join(format!("{}.png", clean_id));
            return fs::write(target, bytes).is_ok();
        }
        false
    }

    pub fn delete_signature(&self, id: &str) -> bool {
        let clean_id: String = id
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        let target = self.signatures_dir.join(format!("{}.png", clean_id));
        if target.exists() {
            let _ = fs::remove_file(target);
        }
        true
    }
}

