use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use ulid::Ulid;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to resolve application data directory")]
    NoDataDir,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RetentionMode {
    #[default]
    Count,
    Days,
    Both,
    Unlimited,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncConfig {
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub prefix: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub salt_b64: String,
    pub derived_key_b64: Option<String>,
    pub auto_sync_secs: u64,
    pub push_latest_to_clipboard: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub device_id: String,
    pub device_name: String,
    pub hotkey_macos: String,
    pub hotkey_windows: String,
    pub paste_on_enter: bool,
    pub show_tray_icon: bool,
    pub retention_mode: RetentionMode,
    pub max_history_items: usize,
    pub max_history_days: u32,
    pub max_image_bytes: usize,
    pub ignored_apps: Vec<String>,
    pub sync: Option<SyncConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device_id: Ulid::new().to_string(),
            device_name: default_device_name(),
            hotkey_macos: "Control+Space".to_string(),
            hotkey_windows: "Alt+Backquote".to_string(),
            paste_on_enter: true,
            show_tray_icon: true,
            retention_mode: RetentionMode::Count,
            max_history_items: 10_000,
            max_history_days: 90,
            max_image_bytes: 5 * 1024 * 1024,
            ignored_apps: vec![
                "Bitwarden".to_string(),
                "1Password".to_string(),
                "KeePassXC".to_string(),
                "Keychain Access".to_string(),
                "LastPass".to_string(),
                "Bitwarden.exe".to_string(),
                "1Password.exe".to_string(),
                "KeePassXC.exe".to_string(),
            ],
            sync: None,
        }
    }
}

fn default_device_name() -> String {
    for key in ["COMPUTERNAME", "HOSTNAME", "HOST"] {
        if let Ok(val) = std::env::var(key) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = std::process::Command::new("scutil")
            .args(["--get", "ComputerName"])
            .output()
            && out.status.success()
        {
            let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !name.is_empty() {
                return name;
            }
        }
        "mac".to_string()
    }
    #[cfg(not(target_os = "macos"))]
    {
        "desktop".to_string()
    }
}

pub fn default_data_dir() -> Result<PathBuf, ConfigError> {
    if let Ok(override_dir) = std::env::var("KLIP_DATA_DIR") {
        let p = PathBuf::from(override_dir);
        if !p.as_os_str().is_empty() {
            return Ok(p);
        }
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").ok_or(ConfigError::NoDataDir)?;
        Ok(PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("klip"))
    }
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var_os("APPDATA").ok_or(ConfigError::NoDataDir)?;
        Ok(PathBuf::from(appdata).join("klip"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            return Ok(PathBuf::from(xdg).join("klip"));
        }
        let home = std::env::var_os("HOME").ok_or(ConfigError::NoDataDir)?;
        Ok(PathBuf::from(home).join(".local/share/klip"))
    }
}

pub fn load_or_create_config(data_dir: &Path) -> Result<Config, ConfigError> {
    fs::create_dir_all(data_dir)?;
    let path = data_dir.join("config.json");
    if path.exists() {
        let raw = fs::read_to_string(&path)?;
        let cfg: Config = serde_json::from_str(&raw)?;
        Ok(cfg)
    } else {
        let cfg = Config::default();
        save_config(data_dir, &cfg)?;
        Ok(cfg)
    }
}

pub fn save_config(data_dir: &Path, cfg: &Config) -> Result<(), ConfigError> {
    fs::create_dir_all(data_dir)?;
    let path = data_dir.join("config.json");
    let json = serde_json::to_string_pretty(cfg)?;
    fs::write(&path, format!("{json}\n"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
