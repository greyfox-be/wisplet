use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, LogicalSize, Manager, Monitor, State, WebviewWindow,
};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

#[cfg(target_os = "windows")]
fn set_window_alpha(window: &tauri::WebviewWindow, alpha: u8) {
    use windows::Win32::Foundation::{COLORREF, HWND};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetLayeredWindowAttributes, SetWindowLongPtrW, GWL_EXSTYLE, LWA_ALPHA,
        WS_EX_LAYERED,
    };
    if let Ok(raw) = window.hwnd() {
        let hwnd = HWND(raw.0 as *mut _);
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            if (style & WS_EX_LAYERED.0 as isize) == 0 {
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | WS_EX_LAYERED.0 as isize);
            }
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA);
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn set_window_alpha(_window: &tauri::WebviewWindow, _alpha: u8) {}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct App {
    pub id: i64,
    pub title: String,
    pub path: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(rename = "type", default)]
    pub app_type: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
}

#[derive(Default)]
struct AppState {
    // True = window should cover the current monitor (fake fullscreen overlay).
    // False = windowed, centered. We avoid Tauri's real set_fullscreen on Win11
    // because it (a) eats Alt+Shift+A global hotkey and (b) glitches the show anim
    // with a brief windowed flash before going fullscreen.
    fullsize: AtomicBool,
}

fn apps_json_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config").join("yasb").join("launchpad").join("apps.json"))
}

#[tauri::command]
fn load_apps() -> Result<Vec<App>, String> {
    let path = apps_json_path().ok_or_else(|| "Cannot resolve home directory".to_string())?;
    let content = fs::read_to_string(&path)
        .map_err(|e| format!("Cannot read {}: {}", path.display(), e))?;
    let apps: Vec<App> = serde_json::from_str(&content).map_err(|e| e.to_string())?;
    Ok(apps)
}

#[tauri::command]
fn launch_app(path: String) -> Result<(), String> {
    use std::process::Command;
    Command::new("cmd")
        .args(["/c", "start", "", "/B", &path])
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn hide_window(app: AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        window.hide().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn set_fullsize_pref(app: AppHandle, state: State<AppState>, value: bool) -> Result<(), String> {
    state.fullsize.store(value, Ordering::Relaxed);
    if let Some(window) = app.get_webview_window("main") {
        if window.is_visible().unwrap_or(false) {
            apply_window_geometry(&app, &window, value);
        }
    }
    Ok(())
}

fn monitor_for_cursor(app: &AppHandle) -> Option<Monitor> {
    let cursor = app.cursor_position().ok()?;
    let cx = cursor.x as i32;
    let cy = cursor.y as i32;
    let monitors = app.available_monitors().ok()?;
    monitors.into_iter().find(|m| {
        let p = m.position();
        let s = m.size();
        cx >= p.x && cx < p.x + s.width as i32 && cy >= p.y && cy < p.y + s.height as i32
    })
}

fn apply_window_geometry(app: &AppHandle, window: &WebviewWindow, fullsize: bool) {
    if fullsize {
        if let Some(monitor) = monitor_for_cursor(app) {
            let _ = window.set_size(*monitor.size());
            let _ = window.set_position(*monitor.position());
            return;
        }
    }
    let _ = window.set_size(LogicalSize::new(1400.0, 900.0));
    let _ = window.center();
}

// Toggle flow:
//   - Hidden → set geometry (windowed or monitor-cover), show invisible (alpha 0),
//     wait for first paint, emit "anim-in" so Svelte starts its fade, then 60ms
//     later bump alpha to 255.
//   - Visible → emit "anim-out" → Svelte plays out anim → calls hide_window.
fn show_or_hide(app: &AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        let visible = window.is_visible().map_err(|e| e.to_string())?;
        if visible {
            let _ = app.emit("anim-out", ());
        } else {
            set_window_alpha(&window, 0);
            let fullsize = app.state::<AppState>().fullsize.load(Ordering::Relaxed);
            apply_window_geometry(app, &window, fullsize);
            window.show().map_err(|e| e.to_string())?;
            window.set_focus().map_err(|e| e.to_string())?;

            let win = window.clone();
            let handle = app.clone();
            std::thread::spawn(move || {
                // Longer wait so WebView2 has fully painted the DOM (with transparent bg
                // applied) before we make the window visible.
                std::thread::sleep(std::time::Duration::from_millis(100));
                let _ = handle.emit("anim-in", ());
                std::thread::sleep(std::time::Duration::from_millis(60));
                set_window_alpha(&win, 255);
            });
        }
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let toggle_shortcut = Shortcut::new(Some(Modifiers::ALT | Modifiers::SHIFT), Code::KeyA);

    tauri::Builder::default()
        .manage(AppState::default())
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, shortcut, event| {
                    if shortcut == &toggle_shortcut && event.state() == ShortcutState::Pressed {
                        let _ = show_or_hide(app);
                    }
                })
                .build(),
        )
        .setup(move |app| {
            app.global_shortcut().register(toggle_shortcut)?;

            // Make the WebView2 control itself transparent (no white default background).
            // This is the canonical fix for the Tauri 2 / WebView2 "white flash on show" bug.
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_background_color(Some(tauri::window::Color(0, 0, 0, 0)));
            }

            // Tag window as Win32 tool window so tiling WMs (komorebi, etc.) skip it.
            #[cfg(target_os = "windows")]
            if let Some(window) = app.get_webview_window("main") {
                use windows::Win32::Foundation::HWND;
                use windows::Win32::UI::WindowsAndMessaging::{
                    GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_TOOLWINDOW,
                };
                if let Ok(raw) = window.hwnd() {
                    let hwnd = HWND(raw.0 as *mut _);
                    unsafe {
                        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | WS_EX_TOOLWINDOW.0 as isize);
                    }
                }
            }

            // Auto-hide on focus loss — emit anim-out so Svelte plays the exit anim.
            #[cfg(not(debug_assertions))]
            if let Some(window) = app.get_webview_window("main") {
                let handle = app.app_handle().clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::Focused(false) = event {
                        let _ = handle.emit("anim-out", ());
                    }
                });
            }

            let show_item = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit Wisplet", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_item, &quit_item])?;

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Wisplet — Alt+Shift+A to toggle")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => {
                        let _ = show_or_hide(app);
                    }
                    "quit" => {
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let _ = show_or_hide(tray.app_handle());
                    }
                })
                .build(app)?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            load_apps,
            launch_app,
            hide_window,
            set_fullsize_pref
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
