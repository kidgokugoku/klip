use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use clipboard_rs::{Clipboard, ClipboardContext, RustImageData, common::RustImage};
use klip_core::config::{Config, RetentionMode, SyncConfig, save_config};
use klip_core::crypto;
use klip_core::maccy::import_from_maccy;
use klip_core::model::{ClipDetail, ClipItem, ClipKind, ClipQuery, MaccyImportReport, SyncReport};
use klip_core::storage::ClipStore;
use klip_core::sync::S3SyncClient;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

use crate::platform::{self, PreviousFocus};

pub struct AppState {
    pub data_dir: PathBuf,
    pub config: Mutex<Config>,
    pub store: Mutex<ClipStore>,
    pub previous_focus: Mutex<Option<PreviousFocus>>,
    /// Hash of the content we most recently wrote to the system clipboard so the watcher
    /// ignores our own echoes / remote-applied updates without uploading duplicates.
    pub suppress_hash: Mutex<Option<String>>,
    pub last_sync_error: Mutex<Option<String>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatusInfo {
    pub enabled: bool,
    pub bucket: Option<String>,
    pub endpoint: Option<String>,
    pub prefix: Option<String>,
    pub access_key_id: Option<String>,
    pub auto_sync_secs: u64,
    pub push_latest_to_clipboard: bool,
    pub total_items: usize,
    pub pending_events: usize,
    pub last_synced_at: Option<i64>,
    pub last_error: Option<String>,
    pub hotkey: String,
    pub device_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupSyncRequest {
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub prefix: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub passphrase: String,
    pub auto_sync_secs: u64,
    pub push_latest_to_clipboard: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub paste_on_enter: bool,
    pub show_tray_icon: bool,
    pub retention_mode: RetentionMode,
    pub max_history_items: usize,
    pub max_history_days: u32,
    pub max_image_mb: usize,
    pub hotkey: String,
    pub ignored_apps: Vec<String>,
}

fn settings_from_config(cfg: &Config) -> AppSettings {
    #[cfg(target_os = "macos")]
    let hotkey = cfg.hotkey_macos.clone();
    #[cfg(not(target_os = "macos"))]
    let hotkey = cfg.hotkey_windows.clone();

    AppSettings {
        paste_on_enter: cfg.paste_on_enter,
        show_tray_icon: cfg.show_tray_icon,
        retention_mode: cfg.retention_mode,
        max_history_items: cfg.max_history_items,
        max_history_days: cfg.max_history_days,
        max_image_mb: (cfg.max_image_bytes / (1024 * 1024)).max(1),
        hotkey,
        ignored_apps: cfg.ignored_apps.clone(),
    }
}

#[tauri::command]
pub fn get_settings(state: State<'_, Arc<AppState>>) -> Result<AppSettings, String> {
    let cfg = state.config.lock().map_err(|e| e.to_string())?;
    Ok(settings_from_config(&cfg))
}

#[tauri::command]
pub fn update_settings(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    settings: AppSettings,
) -> Result<AppSettings, String> {
    let new_hotkey = settings.hotkey.trim().to_string();
    let new_shortcut = new_hotkey
        .parse::<Shortcut>()
        .map_err(|e| format!("invalid hotkey '{new_hotkey}': {e}"))?;

    let (old_hotkey, retention_mode, max_items, max_days, updated) = {
        let mut cfg = state.config.lock().map_err(|e| e.to_string())?;

        #[cfg(target_os = "macos")]
        let old_hotkey = std::mem::replace(&mut cfg.hotkey_macos, new_hotkey.clone());
        #[cfg(not(target_os = "macos"))]
        let old_hotkey = std::mem::replace(&mut cfg.hotkey_windows, new_hotkey.clone());

        cfg.paste_on_enter = settings.paste_on_enter;
        cfg.show_tray_icon = settings.show_tray_icon;
        cfg.retention_mode = settings.retention_mode;
        cfg.max_history_items = settings.max_history_items.max(1);
        cfg.max_history_days = settings.max_history_days.max(1);
        cfg.max_image_bytes = settings.max_image_mb.clamp(1, 100) * 1024 * 1024;
        cfg.ignored_apps = settings
            .ignored_apps
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        save_config(&state.data_dir, &cfg).map_err(|e| e.to_string())?;
        (
            old_hotkey,
            cfg.retention_mode,
            cfg.max_history_items,
            cfg.max_history_days,
            settings_from_config(&cfg),
        )
    };

    if let Some(tray) = app.tray_by_id("klip-tray") {
        let _ = tray.set_visible(updated.show_tray_icon);
    }

    if old_hotkey != new_hotkey {
        let gs = app.global_shortcut();
        if let Ok(old_sc) = old_hotkey.parse::<Shortcut>() {
            let _ = gs.unregister(old_sc);
        }
        gs.register(new_shortcut)
            .map_err(|e| format!("failed to register hotkey '{new_hotkey}': {e}"))?;
    }

    {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        store
            .prune(retention_mode, max_items, max_days)
            .map_err(|e| e.to_string())?;
    }
    let _ = app.emit("klip://updated", ());
    Ok(updated)
}

#[tauri::command]
pub fn query_items(state: State<'_, Arc<AppState>>, query: ClipQuery) -> Result<Vec<ClipItem>, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    store.query_items(&query).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_item_detail(state: State<'_, Arc<AppState>>, id: String) -> Result<Option<ClipDetail>, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    store.get_detail(&id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn hide_panel(app: AppHandle) -> Result<(), String> {
    platform::hide_main_panel(&app);
    Ok(())
}

#[tauri::command]
pub fn copy_item(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
    paste_after: bool,
) -> Result<(), String> {
    let (item, text_content, image_bytes) = {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        let Some((item, text_content)) = store.get_by_id(&id).map_err(|e| e.to_string())? else {
            return Err(format!("item not found: {id}"));
        };
        let image_bytes = if item.kind == ClipKind::Image {
            store.read_image_bytes(&item).map_err(|e| e.to_string())?
        } else {
            None
        };
        (item, text_content, image_bytes)
    };

    {
        let mut guard = state.suppress_hash.lock().map_err(|e| e.to_string())?;
        *guard = Some(item.content_hash.clone());
    }

    let ctx = ClipboardContext::new().map_err(|e| e.to_string())?;
    match item.kind {
        ClipKind::Image => {
            let bytes = image_bytes.ok_or_else(|| "image blob missing on disk".to_string())?;
            let img = RustImageData::from_bytes(&bytes).map_err(|e| e.to_string())?;
            ctx.set_image(img).map_err(|e| e.to_string())?;
        }
        _ => {
            let text = text_content.unwrap_or_else(|| item.title.clone());
            ctx.set_text(text).map_err(|e| e.to_string())?;
        }
    }

    let prev = state
        .previous_focus
        .lock()
        .ok()
        .and_then(|mut g| g.take());
    platform::hide_main_panel(&app);

    if paste_after {
        std::thread::spawn(move || {
            platform::restore_focus_and_paste(prev);
        });
    }
    Ok(())
}

#[tauri::command]
pub fn toggle_pin(state: State<'_, Arc<AppState>>, id: String) -> Result<bool, String> {
    let cfg = state.config.lock().map_err(|e| e.to_string())?.clone();
    let mut store = state.store.lock().map_err(|e| e.to_string())?;
    let pinned = store
        .toggle_pin(&id, &cfg.device_id, &cfg.device_name, cfg.sync.is_some())
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("item not found: {id}"))?;
    Ok(pinned)
}

#[tauri::command]
pub fn delete_item(state: State<'_, Arc<AppState>>, id: String) -> Result<bool, String> {
    let cfg = state.config.lock().map_err(|e| e.to_string())?.clone();
    let mut store = state.store.lock().map_err(|e| e.to_string())?;
    store
        .delete_item(&id, &cfg.device_id, &cfg.device_name, cfg.sync.is_some())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn clear_unpinned(state: State<'_, Arc<AppState>>) -> Result<usize, String> {
    let cfg = state.config.lock().map_err(|e| e.to_string())?.clone();
    let mut store = state.store.lock().map_err(|e| e.to_string())?;
    store
        .clear_unpinned(&cfg.device_id, &cfg.device_name, cfg.sync.is_some())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn import_maccy(state: State<'_, Arc<AppState>>) -> Result<MaccyImportReport, String> {
    let cfg = state.config.lock().map_err(|e| e.to_string())?.clone();
    let mut store = state.store.lock().map_err(|e| e.to_string())?;
    import_from_maccy(
        &mut store,
        None,
        &cfg.device_id,
        &cfg.device_name,
        cfg.sync.is_some(),
        cfg.max_image_bytes,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_sync_status(state: State<'_, Arc<AppState>>) -> Result<SyncStatusInfo, String> {
    let cfg = state.config.lock().map_err(|e| e.to_string())?.clone();
    let store = state.store.lock().map_err(|e| e.to_string())?;
    let (total_items, pending_events) = store.stats().map_err(|e| e.to_string())?;
    let last_synced_at = store
        .get_meta("last_synced_at")
        .map_err(|e| e.to_string())?
        .and_then(|s| s.parse::<i64>().ok());
    let last_error = state.last_sync_error.lock().ok().and_then(|g| g.clone());

    #[cfg(target_os = "macos")]
    let hotkey = cfg.hotkey_macos.clone();
    #[cfg(not(target_os = "macos"))]
    let hotkey = cfg.hotkey_windows.clone();

    Ok(SyncStatusInfo {
        enabled: cfg.sync.is_some(),
        bucket: cfg.sync.as_ref().map(|s| s.bucket.clone()),
        endpoint: cfg.sync.as_ref().map(|s| s.endpoint.clone()),
        prefix: cfg.sync.as_ref().map(|s| s.prefix.clone()),
        access_key_id: cfg.sync.as_ref().map(|s| s.access_key_id.clone()),
        auto_sync_secs: cfg.sync.as_ref().map(|s| s.auto_sync_secs).unwrap_or(30),
        push_latest_to_clipboard: cfg
            .sync
            .as_ref()
            .map(|s| s.push_latest_to_clipboard)
            .unwrap_or(true),
        total_items,
        pending_events,
        last_synced_at,
        last_error,
        hotkey,
        device_name: cfg.device_name,
    })
}

#[tauri::command]
pub fn setup_sync(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    req: SetupSyncRequest,
) -> Result<SyncReport, String> {
    let salt = crypto::deterministic_vault_salt(&format!("{}/{}", req.bucket, req.prefix));
    let derived = crypto::derive_key(&req.passphrase, &salt).map_err(|e| e.to_string())?;
    let sync_cfg = SyncConfig {
        endpoint: req.endpoint.trim().to_string(),
        bucket: req.bucket.trim().to_string(),
        region: req.region.trim().to_string(),
        prefix: req.prefix.trim().to_string(),
        access_key_id: req.access_key_id.trim().to_string(),
        secret_access_key: req.secret_access_key.trim().to_string(),
        salt_b64: crypto::encode_bytes(&salt),
        derived_key_b64: Some(crypto::encode_bytes(&derived)),
        auto_sync_secs: req.auto_sync_secs.max(5),
        push_latest_to_clipboard: req.push_latest_to_clipboard,
    };

    let client = S3SyncClient::from_config(&sync_cfg, None).map_err(|e| e.to_string())?;
    let device_id = state
        .config
        .lock()
        .map_err(|e| e.to_string())?
        .device_id
        .clone();

    let report = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        client
            .sync_now(&mut store, &device_id)
            .map_err(|e| e.to_string())?
    };

    {
        let mut cfg = state.config.lock().map_err(|e| e.to_string())?;
        cfg.sync = Some(sync_cfg);
        save_config(&state.data_dir, &cfg).map_err(|e| e.to_string())?;
    }
    if let Ok(mut err) = state.last_sync_error.lock() {
        *err = None;
    }
    let _ = app.emit("klip://updated", ());
    Ok(report)
}

#[tauri::command]
pub fn sync_now(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<SyncReport, String> {
    run_sync_cycle(&app, &state)
}

pub fn run_sync_cycle(app: &AppHandle, state: &Arc<AppState>) -> Result<SyncReport, String> {
    let (sync_cfg, device_id) = {
        let cfg = state.config.lock().map_err(|e| e.to_string())?;
        let Some(sync) = cfg.sync.clone() else {
            return Err("sync not configured".into());
        };
        (sync, cfg.device_id.clone())
    };

    let client = S3SyncClient::from_config(&sync_cfg, None).map_err(|e| e.to_string())?;
    let report = {
        let mut store = state.store.lock().map_err(|e| e.to_string())?;
        client.sync_now(&mut store, &device_id)
    };

    match report {
        Ok(rep) => {
            if let Ok(mut err) = state.last_sync_error.lock() {
                *err = None;
            }
            if sync_cfg.push_latest_to_clipboard
                && let Some(hash) = rep.latest_remote_hash.as_deref()
            {
                let _ = push_hash_to_clipboard(state, hash);
            }
            if rep.applied_items > 0 {
                let _ = app.emit("klip://updated", ());
            }
            Ok(rep)
        }
        Err(e) => {
            let msg = e.to_string();
            if let Ok(mut err) = state.last_sync_error.lock() {
                *err = Some(msg.clone());
            }
            Err(msg)
        }
    }
}

fn push_hash_to_clipboard(state: &Arc<AppState>, content_hash: &str) -> Result<(), String> {
    let (kind, text, image_bytes) = {
        let store = state.store.lock().map_err(|e| e.to_string())?;
        let items = store
            .query_items(&ClipQuery {
                limit: Some(25),
                ..Default::default()
            })
            .map_err(|e| e.to_string())?;
        let Some(item) = items.into_iter().find(|i| i.content_hash == content_hash) else {
            return Ok(());
        };
        let detail = store.get_detail(&item.id).map_err(|e| e.to_string())?;
        let text = detail.as_ref().and_then(|d| d.text_content.clone());
        let img = if item.kind == ClipKind::Image {
            store.read_image_bytes(&item).map_err(|e| e.to_string())?
        } else {
            None
        };
        (item.kind, text, img)
    };

    {
        let mut guard = state.suppress_hash.lock().map_err(|e| e.to_string())?;
        *guard = Some(content_hash.to_string());
    }

    let ctx = ClipboardContext::new().map_err(|e| e.to_string())?;
    match kind {
        ClipKind::Image => {
            if let Some(bytes) = image_bytes
                && let Ok(img) = RustImageData::from_bytes(&bytes)
            {
                let _ = ctx.set_image(img);
            }
        }
        _ => {
            if let Some(t) = text {
                let _ = ctx.set_text(t);
            }
        }
    }
    Ok(())
}
