use tauri::AppHandle;

#[derive(Debug, Clone)]
pub struct PreviousFocus {
    #[cfg(target_os = "macos")]
    pub bundle_id: Option<String>,
    #[cfg(target_os = "windows")]
    pub hwnd: isize,
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    pub _unused: (),
}

#[cfg(target_os = "macos")]
mod imp {
    use std::thread;
    use std::time::Duration;

    use enigo::{Direction, Enigo, Key, Keyboard, Settings};
    use objc2_app_kit::{NSApplicationActivationOptions, NSWorkspace};
    use tauri::{AppHandle, Emitter, Manager, WebviewUrl};
    use tauri_nspanel::{
        CollectionBehavior, ManagerExt, PanelBuilder, PanelLevel, StyleMask, tauri_panel,
    };

    use super::PreviousFocus;

    const SELF_BUNDLE_ID: &str = "xyz.444200.klip";

    tauri_panel! {
        panel!(KlipPanel {
            config: {
                can_become_key_window: true,
                can_become_main_window: false,
                is_floating_panel: true
            }
        })

        panel_event!(KlipPanelEventHandler {
            window_did_resign_key(notification: &NSNotification) -> ()
        })
    }

    pub fn setup_main_window(app: &AppHandle) -> tauri::Result<()> {
        let panel = PanelBuilder::<_, KlipPanel>::new(app, "main")
            .url(WebviewUrl::App("index.html".into()))
            .title("Klip")
            .size(tauri::Size::Logical(tauri::LogicalSize::new(780.0, 500.0)))
            .with_window(|w| {
                w.decorations(false)
                    .transparent(true)
                    .visible(false)
                    .resizable(false)
                    .center()
            })
            .add_style_mask(StyleMask::empty().nonactivating_panel())
            .collection_behavior(
                CollectionBehavior::new()
                    .can_join_all_spaces()
                    .full_screen_auxiliary()
                    .ignores_cycle(),
            )
            .level(PanelLevel::Floating)
            .no_activate(true)
            .corner_radius(14.0)
            .has_shadow(true)
            .hides_on_deactivate(false)
            .build()?;

        let handler = KlipPanelEventHandler::new();
        let app_handle = app.clone();
        handler.window_did_resign_key(move |_notification| {
            if let Ok(p) = app_handle.get_webview_panel("main") {
                p.hide();
            }
        });
        panel.set_event_handler(Some(handler.as_ref()));
        Ok(())
    }

    pub fn is_panel_visible(app: &AppHandle) -> bool {
        app.get_webview_panel("main")
            .map(|p| p.is_visible())
            .unwrap_or(false)
    }

    pub fn show_main_panel(app: &AppHandle) {
        if let Some(win) = app.get_webview_window("main") {
            let _ = win.center();
        }
        if let Ok(panel) = app.get_webview_panel("main") {
            panel.show_and_make_key();
            let _ = app.emit("klip://shown", ());
        }
    }

    pub fn hide_main_panel(app: &AppHandle) {
        if let Ok(panel) = app.get_webview_panel("main") {
            panel.hide();
        }
    }

    pub fn active_app_name() -> Option<String> {
        let ws = NSWorkspace::sharedWorkspace();
        let app = ws.frontmostApplication()?;
        if app
            .bundleIdentifier()
            .is_some_and(|id| id.to_string() == SELF_BUNDLE_ID)
        {
            return None;
        }
        app.localizedName().map(|n| n.to_string())
    }

    pub fn capture_frontmost_app() -> Option<PreviousFocus> {
        let ws = NSWorkspace::sharedWorkspace();
        let app = ws.frontmostApplication()?;
        let bundle_id = app.bundleIdentifier().map(|b| b.to_string());
        if bundle_id.as_deref() == Some(SELF_BUNDLE_ID) {
            return None;
        }
        Some(PreviousFocus { bundle_id })
    }

    pub fn restore_focus_and_paste(prev: Option<PreviousFocus>) {
        if let Some(PreviousFocus {
            bundle_id: Some(target_id),
        }) = prev
        {
            let ws = NSWorkspace::sharedWorkspace();
            let running = ws.runningApplications();
            for app in running.iter() {
                if app
                    .bundleIdentifier()
                    .is_some_and(|id| id.to_string() == target_id)
                {
                    #[allow(deprecated)]
                    let _ = app.activateWithOptions(NSApplicationActivationOptions::empty());
                    break;
                }
            }
        }
        thread::sleep(Duration::from_millis(60));
        if let Ok(mut enigo) = Enigo::new(&Settings::default()) {
            let _ = enigo.key(Key::Meta, Direction::Press);
            let _ = enigo.key(Key::Unicode('v'), Direction::Click);
            let _ = enigo.key(Key::Meta, Direction::Release);
        }
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::path::Path;
    use std::thread;
    use std::time::Duration;

    use enigo::{Direction, Enigo, Key, Keyboard, Settings};
    use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
    use windows::Win32::Foundation::{HWND, MAX_PATH};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow,
    };

    use super::PreviousFocus;

    pub fn setup_main_window(app: &AppHandle) -> tauri::Result<()> {
        let win = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
            .title("Klip")
            .inner_size(780.0, 500.0)
            .decorations(false)
            .transparent(true)
            .visible(false)
            .resizable(false)
            .skip_taskbar(true)
            .always_on_top(true)
            .center()
            .build()?;

        let app_handle = app.clone();
        win.on_window_event(move |event| {
            if let tauri::WindowEvent::Focused(false) = event
                && let Some(w) = app_handle.get_webview_window("main")
            {
                let _ = w.hide();
            }
        });
        Ok(())
    }

    pub fn is_panel_visible(app: &AppHandle) -> bool {
        app.get_webview_window("main")
            .and_then(|w| w.is_visible().ok())
            .unwrap_or(false)
    }

    pub fn show_main_panel(app: &AppHandle) {
        if let Some(win) = app.get_webview_window("main") {
            let _ = win.center();
            let _ = win.show();
            let _ = win.set_focus();
            let _ = app.emit("klip://shown", ());
        }
    }

    pub fn hide_main_panel(app: &AppHandle) {
        if let Some(win) = app.get_webview_window("main") {
            let _ = win.hide();
        }
    }

    pub fn active_app_name() -> Option<String> {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.0.is_null() {
                return None;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == 0 || pid == std::process::id() {
                return None;
            }
            let Ok(proc_handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return None;
            };
            let mut buf = [0u16; MAX_PATH as usize];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(
                proc_handle,
                PROCESS_NAME_WIN32,
                windows::core::PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
            .is_ok();
            let _ = windows::Win32::Foundation::CloseHandle(proc_handle);
            if !ok || len == 0 {
                return None;
            }
            let full = OsString::from_wide(&buf[..len as usize]);
            let name = Path::new(&full)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())?;
            Some(name)
        }
    }

    pub fn capture_frontmost_app() -> Option<PreviousFocus> {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.0.is_null() {
                None
            } else {
                Some(PreviousFocus {
                    hwnd: hwnd.0 as isize,
                })
            }
        }
    }

    pub fn restore_focus_and_paste(prev: Option<PreviousFocus>) {
        if let Some(PreviousFocus { hwnd }) = prev
            && hwnd != 0
        {
            unsafe {
                let _ = SetForegroundWindow(HWND(hwnd as *mut core::ffi::c_void));
            }
        }
        thread::sleep(Duration::from_millis(60));
        if let Ok(mut enigo) = Enigo::new(&Settings::default()) {
            let _ = enigo.key(Key::Control, Direction::Press);
            let _ = enigo.key(Key::Unicode('v'), Direction::Click);
            let _ = enigo.key(Key::Control, Direction::Release);
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod imp {
    use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

    use super::PreviousFocus;

    pub fn setup_main_window(app: &AppHandle) -> tauri::Result<()> {
        WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
            .title("Klip")
            .inner_size(780.0, 500.0)
            .decorations(false)
            .transparent(true)
            .visible(false)
            .resizable(false)
            .center()
            .build()?;
        Ok(())
    }

    pub fn is_panel_visible(app: &AppHandle) -> bool {
        app.get_webview_window("main")
            .and_then(|w| w.is_visible().ok())
            .unwrap_or(false)
    }

    pub fn show_main_panel(app: &AppHandle) {
        if let Some(win) = app.get_webview_window("main") {
            let _ = win.center();
            let _ = win.show();
            let _ = win.set_focus();
            let _ = app.emit("klip://shown", ());
        }
    }

    pub fn hide_main_panel(app: &AppHandle) {
        if let Some(win) = app.get_webview_window("main") {
            let _ = win.hide();
        }
    }

    pub fn active_app_name() -> Option<String> {
        None
    }

    pub fn capture_frontmost_app() -> Option<PreviousFocus> {
        None
    }

    pub fn restore_focus_and_paste(_prev: Option<PreviousFocus>) {}
}

pub fn setup_main_window(app: &AppHandle) -> tauri::Result<()> {
    imp::setup_main_window(app)
}

pub fn toggle_main_panel(app: &AppHandle, state: &crate::commands::AppState) {
    if imp::is_panel_visible(app) {
        imp::hide_main_panel(app);
    } else {
        let prev = imp::capture_frontmost_app();
        if let Ok(mut guard) = state.previous_focus.lock() {
            *guard = prev;
        }
        imp::show_main_panel(app);
    }
}

pub fn show_main_panel(app: &AppHandle, state: &crate::commands::AppState) {
    if !imp::is_panel_visible(app) {
        let prev = imp::capture_frontmost_app();
        if let Ok(mut guard) = state.previous_focus.lock() {
            *guard = prev;
        }
    }
    imp::show_main_panel(app);
}

pub fn hide_main_panel(app: &AppHandle) {
    imp::hide_main_panel(app);
}

pub fn active_app_name() -> Option<String> {
    imp::active_app_name()
}

pub fn restore_focus_and_paste(prev: Option<PreviousFocus>) {
    imp::restore_focus_and_paste(prev);
}
