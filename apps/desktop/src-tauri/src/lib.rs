mod commands;
mod platform;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use clipboard_rs::{
    Clipboard, ClipboardContext, ClipboardHandler, ClipboardWatcher, ClipboardWatcherContext,
    ContentFormat, common::RustImage,
};
use klip_core::config::{default_data_dir, load_or_create_config};
use klip_core::model::{ClipKind, NewClipInput};
use klip_core::privacy::{has_sensitive_marker, is_ignored_app};
use klip_core::storage::{ClipStore, sha256_hex};
use tauri::{
    AppHandle, Emitter,
    menu::{MenuBuilder, MenuItemBuilder},
    tray::TrayIconBuilder,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use crate::commands::AppState;

struct WatcherHandler {
    app: AppHandle,
    state: Arc<AppState>,
    ctx: ClipboardContext,
}

impl ClipboardHandler for WatcherHandler {
    fn on_clipboard_change(&mut self) {
        let formats = self.ctx.available_formats().unwrap_or_default();
        if has_sensitive_marker(&formats) {
            return;
        }

        let source_app = platform::active_app_name();
        let cfg = {
            let Ok(g) = self.state.config.lock() else {
                return;
            };
            g.clone()
        };
        if is_ignored_app(source_app.as_deref(), &cfg.ignored_apps) {
            return;
        }

        let input = if self.ctx.has(ContentFormat::Text) {
            let Ok(text) = self.ctx.get_text() else {
                return;
            };
            if text.trim().is_empty() {
                return;
            }
            let hash = sha256_hex(text.as_bytes());
            if is_suppressed(&self.state, &hash) {
                return;
            }
            NewClipInput {
                kind: ClipKind::Text,
                text: Some(text),
                png_bytes: None,
                source_app,
                copied_at_ms: None,
                pinned: false,
                copy_count: None,
            }
        } else if self.ctx.has(ContentFormat::Image) {
            let Ok(img) = self.ctx.get_image() else {
                return;
            };
            let Ok(png) = img.to_png() else {
                return;
            };
            let bytes = png.get_bytes().to_vec();
            if bytes.is_empty() || bytes.len() > cfg.max_image_bytes {
                return;
            }
            let hash = sha256_hex(&bytes);
            if is_suppressed(&self.state, &hash) {
                return;
            }
            NewClipInput {
                kind: ClipKind::Image,
                text: None,
                png_bytes: Some(bytes),
                source_app,
                copied_at_ms: None,
                pinned: false,
                copy_count: None,
            }
        } else {
            return;
        };

        let saved = {
            let Ok(mut store) = self.state.store.lock() else {
                return;
            };
            let res = store.capture_local(
                input,
                &cfg.device_id,
                &cfg.device_name,
                cfg.sync.is_some(),
            );
            let _ = store.prune(
                cfg.retention_mode,
                cfg.max_history_items,
                cfg.max_history_days,
            );
            res
        };

        if matches!(saved, Ok(Some(_))) {
            let _ = self.app.emit("klip://updated", ());
        }
    }
}

fn is_suppressed(state: &AppState, hash: &str) -> bool {
    let Ok(mut guard) = state.suppress_hash.lock() else {
        return false;
    };
    if guard.as_deref() == Some(hash) {
        *guard = None;
        true
    } else {
        false
    }
}

fn start_clipboard_watcher(app: AppHandle, state: Arc<AppState>) {
    std::thread::spawn(move || {
        let Ok(ctx) = ClipboardContext::new() else {
            return;
        };
        let Ok(mut watcher) = ClipboardWatcherContext::new() else {
            return;
        };
        watcher.add_handler(WatcherHandler { app, state, ctx });
        watcher.start_watch();
    });
}

fn start_sync_loop(app: AppHandle, state: Arc<AppState>) {
    std::thread::spawn(move || {
        loop {
            let interval_secs = {
                let guard = state.config.lock().ok();
                guard
                    .and_then(|c| c.sync.as_ref().map(|s| s.auto_sync_secs.max(5)))
                    .unwrap_or(30)
            };
            let enabled = state
                .config
                .lock()
                .ok()
                .is_some_and(|c| c.sync.is_some());
            if enabled {
                let _ = commands::run_sync_cycle(&app, &state);
            }
            std::thread::sleep(Duration::from_secs(interval_secs));
        }
    });
}

fn setup_tray(app: &AppHandle, state: Arc<AppState>, visible: bool) -> tauri::Result<()> {
    let show_item = MenuItemBuilder::with_id("show", "Open Klip").build(app)?;
    let sync_item = MenuItemBuilder::with_id("sync", "Sync R2 Now").build(app)?;
    let quit_item = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
    let menu = MenuBuilder::new(app)
        .items(&[&show_item, &sync_item, &quit_item])
        .build()?;

    let mut builder = TrayIconBuilder::with_id("klip-tray").menu(&menu).tooltip("Klip");
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }

    let tray = builder
        .on_menu_event(move |app_handle, event| match event.id().as_ref() {
            "show" => platform::show_main_panel(app_handle, &state),
            "sync" => {
                let st = state.clone();
                let ah = app_handle.clone();
                std::thread::spawn(move || {
                    let _ = commands::run_sync_cycle(&ah, &st);
                });
            }
            "quit" => app_handle.exit(0),
            _ => {}
        })
        .build(app)?;

    let _ = tray.set_visible(visible);
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let data_dir = default_data_dir().expect("failed to resolve klip data dir");
    let config = load_or_create_config(&data_dir).expect("failed to load klip config");
    let mut store = ClipStore::open(&data_dir).expect("failed to open klip sqlite");
    let _ = store.prune(
        config.retention_mode,
        config.max_history_items,
        config.max_history_days,
    );

    #[cfg(target_os = "macos")]
    let hotkey_str = config.hotkey_macos.clone();
    #[cfg(not(target_os = "macos"))]
    let hotkey_str = config.hotkey_windows.clone();

    let show_tray_icon = config.show_tray_icon;

    let state = Arc::new(AppState {
        data_dir,
        config: Mutex::new(config),
        store: Mutex::new(store),
        previous_focus: Mutex::new(None),
        suppress_hash: Mutex::new(None),
        last_sync_error: Mutex::new(None),
    });

    let state_for_setup = state.clone();
    let state_for_shortcut = state.clone();

    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default()
        .manage(state)
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, _shortcut, event| {
                    if event.state == ShortcutState::Pressed {
                        platform::toggle_main_panel(app, &state_for_shortcut);
                    }
                })
                .build(),
        );

    #[cfg(target_os = "macos")]
    {
        builder = builder.plugin(tauri_nspanel::init());
    }

    builder
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::update_settings,
            commands::query_items,
            commands::get_item_detail,
            commands::hide_panel,
            commands::copy_item,
            commands::toggle_pin,
            commands::delete_item,
            commands::clear_unpinned,
            commands::get_sync_status,
            commands::setup_sync,
            commands::sync_now,
        ])
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            platform::setup_main_window(app.handle())?;
            setup_tray(app.handle(), state_for_setup.clone(), show_tray_icon)?;

            if let Ok(shortcut) = hotkey_str.parse::<Shortcut>() {
                let _ = app.global_shortcut().register(shortcut);
            }

            start_clipboard_watcher(app.handle().clone(), state_for_setup.clone());
            start_sync_loop(app.handle().clone(), state_for_setup.clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running klip");
}
