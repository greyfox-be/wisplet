use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, LogicalSize, Manager, Monitor, State, WebviewWindow,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

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

// ===== Crash surfacing =====
// Wisplet runs without a console (Windows GUI subsystem), so panics and Tauri
// setup errors otherwise die silently. We pop a MessageBox AND append to
// %LOCALAPPDATA%\Wisplet\crash.log so the user knows what happened.

#[cfg(target_os = "windows")]
fn show_fatal_dialog(title: &str, body: &str) {
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_OK, MB_SYSTEMMODAL,
    };
    let title_w: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let body_w: Vec<u16> = body.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(body_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            MB_OK | MB_ICONERROR | MB_SYSTEMMODAL,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn show_fatal_dialog(_title: &str, _body: &str) {}

fn crash_log_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("Wisplet").join("crash.log"))
}

fn append_crash_log(body: &str) {
    let Some(p) = crash_log_path() else { return };
    if let Some(parent) = p.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = format!("=== unix:{ts} ===\n{body}\n\n");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        use std::io::Write;
        let _ = f.write_all(line.as_bytes());
    }
}

fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<unknown panic payload>".to_string());
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown location>".to_string());
        append_crash_log(&format!("Panic à {location} :\n\n{payload}"));
        show_fatal_dialog(
            "Wisplet — crash",
            "Wisplet a planté. Détails dans %LOCALAPPDATA%\\Wisplet\\crash.log",
        );
        default(info);
    }));
}

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
    // True while the add/edit modal is open. The native file picker steals
    // window focus; without this flag the Focused(false) handler would emit
    // anim-out and hide the launcher mid-edit.
    hide_suppressed: AtomicBool,
    current_shortcut: Mutex<Option<Shortcut>>,
    current_combo: Mutex<Option<String>>,
}

// Cascade tried at startup. First one that registers wins. If all three are
// taken by other apps, the main window auto-shows so the user can pick a
// custom combo in settings.
const FALLBACK_HOTKEYS: &[&str] = &["Alt+Space", "Ctrl+Alt+Space", "Ctrl+Shift+Space"];

// Read-only YASB launchpad file — used only once to seed Wisplet's own file.
fn yasb_apps_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config").join("yasb").join("launchpad").join("apps.json"))
}

// Wisplet's own config dir — the launcher owns this once seeded.
fn wisplet_config_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config").join("wisplet"))
}

fn wisplet_apps_path() -> Option<PathBuf> {
    wisplet_config_dir().map(|d| d.join("apps.json"))
}

fn wisplet_icons_dir() -> Option<PathBuf> {
    wisplet_config_dir().map(|d| d.join("icons"))
}

#[tauri::command]
fn load_apps() -> Result<Vec<App>, String> {
    let path = wisplet_apps_path().ok_or_else(|| "Cannot resolve home directory".to_string())?;

    // First run: seed Wisplet's file from YASB's launchpad, then never touch
    // YASB again. If neither file exists, start with an empty list.
    if !path.exists() {
        match yasb_apps_path() {
            Some(yasb) if yasb.exists() => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                fs::copy(&yasb, &path)
                    .map_err(|e| format!("Cannot seed from {}: {}", yasb.display(), e))?;
            }
            _ => return Ok(Vec::new()),
        }
    }

    let content = fs::read_to_string(&path)
        .map_err(|e| format!("Cannot read {}: {}", path.display(), e))?;
    let apps: Vec<App> = serde_json::from_str(&content).map_err(|e| e.to_string())?;
    Ok(apps)
}

#[tauri::command]
fn save_apps(apps: Vec<App>) -> Result<(), String> {
    let path = wisplet_apps_path().ok_or_else(|| "Cannot resolve home directory".to_string())?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(&apps).map_err(|e| e.to_string())?;
    // Atomic write: temp file + rename so a crash mid-write can't corrupt apps.json.
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json).map_err(|e| format!("Cannot write {}: {}", tmp.display(), e))?;
    fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

// Render a file's shell icon (HICON) to a top-down RGBA PNG buffer using Win32
// directly — avoids the `systemicons` crate, which drags in a conflicting
// `gtk-sys` even on Windows.
#[cfg(target_os = "windows")]
fn extract_icon_png(path: &str) -> Result<Vec<u8>, String> {
    use windows::core::PCWSTR;
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO,
        BITMAPINFOHEADER, DIB_RGB_COLORS, HDC,
    };
    use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON};
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, ICONINFO};

    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut shfi = SHFILEINFOW::default();
    let ok = unsafe {
        SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            Default::default(),
            Some(&mut shfi),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        )
    };
    if ok == 0 || shfi.hIcon.0.is_null() {
        return Err("Aucune icône trouvée pour ce fichier".to_string());
    }
    let hicon = shfi.hIcon;

    let render = || -> Result<Vec<u8>, String> {
        let mut info = ICONINFO::default();
        unsafe { GetIconInfo(hicon, &mut info) }.map_err(|e| e.to_string())?;

        let mut bmp = BITMAP::default();
        let got = unsafe {
            GetObjectW(
                info.hbmColor,
                std::mem::size_of::<BITMAP>() as i32,
                Some(&mut bmp as *mut _ as *mut _),
            )
        };
        if got == 0 {
            unsafe {
                let _ = DeleteObject(info.hbmColor);
                let _ = DeleteObject(info.hbmMask);
            }
            return Err("GetObjectW a échoué".to_string());
        }
        let w = bmp.bmWidth.max(1);
        let h = bmp.bmHeight.max(1);

        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w;
        bmi.bmiHeader.biHeight = -h; // negative = top-down rows
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = 0; // BI_RGB

        let hdc = unsafe { CreateCompatibleDC(HDC::default()) };
        let px = (w * h) as usize;
        let mut color = vec![0u8; px * 4];
        let lines = unsafe {
            GetDIBits(
                hdc,
                info.hbmColor,
                0,
                h as u32,
                Some(color.as_mut_ptr() as *mut _),
                &mut bmi,
                DIB_RGB_COLORS,
            )
        };

        // Modern icons carry a real alpha channel; legacy ones don't — for those
        // we derive transparency from the AND mask (white = transparent).
        let has_alpha = (0..px).any(|i| color[i * 4 + 3] != 0);
        if !has_alpha {
            let mut mask = vec![0u8; px * 4];
            unsafe {
                GetDIBits(
                    hdc,
                    info.hbmMask,
                    0,
                    h as u32,
                    Some(mask.as_mut_ptr() as *mut _),
                    &mut bmi,
                    DIB_RGB_COLORS,
                );
            }
            for i in 0..px {
                color[i * 4 + 3] = if mask[i * 4] != 0 { 0 } else { 255 };
            }
        }

        unsafe {
            let _ = DeleteDC(hdc);
            let _ = DeleteObject(info.hbmColor);
            let _ = DeleteObject(info.hbmMask);
        }
        if lines == 0 {
            return Err("GetDIBits a échoué".to_string());
        }

        // BGRA → RGBA
        for i in 0..px {
            color.swap(i * 4, i * 4 + 2);
        }

        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w as u32, h as u32);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().map_err(|e| e.to_string())?;
            writer.write_image_data(&color).map_err(|e| e.to_string())?;
        }
        Ok(out)
    };

    let result = render();
    unsafe {
        let _ = DestroyIcon(hicon);
    }
    result
}

// Extract an executable's icon as a PNG into Wisplet's icons dir, return its path.
#[cfg(target_os = "windows")]
fn save_extracted_icon(path: &str) -> Result<String, String> {
    let png = extract_icon_png(path)?;
    let dir = wisplet_icons_dir().ok_or_else(|| "Cannot resolve home directory".to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stem = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("app")
        .chars()
        .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect::<String>();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let out = dir.join(format!("{stem}_{ts}.png"));
    fs::write(&out, png).map_err(|e| e.to_string())?;
    Ok(out.to_string_lossy().into_owned())
}

fn file_stem(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

// Read the `FileDescription` field from a Win32 executable's version resource —
// that's the polished display name (e.g. Code.exe → "Visual Studio Code").
#[cfg(target_os = "windows")]
fn exe_file_description(path: &str) -> Option<String> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(wide.as_ptr()), None);
        if size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(wide.as_ptr()), 0, size, buf.as_mut_ptr() as *mut _)
            .ok()?;

        // \VarFileInfo\Translation → first [language, codepage] pair.
        let trans_q: Vec<u16> = "\\VarFileInfo\\Translation"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut trans_ptr: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut trans_len = 0u32;
        if !VerQueryValueW(
            buf.as_ptr() as *const _,
            PCWSTR(trans_q.as_ptr()),
            &mut trans_ptr,
            &mut trans_len,
        )
        .as_bool()
            || trans_ptr.is_null()
            || trans_len < 4
        {
            return None;
        }
        let lang = *(trans_ptr as *const u16);
        let cp = *(trans_ptr as *const u16).add(1);

        let sub_block = format!("\\StringFileInfo\\{lang:04x}{cp:04x}\\FileDescription");
        let sb_wide: Vec<u16> = sub_block.encode_utf16().chain(std::iter::once(0)).collect();
        let mut val_ptr: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut val_len = 0u32;
        if !VerQueryValueW(
            buf.as_ptr() as *const _,
            PCWSTR(sb_wide.as_ptr()),
            &mut val_ptr,
            &mut val_len,
        )
        .as_bool()
            || val_ptr.is_null()
            || val_len == 0
        {
            return None;
        }
        let slice = std::slice::from_raw_parts(val_ptr as *const u16, val_len as usize);
        let s = String::from_utf16_lossy(slice);
        let s = s.trim_end_matches('\0').trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
}

#[cfg(target_os = "windows")]
fn suggest_title(path: &str) -> String {
    if path.to_lowercase().ends_with(".exe") {
        if let Some(desc) = exe_file_description(path) {
            return desc;
        }
    }
    file_stem(path)
}

// Suggested title + icon for a freshly picked executable — drives the editor's
// auto-fill when the user browses for a .exe / .lnk.
#[derive(Serialize)]
pub struct AppSuggestion {
    pub title: String,
    pub icon: Option<String>,
}

#[cfg(target_os = "windows")]
#[tauri::command]
fn inspect_app(path: String) -> AppSuggestion {
    AppSuggestion {
        title: suggest_title(&path),
        icon: save_extracted_icon(&path).ok(),
    }
}

#[cfg(not(target_os = "windows"))]
#[tauri::command]
fn inspect_app(path: String) -> AppSuggestion {
    AppSuggestion {
        title: file_stem(&path),
        icon: None,
    }
}

#[tauri::command]
fn launch_app(path: String) -> Result<(), String> {
    use std::process::Command;
    let mut cmd = Command::new("cmd");
    cmd.args(["/c", "start", "", "/B", &path]);
    // Without this the child inherits Wisplet's own cwd, and anything that
    // resolves sibling files relatively (mod loaders, portable tools) fails.
    // .lnk files carry their own "Start in", so let the shell decide there.
    if !path.to_lowercase().ends_with(".lnk") {
        if let Some(dir) = std::path::Path::new(&path).parent() {
            cmd.current_dir(dir);
        }
    }
    cmd.spawn().map_err(|e| e.to_string())?;
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

#[tauri::command]
fn set_hide_suppressed(state: State<AppState>, value: bool) {
    state.hide_suppressed.store(value, Ordering::Relaxed);
}

#[tauri::command]
fn get_current_hotkey(state: State<AppState>) -> Option<String> {
    state.current_combo.lock().ok().and_then(|g| g.clone())
}

#[tauri::command]
fn set_hotkey(app: AppHandle, state: State<AppState>, combo: String) -> Result<String, String> {
    let new_sc = Shortcut::from_str(&combo)
        .map_err(|e| format!("Format invalide ({combo}): {e}"))?;
    let gs = app.global_shortcut();

    let mut cur_sc = state.current_shortcut.lock().map_err(|e| e.to_string())?;
    let mut cur_combo = state.current_combo.lock().map_err(|e| e.to_string())?;

    if let Some(old) = *cur_sc {
        if old == new_sc {
            return Ok(combo); // already active
        }
    }

    gs.register(new_sc).map_err(|e| format!("Hotkey déjà pris ({combo}): {e}"))?;
    if let Some(old) = *cur_sc {
        let _ = gs.unregister(old);
    }
    *cur_sc = Some(new_sc);
    *cur_combo = Some(combo.clone());
    Ok(combo)
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
    install_panic_hook();
    let result = tauri::Builder::default()
        .manage(AppState::default())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        let _ = show_or_hide(app);
                    }
                })
                .build(),
        )
        .setup(move |app| {
            // Try to register the cascade. First one that succeeds wins.
            // If all are taken, we still start — the user can rebind in settings.
            let gs = app.global_shortcut();
            for combo in FALLBACK_HOTKEYS {
                if let Ok(sc) = Shortcut::from_str(combo) {
                    if gs.register(sc).is_ok() {
                        let state = app.state::<AppState>();
                        if let Ok(mut g) = state.current_shortcut.lock() {
                            *g = Some(sc);
                        }
                        if let Ok(mut g) = state.current_combo.lock() {
                            *g = Some((*combo).to_string());
                        }
                        break;
                    }
                }
            }

            // If no hotkey could be registered, surface the window so the user
            // can rebind via the settings drawer (otherwise the app is unreachable).
            let no_hotkey = app
                .state::<AppState>()
                .current_shortcut
                .lock()
                .ok()
                .map(|g| g.is_none())
                .unwrap_or(true);
            if no_hotkey {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                    set_window_alpha(&window, 255);
                    let _ = app.emit("hotkey-conflict", ());
                }
            }

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
                        // Skip auto-hide while the add/edit modal owns a native
                        // file picker — that dialog steals focus on purpose.
                        let suppressed = handle
                            .state::<AppState>()
                            .hide_suppressed
                            .load(Ordering::Relaxed);
                        if !suppressed {
                            let _ = handle.emit("anim-out", ());
                        }
                    }
                });
            }

            let show_item = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit Wisplet", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_item, &quit_item])?;

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Wisplet — Alt+Space to toggle")
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
            save_apps,
            inspect_app,
            launch_app,
            hide_window,
            set_fullsize_pref,
            set_hide_suppressed,
            get_current_hotkey,
            set_hotkey
        ])
        .run(tauri::generate_context!());

    if let Err(e) = result {
        append_crash_log(&format!("Tauri n'a pas pu démarrer :\n\n{e}"));
        show_fatal_dialog(
            "Wisplet — erreur de démarrage",
            "Wisplet n'a pas pu démarrer. Détails dans %LOCALAPPDATA%\\Wisplet\\crash.log",
        );
    }
}
