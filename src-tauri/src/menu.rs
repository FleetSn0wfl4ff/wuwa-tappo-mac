use serde::Deserialize;
use serde_json::Value;
use std::sync::Mutex;
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, MenuItemKind, PredefinedMenuItem, Submenu},
    tray::{TrayIcon, TrayIconBuilder},
    AppHandle, Emitter, LogicalPosition, Manager, WebviewWindow,
};

const TRAY_ID: &str = "wuwatappo-mac-tray";
const SCALE_PRESETS: [u32; 7] = [50, 75, 100, 125, 150, 175, 200];
const OPACITY_PRESETS: [u32; 5] = [0, 25, 50, 75, 100];

#[derive(Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Labels {
    open_bag: String,
    open_atlas: String,
    open_exchange: String,
    open_room: String,
    hide_pet: String,
    show_pet: String,
    tray_open_settings: String,
    dock_settings: String,
    window_size: String,
    window_passthrough: String,
    game_mode: String,
    window_opacity: String,
    restart: String,
    quit: String,
    tray_tooltip: String,
}

impl Default for Labels {
    fn default() -> Self {
        Self {
            open_bag: "背包".into(),
            open_atlas: "图鉴".into(),
            open_exchange: "合成".into(),
            open_room: "联机".into(),
            hide_pet: "隐藏桌宠".into(),
            show_pet: "显示桌宠".into(),
            tray_open_settings: "设置".into(),
            dock_settings: "设置".into(),
            window_size: "桌宠大小".into(),
            window_passthrough: "鼠标穿透".into(),
            game_mode: "游戏模式".into(),
            window_opacity: "不透明度".into(),
            restart: "重新启动".into(),
            quit: "退出 Wuwa Tappo".into(),
            tray_tooltip: "Wuwa Tappo".into(),
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct MenuOptions {
    labels: Labels,
    pet_dock_visible: bool,
    pet_scale: u32,
    window_opacity: u32,
    window_passthrough: bool,
    game_mode: bool,
}

impl Default for MenuOptions {
    fn default() -> Self {
        Self {
            labels: Labels::default(),
            pet_dock_visible: true,
            pet_scale: 100,
            window_opacity: 100,
            window_passthrough: false,
            game_mode: false,
        }
    }
}

impl MenuOptions {
    fn from_args(args: &Value) -> Result<Self, String> {
        let options: Self = serde_json::from_value(args.clone()).map_err(|e| e.to_string())?;
        if !(50..=200).contains(&options.pet_scale) {
            return Err("petScale must be between 50 and 200".into());
        }
        if options.window_opacity > 100 {
            return Err("windowOpacity must be between 0 and 100".into());
        }
        Ok(options)
    }
}

struct MenuState {
    context: Mutex<Menu<tauri::Wry>>,
    tray: TrayIcon,
    tray_menu: Mutex<Menu<tauri::Wry>>,
}

fn normal(app: &AppHandle, id: &str, title: &str) -> tauri::Result<MenuItemKind<tauri::Wry>> {
    Ok(MenuItemKind::MenuItem(MenuItem::with_id(
        app,
        id,
        title,
        true,
        None::<&str>,
    )?))
}

fn checked(
    app: &AppHandle,
    id: &str,
    title: &str,
    value: bool,
) -> tauri::Result<MenuItemKind<tauri::Wry>> {
    Ok(MenuItemKind::Check(CheckMenuItem::with_id(
        app,
        id,
        title,
        true,
        value,
        None::<&str>,
    )?))
}

fn selection_menu(
    app: &AppHandle,
    title: &str,
    prefix: &str,
    presets: &[u32],
    selected: u32,
) -> tauri::Result<MenuItemKind<tauri::Wry>> {
    let submenu = Submenu::new(app, title, true)?;
    for preset in presets {
        submenu.append(&CheckMenuItem::with_id(
            app,
            format!("{prefix}-{preset}"),
            format!("{preset}%"),
            true,
            selected == *preset,
            None::<&str>,
        )?)?;
    }
    if !presets.contains(&selected) {
        submenu.append(&CheckMenuItem::with_id(
            app,
            format!("{prefix}-{selected}"),
            format!("{selected}%"),
            true,
            true,
            None::<&str>,
        )?)?;
    }
    Ok(MenuItemKind::Submenu(submenu))
}

fn context_items(
    app: &AppHandle,
    options: &MenuOptions,
    tray: bool,
) -> tauri::Result<Vec<MenuItemKind<tauri::Wry>>> {
    let labels = &options.labels;
    let visibility = if options.pet_dock_visible {
        &labels.hide_pet
    } else {
        &labels.show_pet
    };
    let settings = if tray {
        &labels.tray_open_settings
    } else {
        &labels.dock_settings
    };
    Ok(vec![
        normal(app, "open-bag", &labels.open_bag)?,
        normal(app, "open-atlas", &labels.open_atlas)?,
        normal(app, "open-exchange", &labels.open_exchange)?,
        normal(app, "open-room", &labels.open_room)?,
        normal(app, "open-setting", settings)?,
        MenuItemKind::Predefined(PredefinedMenuItem::separator(app)?),
        normal(app, "toggle-pet", visibility)?,
        selection_menu(
            app,
            &labels.window_size,
            "pet-scale",
            &SCALE_PRESETS,
            options.pet_scale,
        )?,
        selection_menu(
            app,
            &labels.window_opacity,
            "window-opacity",
            &OPACITY_PRESETS,
            options.window_opacity,
        )?,
        checked(
            app,
            "toggle-passthrough",
            &labels.window_passthrough,
            options.window_passthrough,
        )?,
        checked(
            app,
            "toggle-game-mode",
            &labels.game_mode,
            options.game_mode,
        )?,
        MenuItemKind::Predefined(PredefinedMenuItem::separator(app)?),
        normal(app, "input-access", "启用键鼠监听…")?,
        normal(app, "open-logs", "查看日志")?,
        normal(app, "session-mode", session_mode_label(current_mode(app)))?,
        MenuItemKind::Predefined(PredefinedMenuItem::separator(app)?),
        normal(app, "restart", &labels.restart)?,
        normal(app, "quit", &labels.quit)?,
    ])
}

fn context_menu(
    app: &AppHandle,
    options: &MenuOptions,
    tray: bool,
) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::new(app)?;
    for item in context_items(app, options, tray)? {
        menu.append(&item)?;
    }
    Ok(menu)
}

fn application_menu(app: &AppHandle, options: &MenuOptions) -> tauri::Result<Menu<tauri::Wry>> {
    let application = Submenu::new(app, crate::profile::Region::from_app(app).name(), true)?;
    application.append(&MenuItem::with_id(
        app,
        "open-about",
        "关于 Wuwa Tappo",
        true,
        None::<&str>,
    )?)?;
    let session_label = session_mode_label(current_mode(app));
    application.append(&MenuItem::with_id(
        app,
        "session-mode",
        session_label,
        true,
        None::<&str>,
    )?)?;
    application.append(&PredefinedMenuItem::separator(app)?)?;
    application.append(&MenuItem::with_id(
        app,
        "open-setting",
        &options.labels.tray_open_settings,
        true,
        Some("CmdOrCtrl+,"),
    )?)?;
    application.append(&MenuItem::with_id(
        app,
        "input-access",
        "启用键鼠监听…",
        true,
        None::<&str>,
    )?)?;
    application.append(&PredefinedMenuItem::separator(app)?)?;
    application.append(&PredefinedMenuItem::hide(app, None)?)?;
    application.append(&PredefinedMenuItem::hide_others(app, None)?)?;
    application.append(&PredefinedMenuItem::show_all(app, None)?)?;
    application.append(&PredefinedMenuItem::separator(app)?)?;
    application.append(&MenuItem::with_id(
        app,
        "quit",
        &options.labels.quit,
        true,
        Some("CmdOrCtrl+Q"),
    )?)?;

    let edit = Submenu::with_items(
        app,
        "编辑",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;
    let pet = Submenu::new(app, "Wuwa Tappo", true)?;
    for item in context_items(app, options, false)? {
        pet.append(&item)?;
    }
    Menu::with_items(app, &[&application, &edit, &pet])
}

fn event_for_id(id: &str) -> Option<(&'static str, Value)> {
    let event = match id {
        "open-bag" => ("dock-open-panel", Value::from("bag")),
        "open-atlas" => ("dock-open-panel", Value::from("atlas")),
        "open-exchange" => ("dock-open-panel", Value::from("exchange")),
        "open-room" => ("dock-open-panel", Value::from("room")),
        "open-setting" => ("dock-open-panel", Value::from("setting")),
        "toggle-pet" => ("tray-toggle-pet-visibility", Value::Null),
        "toggle-passthrough" => ("tray-toggle-window-passthrough", Value::Null),
        "toggle-game-mode" => ("tray-toggle-game-mode", Value::Null),
        "restart" => ("tray-restart", Value::Null),
        "quit" => ("tray-quit", Value::Null),
        "input-access" => ("mac-request-input-access", Value::Null),
        "open-about" => ("mac-open-about", Value::Null),
        _ => {
            if let Some(value) = id.strip_prefix("pet-scale-") {
                let value = value.parse::<u32>().ok()?;
                if !(50..=200).contains(&value) {
                    return None;
                }
                return Some(("tray-set-pet-scale", Value::from(value)));
            }
            if let Some(value) = id.strip_prefix("window-opacity-") {
                let value = value.parse::<u32>().ok()?;
                if value > 100 {
                    return None;
                }
                return Some(("tray-set-window-opacity", Value::from(value)));
            }
            return None;
        }
    };
    Some(event)
}

pub fn setup(app: &AppHandle) -> Result<(), String> {
    let options = MenuOptions::default();
    let tray_menu = context_menu(app, &options, true).map_err(|e| e.to_string())?;
    let context = context_menu(app, &options, false).map_err(|e| e.to_string())?;
    let icon = app.default_window_icon().cloned().ok_or("Application icon is unavailable")?;
    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip(crate::profile::Region::from_app(app).name())
        .menu(&tray_menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                // macOS habit: the primary click performs the main action, the
                // menu stays on the secondary click.
                let app = tray.app_handle();
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                }
                let _ = app.emit("dock-open-panel", "bag");
            }
        })
        .build(app)
        .map_err(|e| e.to_string())?;
    app.set_menu(application_menu(app, &options).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if !app.manage(MenuState {
        context: Mutex::new(context),
        tray,
        tray_menu: Mutex::new(tray_menu),
    }) {
        return Err("native menu already initialized".into());
    }
    app.on_menu_event(|app, event| {
        let id = event.id().as_ref();
        if app.get_webview_window("main").is_none() {
            match id {
                "quit" => {
                    app.exit(0);
                    return;
                }
                "restart" => app.restart(),
                _ => {}
            }
        }
        let result = if id == "session-mode" {
            toggle_session_mode(app);
            Ok(())
        } else if id == "open-logs" {
            crate::system::open_log_dir(app)
        } else if let Some((name, payload)) = event_for_id(id) {
            app.emit(name, payload).map_err(|e| e.to_string())
        } else {
            Ok(())
        };
        if let Err(error) = result {
            eprintln!("native menu action failed: {error}");
        }
    });
    Ok(())
}

pub fn sync(app: &AppHandle, args: &Value) -> Result<Value, String> {
    let options = MenuOptions::from_args(args)?;
    let tray_menu = context_menu(app, &options, true).map_err(|e| e.to_string())?;
    let context = context_menu(app, &options, false).map_err(|e| e.to_string())?;
    let state = app
        .try_state::<MenuState>()
        .ok_or("native menu not initialized")?;
    state
        .tray
        .set_menu(Some(tray_menu))
        .map_err(|e| e.to_string())?;
    state
        .tray
        .set_tooltip(Some(&options.labels.tray_tooltip))
        .map_err(|e| e.to_string())?;
    app.set_menu(application_menu(app, &options).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    *state
        .context
        .lock()
        .map_err(|_| "native menu state poisoned")? = context;
    Ok(Value::Null)
}

pub fn popup(app: &AppHandle, window: &WebviewWindow, x: f64, y: f64) -> Result<Value, String> {
    if !x.is_finite() || !y.is_finite() {
        return Err("context menu coordinates must be finite".into());
    }
    let state = app
        .try_state::<MenuState>()
        .ok_or("native menu not initialized")?;
    let menu = state
        .context
        .lock()
        .map_err(|_| "native menu state poisoned")?
        .clone();
    window
        .popup_menu_at(&menu, LogicalPosition::new(x, y))
        .map_err(|e| e.to_string())?;
    Ok(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normal_shutdown_actions_preserve_frontend_flush_events() {
        assert_eq!(event_for_id("quit"), Some(("tray-quit", Value::Null)));
        assert_eq!(event_for_id("restart"), Some(("tray-restart", Value::Null)));
    }

    #[test]
    fn native_panel_events_match_frontend_tab_validator() {
        for (id, tab) in [
            ("open-bag", "bag"),
            ("open-atlas", "atlas"),
            ("open-exchange", "exchange"),
            ("open-room", "room"),
            ("open-setting", "setting"),
        ] {
            assert_eq!(event_for_id(id), Some(("dock-open-panel", json!(tab))));
        }
    }

    #[test]
    fn scale_and_opacity_payloads_are_numbers() {
        assert_eq!(
            event_for_id("pet-scale-125"),
            Some(("tray-set-pet-scale", json!(125)))
        );
        assert_eq!(
            event_for_id("window-opacity-0"),
            Some(("tray-set-window-opacity", json!(0)))
        );
        assert!(event_for_id("pet-scale-201").is_none());
        assert!(event_for_id("window-opacity-101").is_none());
        assert!(event_for_id("pet-scale-nan").is_none());
    }

    #[test]
    fn options_preserve_original_percentage_limits() {
        let options = MenuOptions::from_args(&json!({
            "labels": { "openBag": "Backpack" },
            "petDockVisible": false,
            "petScale": 51,
            "windowOpacity": 0,
            "windowPassthrough": true
        }))
        .unwrap();
        assert_eq!(options.labels.open_bag, "Backpack");
        assert_eq!(options.labels.open_atlas, "图鉴");
        assert!(!options.pet_dock_visible);
        assert_eq!(options.pet_scale, 51);
        assert_eq!(options.window_opacity, 0);
        assert!(options.window_passthrough);
        assert!(MenuOptions::from_args(&json!({ "petScale": 49 })).is_err());
        assert!(MenuOptions::from_args(&json!({ "windowOpacity": 101 })).is_err());
    }
}

/// Reflect the real input-monitoring state in the menu bar instead of offering a
/// command whose effect depends on a permission the user cannot see.

/// Reflect the real input-monitoring state in the menu instead of offering a
/// command whose effect depends on a permission the user cannot see.
pub fn set_input_access_state(app: &AppHandle, granted: bool) {
    let title = if granted {
        "键鼠监听：已启用"
    } else {
        "键鼠监听：未启用（点击开启）"
    };
    let mut menus: Vec<Menu<tauri::Wry>> = Vec::new();
    if let Some(state) = app.try_state::<MenuState>() {
        if let Ok(menu) = state.tray_menu.lock() {
            menus.push(menu.clone());
        }
        if let Ok(menu) = state.context.lock() {
            menus.push(menu.clone());
        }
    }
    if let Some(menu) = app.menu() {
        menus.push(menu);
    }
    for menu in menus {
        if let Some(item) = menu.get("input-access") {
            if let Some(menu_item) = item.as_menuitem() {
                let _ = menu_item.set_text(title);
            }
        }
    }
}

fn current_mode(app: &AppHandle) -> Option<crate::startup::SessionMode> {
    if let Some(session) = app.try_state::<crate::session::SessionStore>() {
        return Some(if session.is_ephemeral() {
            crate::startup::SessionMode::Memory
        } else {
            crate::startup::SessionMode::Keychain
        });
    }
    app.path()
        .app_config_dir()
        .ok()
        .and_then(|dir| crate::startup::load_mode(&dir.join("session-mode.json")).ok().flatten())
}

fn session_mode_label(mode: Option<crate::startup::SessionMode>) -> &'static str {
    match mode {
        Some(crate::startup::SessionMode::Memory) => "登录状态：仅本次会话（重启后需重新登录，点击切换）",
        _ => "登录状态：已记住（重启后免登录，点击切换）",
    }
}

/// Flip between the ephemeral and the keychain-backed session store. The choice
/// only affects the next launch, so the user is told (and offered a restart)
/// instead of silently losing or gaining persistence.
fn toggle_session_mode(app: &AppHandle) {
    let Ok(directory) = app.path().app_config_dir() else {
        return;
    };
    let path = directory.join("session-mode.json");
    let current = crate::startup::load_mode(&path).ok().flatten();
    let next = match current {
        Some(crate::startup::SessionMode::Memory) => crate::startup::SessionMode::Keychain,
        _ => crate::startup::SessionMode::Memory,
    };
    if let Err(error) = crate::startup::save_mode(&path, next) {
        eprintln!("Could not save the session mode: {error}");
        return;
    }
    let label = session_mode_label(Some(next));
    if let Some(menu) = app.menu() {
        if let Some(item) = menu.get("session-mode") {
            if let Some(menu_item) = item.as_menuitem() {
                let _ = menu_item.set_text(label);
            }
        }
    }
    if let Some(state) = app.try_state::<MenuState>() {
        if let Ok(tray_menu) = state.tray_menu.lock() {
            if let Some(item) = tray_menu.get("session-mode") {
                if let Some(menu_item) = item.as_menuitem() {
                    let _ = menu_item.set_text(label);
                }
            }
        }
    }
    show_session_mode_hint(app, next);
}

#[cfg(target_os = "macos")]
fn show_session_mode_hint(app: &AppHandle, mode: crate::startup::SessionMode) {
    use objc2_foundation::NSString;
    let Some(mtm) = objc2::MainThreadMarker::new() else {
        return;
    };
    let (title, detail) = match mode {
        crate::startup::SessionMode::Memory => (
            "已切换为「仅本次会话」",
            "登录凭据只保存在内存中，退出应用后需要重新登录。重启应用后生效。",
        ),
        crate::startup::SessionMode::Keychain => (
            "已切换为「记住登录」",
            "登录凭据会写入 macOS 钥匙串，退出应用后仍保持登录。重启应用后生效。",
        ),
    };
    let alert = objc2_app_kit::NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(detail));
    alert.addButtonWithTitle(&NSString::from_str("立即重启"));
    alert.addButtonWithTitle(&NSString::from_str("稍后"));
    if alert.runModal() == 1000 {
        app.restart();
    }
}

#[cfg(not(target_os = "macos"))]
fn show_session_mode_hint(_app: &AppHandle, _mode: crate::startup::SessionMode) {}
