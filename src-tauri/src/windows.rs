use crate::{
    geometry::{work_area_insets, HitRegion},
    session::SessionStore,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, RwLock,
    },
    time::Duration,
};
use tauri::{
    AppHandle, Emitter, Listener, LogicalPosition, LogicalSize, Manager, PhysicalPosition, WebviewUrl,
    WebviewWindow,
    WebviewWindowBuilder,
};

pub const COMMANDS: &[&str] = &[
    "apply_main_window_position",
    "close_about_window",
    "close_login_window",
    "close_oss_notice_window",
    "close_panel_window",
    "close_update_window",
    "ensure_main_taskbar_icon",
    "exit_app",
    "open_input_monitoring_settings",
    "finish_main_window_monitor_switch",
    "get_app_update_detail",
    "get_main_desktop_safe_area",
    "get_main_taskbar_insets",
    "get_maintenance_notice_payload",
    "has_maintenance_notice",
    "hide_main_after_maintenance_notice_ready",
    "is_force_update_locked",
    "open_about_window",
    "open_login_window",
    "open_maintenance_notice",
    "open_oss_notice_window",
    "open_panel_window",
    "open_update_window",
    "popup_main_dock_context_menu",
    "raise_panel_window_above_pets",
    "resize_login_window_to_content",
    "set_main_force_cursor_ignore",
    "set_main_hit_region",
    "set_notice_region",
    "set_main_skip_taskbar",
    "set_main_swallow_mouse",
    "set_main_window_content_bounds",
    "set_main_window_fullscreen_shell",
    "set_main_window_monitor_bounds",
    "set_native_input_cursor",
    "set_overlay_cursor",
    "set_panel_window_collapsed",
    "show_toast_overlay",
    "sync_aux_windows_to_main_monitor",
    "sync_main_window_to_current_monitor",
    "sync_native_menu",
    "login_window_ready",
];

#[derive(Default)]
pub struct WindowState {
    hit: RwLock<HitState>,
    update: Mutex<Value>,
    maintenance: Mutex<Option<Value>>,
    panel_collapsed: Mutex<bool>,
    pending_login: Mutex<Option<Value>>,
    login_ready: AtomicBool,
    auxiliary: RwLock<BTreeMap<String, HitRegion>>,
    main_geometry: RwLock<MainGeometry>,
    stop: AtomicBool,
    /// Last top-left of the panel so reopening it feels stable instead of
    /// jumping back to the centre every time.
    panel_position: Mutex<Option<(i32, i32)>>,
    /// Last top-left of the login window, used to move the login flow's own
    /// helper windows together with it.
    login_position: Mutex<Option<(i32, i32)>>,
    /// Frontmost application before the panel was opened; focus goes back there
    /// when the panel closes, like a native inspector window would.
    previous_app_pid: Mutex<Option<i32>>,
}

#[derive(Default)]
struct MainGeometry {
    left: f64,
    top: f64,
    scale: f64,
}

#[derive(Default)]
struct HitState {
    regions: Vec<HitRegion>,
    input_regions: Vec<HitRegion>,
    /// Screen area of a transient native notice (the input-monitoring banner).
    /// The pet ignores the cursor everywhere else, so a notice that must be
    /// clickable has to be registered here or it would be unreachable.
    notice: Option<HitRegion>,
    window_passthrough: bool,
    force_ignore: bool,
    swallow: bool,
}

fn ignores_cursor(hit: &HitState, x: f64, y: f64) -> bool {
    if hit.swallow {
        return false;
    }
    if hit.force_ignore {
        return true;
    }
    if hit.notice.as_ref().is_some_and(|region| region.contains(x, y)) {
        return false;
    }
    let regions = if hit.window_passthrough {
        &hit.input_regions
    } else {
        &hit.regions
    };
    !regions.iter().any(|region| region.contains(x, y))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HitUpdate {
    regions: Vec<HitRegion>,
    #[serde(default)]
    input_regions: Vec<HitRegion>,
    #[serde(default)]
    update_native_input: bool,
    #[serde(default)]
    window_passthrough: bool,
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn main_window(app: &AppHandle) -> Result<WebviewWindow, String> {
    app.get_webview_window("main")
        .ok_or_else(|| "Main window is unavailable".into())
}

fn initialize(app: &AppHandle, label: &str) -> Result<String, String> {
    let script = app.state::<SessionStore>().initialization_script()?;
    let mut initialization = format!(
        "{}\n{script}\n{}\n{}",
        crate::profile::Region::from_app(app).initialization_script(),
        include_str!("../../scripts/mac-websocket.js"),
        include_str!("../../scripts/mac-platform.js")
    );
    if label == "main" && cfg!(debug_assertions) && std::env::args().any(|arg| arg == "--self-test")
    {
        initialization.push_str(include_str!("../../scripts/runtime-smoke.js"));
    }
    Ok(initialization)
}

fn refresh_geometry(app: &AppHandle, label: &str) -> Result<(), String> {
    let window = app
        .get_webview_window(label)
        .ok_or("Window geometry unavailable")?;
    let scale = window.scale_factor().map_err(err)?;
    let pos = window.inner_position().map_err(err)?;
    let state = app.state::<WindowState>();
    if label == "main" {
        *state.main_geometry.write().map_err(err)? = MainGeometry {
            left: pos.x as f64 / scale,
            top: pos.y as f64 / scale,
            scale,
        };
    } else if label != "toast" {
        let pos = window.outer_position().map_err(err)?;
        let size = window.outer_size().map_err(err)?;
        state.auxiliary.write().map_err(err)?.insert(
            label.to_string(),
            HitRegion {
                left: pos.x as f64 / scale,
                top: pos.y as f64 / scale,
                width: size.width as f64 / scale,
                height: size.height as f64 / scale,
                member_id: None,
            },
        );
    }
    Ok(())
}

fn build_window(
    app: &AppHandle,
    label: &str,
    route: &str,
    size: (f64, f64),
    focus: bool,
) -> Result<WebviewWindow, String> {
    let window = WebviewWindowBuilder::new(
        app,
        label,
        WebviewUrl::App(format!("index.html#{route}").into()),
    )
    .title(crate::profile::Region::from_app(app).name())
    .initialization_script(initialize(app, label)?)
    .on_page_load(|window, payload| {
        if matches!(payload.event(), tauri::webview::PageLoadEvent::Started) {
            crate::socket::close_window(window.app_handle(), window.label());
        }
    })
    .inner_size(size.0, size.1)
    .transparent(true)
    .decorations(false)
    .shadow(false)
    .resizable(label == "panel")
    .maximizable(false)
    .visible(false)
    .focused(focus)
    .focusable(focus)
    .always_on_top(label == "main")
    .disable_drag_drop_handler()
    .build()
    .map_err(err)?;
    if label == "main" {
        window.set_visible_on_all_workspaces(true).map_err(err)?;
    } else {
        // Belt and braces: borderless windows can still be grabbed by their edges
        // on some macOS versions, and the login dialog must never be resized by
        // hand (only the panel may be resized).
        window.set_resizable(label == "panel").map_err(err)?;
        if label == "panel" {
            // Only the panel is moved by dragging its background. The login
            // dialog is an ordinary form: background dragging there steals text
            // selection, so it gets a dedicated drag strip instead (see the
            // injected platform script).
            allow_background_drag(&window)?;
        }
    }
    if label == "panel" {
        window
            .set_min_size(Some(LogicalSize::new(1100., 760.)))
            .map_err(err)?;
    }
    window.on_window_event({
        let app = app.clone();
        let label = label.to_string();
        move |event| {
            if matches!(event, tauri::WindowEvent::Destroyed) {
                crate::socket::close_window(&app, &label);
            }
            if let tauri::WindowEvent::Moved(position) = event {
                if label == "login" {
                    // The login flow creates helper windows of its own (they are
                    // separate NSWindows, so they would stay behind when the
                    // login dialog is dragged). Keep them glued to it.
                    let next = (position.x, position.y);
                    let previous = app
                        .state::<WindowState>()
                        .login_position
                        .lock()
                        .ok()
                        .and_then(|mut slot| slot.replace(next));
                    if let Some((px, py)) = previous {
                        let (dx, dy) = (next.0 - px, next.1 - py);
                        if dx != 0 || dy != 0 {
                            for (name, window) in app.webview_windows() {
                                if matches!(
                                    name.as_str(),
                                    "main" | "login" | "panel" | "about" | "oss-notice" | "update" | "toast" | "bootstrap"
                                ) {
                                    continue;
                                }
                                if let Ok(current) = window.outer_position() {
                                    let _ = window.set_position(PhysicalPosition::new(
                                        current.x + dx,
                                        current.y + dy,
                                    ));
                                }
                            }
                        }
                    }
                }
                if label == "panel" {
                    if let Ok(mut slot) = app.state::<WindowState>().panel_position.lock() {
                        *slot = Some((position.x, position.y));
                    }
                }
            }
            if matches!(
                event,
                tauri::WindowEvent::Moved(_)
                    | tauri::WindowEvent::Resized(_)
                    | tauri::WindowEvent::ScaleFactorChanged { .. }
            ) {
                let tracked = label == "main"
                    || app
                        .state::<WindowState>()
                        .auxiliary
                        .read()
                        .is_ok_and(|windows| windows.contains_key(&label));
                if tracked {
                    if let Err(error) = refresh_geometry(&app, &label) {
                        eprintln!("Could not refresh {label} geometry: {error}");
                    }
                }
            }
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if let Ok(mut windows) = app.state::<WindowState>().auxiliary.write() {
                    windows.remove(&label);
                }
                if label == "login" {
                    // The login view keeps its own timer state (e.g. the SMS
                    // countdown) in JS memory. Hiding the window preserved that
                    // state, so reopening showed a stale countdown and a stuck
                    // loading mask. Destroy it instead: reopening starts clean,
                    // which is what the Windows build effectively does.
                    if let Err(error) = app.emit("cat-login-overlay", json!({"open":false})) {
                        eprintln!("Login close event failed: {error}");
                    }
                    // The login view creates helper windows of its own (they are
                    // opaque, unlike our transparent aux windows). Close them so
                    // no blank rectangle is left behind.
                    for (name, window) in app.webview_windows() {
                        if matches!(
                            name.as_str(),
                            "main" | "panel" | "about" | "oss-notice" | "update" | "toast" | "bootstrap"
                        ) {
                            continue;
                        }
                        let _ = window.close();
                    }
                    app.state::<WindowState>()
                        .login_ready
                        .store(false, Ordering::Release);
                    if let Some(window) = app.get_webview_window("login") {
                        if let Err(error) = window.destroy() {
                            eprintln!("Could not destroy login window: {error}");
                        }
                    }
                } else {
                    if let Some(w) = app.get_webview_window(&label) {
                        if let Err(error) = w.hide() {
                            eprintln!("Could not hide {label}: {error}");
                        }
                    }
                    if label == "panel" {
                        if let Err(error) = app.emit("cat-panel-window-closed", ()) {
                            eprintln!("Panel close event failed: {error}");
                        }
                        // Hand focus back to whatever the user was doing before
                        // the panel stole it, instead of leaving a frontmost app
                        // with no visible window.
                        if let Ok(mut slot) = app.state::<WindowState>().previous_app_pid.lock() {
                            if let Some(pid) = slot.take() {
                                activate_app(pid);
                            }
                        }
                    }
                }
            }
        }
    });
    if label == "login" {
        schedule_login_diagnostic(window.clone());
    }
    Ok(window)
}

/// Temporary diagnostic: while the domestic login view can stall on its own
/// loading overlay, report what the page actually contains so the failure can be
/// identified from `app.log` instead of guessed.
fn schedule_login_diagnostic(window: WebviewWindow) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(8));
        let script = r#"(function(){
  try {
    const body = document.body;
    const text = (body && body.innerText ? body.innerText : '').replace(/\s+/g, ' ').slice(0, 320);
    const marks = Array.from(document.querySelectorAll('[class*="load"],[class*="spin"],[class*="mask"]'))
      .slice(0, 6).map(node => String(node.className)).join(' | ');
    const iframes = document.querySelectorAll('iframe').length;
    const nodes = document.querySelectorAll('*').length;
    const cookieNames = (document.cookie || '').split(';').map(part => part.split('=')[0].trim()).filter(Boolean).slice(0, 8).join(',');
    window.__TAURI__.core.invoke('write_app_log', {
      level: 'info',
      message: `[login-diag] nodes=${nodes} iframes=${iframes} cookies=[${cookieNames}] marks=[${marks}] text=${text}`,
    }).catch(() => {});
  } catch (error) {
    window.__TAURI__.core.invoke('write_app_log', { level: 'error', message: `[login-diag] failed ${String(error)}` }).catch(() => {});
  }
})()"#;
        let _ = window.eval(script);
    });
}

/// Borderless auxiliary windows (panel, login, about…) cannot be dragged by a
/// title bar. On macOS `isMovableByWindowBackground` restores the Windows
/// behaviour where the dialog can be repositioned by dragging its background.
#[cfg(target_os = "macos")]
fn allow_background_drag(window: &WebviewWindow) -> Result<(), String> {
    use objc2_app_kit::NSWindow;
    let pointer = window.ns_window().map_err(|error| error.to_string())? as *mut NSWindow;
    if pointer.is_null() {
        return Err("window_ns_handle_unavailable".into());
    }
    // Windows are built from the main thread (see `dispatch`), so this stays on
    // the AppKit main thread; the handle is only used within this scope.
    unsafe {
        (*pointer).setMovableByWindowBackground(true);
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn allow_background_drag(_window: &WebviewWindow) -> Result<(), String> {
    Ok(())
}

fn show_aux(
    app: &AppHandle,
    label: &str,
    route: &str,
    size: (f64, f64),
) -> Result<(WebviewWindow, bool), String> {
    let existing = app.get_webview_window(label);
    let existed = existing.is_some();
    let window = match existing {
        Some(w) => w,
        None => build_window(app, label, route, size, true)?,
    };
    let is_panel = label == "panel";
    if is_panel {
        // Remember who was in front before the panel takes focus.
        let own_pid = std::process::id() as i32;
        let frontmost = frontmost_app_pid();
        if let Ok(mut slot) = app.state::<WindowState>().previous_app_pid.lock() {
            if frontmost.is_some_and(|pid| pid != own_pid) {
                *slot = frontmost;
            }
        }
    }
    let saved_position = if is_panel {
        app.state::<WindowState>()
            .panel_position
            .lock()
            .ok()
            .and_then(|slot| *slot)
    } else {
        None
    };
    match saved_position {
        Some((x, y)) => {
            window
                .set_position(PhysicalPosition::new(x, y))
                .map_err(err)?;
        }
        None => center_on_main(app, &window)?,
    }
    window.show().map_err(err)?;
    window.set_focus().map_err(err)?;
    refresh_geometry(app, label)?;
    Ok((window, existed))
}

#[cfg(target_os = "macos")]
fn frontmost_app_pid() -> Option<i32> {
    use objc2_app_kit::NSWorkspace;
    let workspace = unsafe { NSWorkspace::sharedWorkspace() };
    workspace
        .frontmostApplication()
        .map(|application| application.processIdentifier())
}

#[cfg(not(target_os = "macos"))]
fn frontmost_app_pid() -> Option<i32> {
    None
}

#[cfg(target_os = "macos")]
fn activate_app(pid: i32) {
    use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
    if let Some(application) =
        unsafe { NSRunningApplication::runningApplicationWithProcessIdentifier(pid) }
    {
        unsafe {
            application.activateWithOptions(NSApplicationActivationOptions::ActivateIgnoringOtherApps);
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn activate_app(_pid: i32) {}

fn center_on_main(app: &AppHandle, window: &WebviewWindow) -> Result<(), String> {
    let main = main_window(app)?;
    let monitor = main
        .current_monitor()
        .map_err(err)?
        .or(main.primary_monitor().map_err(err)?)
        .ok_or("No display is available")?;
    let size = window
        .outer_size()
        .map_err(err)?
        .to_logical::<f64>(window.scale_factor().map_err(err)?);
    let work = monitor.work_area();
    let scale = monitor.scale_factor();
    window
        .set_position(LogicalPosition::new(
            work.position.x as f64 / scale + (work.size.width as f64 / scale - size.width) / 2.,
            work.position.y as f64 / scale + (work.size.height as f64 / scale - size.height) / 2.,
        ))
        .map_err(err)
}

fn fullscreen_shell(app: &AppHandle) -> Result<(), String> {
    let w = main_window(app)?;
    let monitor = w
        .current_monitor()
        .map_err(err)?
        .or(w.primary_monitor().map_err(err)?)
        .ok_or("No display is available")?;
    w.set_position(*monitor.position()).map_err(err)?;
    w.set_size(*monitor.size()).map_err(err)?;
    Ok(())
}

fn safe_area(app: &AppHandle) -> Result<Value, String> {
    let w = main_window(app)?;
    let m = w
        .current_monitor()
        .map_err(err)?
        .ok_or("No current display")?;
    let p = m.position();
    let s = m.size();
    let a = m.work_area();
    let insets = work_area_insets(
        (p.x as f64, p.y as f64, s.width as f64, s.height as f64),
        (
            a.position.x as f64,
            a.position.y as f64,
            a.size.width as f64,
            a.size.height as f64,
        ),
        m.scale_factor(),
    );
    Ok(
        json!({"insets": insets, "autoHideEdge": null, "foregroundFullscreen": foreground_fullscreen(app)?}),
    )
}

// Other applications' window content is never inspected. macOS supplies the
// current Space's fullscreen presentation state through the application's menu.
fn foreground_fullscreen(_app: &AppHandle) -> Result<bool, String> {
    use objc2_app_kit::{NSApplication, NSApplicationPresentationOptions};
    let mtm =
        objc2::MainThreadMarker::new().ok_or("Screen state must be read on the main thread")?;
    Ok(NSApplication::sharedApplication(mtm)
        .currentSystemPresentationOptions()
        .contains(NSApplicationPresentationOptions::FullScreen))
}

fn payload(args: &Value) -> &Value {
    args.get("payload").unwrap_or(args)
}
fn number(args: &Value, name: &str) -> Result<f64, String> {
    args.get(name)
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite())
        .ok_or_else(|| format!("Invalid {name}"))
}

fn close(app: &AppHandle, label: &str) -> Result<Value, String> {
    app.state::<WindowState>()
        .auxiliary
        .write()
        .map_err(err)?
        .remove(label);
    if let Some(w) = app.get_webview_window(label) {
        w.hide().map_err(err)?;
    }
    if label == "panel" {
        app.emit("cat-panel-window-closed", ()).map_err(err)?;
    }
    if label == "login" {
        app.emit("cat-login-overlay", json!({"open": false}))
            .map_err(err)?;
    }
    Ok(Value::Null)
}

pub fn dispatch(
    app: &AppHandle,
    label: &str,
    command: &str,
    args: &Value,
) -> Result<Value, String> {
    let state = app.state::<WindowState>();
    match command {
        "open_about_window" => {
            let (w, existed) = show_aux(app, "about", "/about", (680., 700.))?;
            if existed {
                w.emit("about-open", ()).map_err(err)?;
            }
        }
        "open_oss_notice_window" => {
            let (w, existed) = show_aux(app, "oss-notice", "/oss-notice", (780., 680.))?;
            if existed {
                w.emit("oss-notice-open", ()).map_err(err)?;
            }
        }
        "open_login_window" => {
            let intent = args
                .get("intent")
                .and_then(Value::as_str)
                .unwrap_or("login");
            if !["login", "show"].contains(&intent) {
                return Err("Unknown login intent".into());
            }
            *state.pending_login.lock().map_err(err)? =
                Some(json!({"intent":intent,"compact":args.get("compact")}));
            let (w, _) = show_aux(app, "login", "/login", (600., 660.))?;
            app.emit("cat-login-overlay", json!({"open": true}))
                .map_err(err)?;
            if state.login_ready.load(Ordering::Acquire) {
                if let Some(pending) = state.pending_login.lock().map_err(err)?.take() {
                    w.emit("login-modal-open", pending).map_err(err)?;
                }
            }
        }
        "login_window_ready" => {
            if label != "login" {
                return Err("Login readiness requires the login window".into());
            }
            state.login_ready.store(true, Ordering::Release);
            if let Some(pending) = state.pending_login.lock().map_err(err)?.take() {
                app.get_webview_window("login")
                    .ok_or("Login window is unavailable")?
                    .emit("login-modal-open", pending)
                    .map_err(err)?;
            }
        }
        "open_panel_window" => {
            let tab = payload(args)
                .get("tab")
                .and_then(Value::as_str)
                .unwrap_or("bag");
            if !["bag", "atlas", "exchange", "room", "setting"].contains(&tab) {
                return Err("Unknown panel tab".into());
            }
            let (w, existed) = show_aux(app, "panel", &format!("/panel?tab={tab}"), (1100., 760.))?;
            if existed {
                w.emit("panel-open-tab", tab).map_err(err)?;
            }
        }
        "open_update_window" => {
            *state.update.lock().map_err(err)? = args.get("detail").cloned().unwrap_or(Value::Null);
            let (w, _) = show_aux(app, "update", "/update", (600., 500.))?;
            w.emit("update-modal-open", ()).map_err(err)?;
            w.emit(
                "cat-app-update-available",
                args.get("detail").cloned().unwrap_or(Value::Null),
            )
            .map_err(err)?;
        }
        "open_maintenance_notice" => {
            let notice = args.get("payload").cloned().filter(|v| !v.is_null());
            *state.maintenance.lock().map_err(err)? = Some(notice.clone().unwrap_or(Value::Null));
            let (w, _) = show_aux(app, "update", "/update", (600., 500.))?;
            w.emit("maintenance-notice-open", notice).map_err(err)?;
        }
        "close_about_window" => return close(app, "about"),
        "close_oss_notice_window" => return close(app, "oss-notice"),
        "close_login_window" => return close(app, "login"),
        "close_panel_window" => return close(app, "panel"),
        "close_update_window" => return close(app, "update"),
        "get_app_update_detail" => return Ok(state.update.lock().map_err(err)?.clone()),
        "get_maintenance_notice_payload" => {
            return Ok(state
                .maintenance
                .lock()
                .map_err(err)?
                .clone()
                .unwrap_or(Value::Null))
        }
        "has_maintenance_notice" => {
            return Ok(json!(state.maintenance.lock().map_err(err)?.is_some()))
        }
        "is_force_update_locked" => {
            let detail = state.update.lock().map_err(err)?;
            return Ok(json!(detail
                .get("forceUpdate")
                .or_else(|| detail.get("forced"))
                .and_then(Value::as_bool)
                .unwrap_or(false)));
        }
        "hide_main_after_maintenance_notice_ready" => main_window(app)?.hide().map_err(err)?,
        "resize_login_window_to_content" => {
            let p = payload(args);
            let width = number(p, "width")?;
            let height = number(p, "height")?;
            if width <= 0. || height <= 0. {
                return Err("Login dimensions must be positive".into());
            }
            let w = app
                .get_webview_window("login")
                .ok_or("Login window is unavailable")?;
            w.set_size(LogicalSize::new(width, height)).map_err(err)?;
            // Programmatic fitting must not leave the dialog user-resizable; the
            // form is not a free-form window.
            w.set_resizable(false).map_err(err)?;
            center_on_main(app, &w)?;
        }
        "raise_panel_window_above_pets" => {
            let w = app
                .get_webview_window("panel")
                .ok_or("Panel window is unavailable")?;
            // Keep the panel a regular window: raising it to the front of the
            // normal window order is enough, matching the Windows build. Forcing
            // `always_on_top` here would make the panel float above every other
            // application until it closes.
            w.set_always_on_top(false).map_err(err)?;
            w.set_focus().map_err(err)?;
        }
        "set_panel_window_collapsed" => {
            let w = app
                .get_webview_window("panel")
                .ok_or("Panel window is unavailable")?;
            let collapsed = payload(args)
                .get("collapsed")
                .and_then(Value::as_bool)
                .ok_or("Missing collapsed state")?;
            let mut previous = state.panel_collapsed.lock().map_err(err)?;
            if *previous != collapsed {
                let size = w
                    .inner_size()
                    .map_err(err)?
                    .to_logical::<f64>(w.scale_factor().map_err(err)?);
                let min_width = if collapsed { 958. } else { 1100. };
                w.set_min_size(Some(LogicalSize::new(min_width, 760.)))
                    .map_err(err)?;
                let width = (size.width + if collapsed { -142. } else { 142. }).max(min_width);
                w.set_size(LogicalSize::new(width, size.height.max(760.)))
                    .map_err(err)?;
                *previous = collapsed;
            }
        }
        "set_main_hit_region" => {
            let update: HitUpdate = serde_json::from_value(payload(args).clone()).map_err(err)?;
            if update
                .regions
                .iter()
                .chain(update.input_regions.iter())
                .any(|r| !r.is_valid())
            {
                return Err("Invalid hit region geometry".into());
            }
            let mut hit = state.hit.write().map_err(err)?;
            hit.regions = update.regions;
            if update.update_native_input {
                hit.input_regions = update.input_regions;
            }
            hit.window_passthrough = update.window_passthrough;
            hit.force_ignore = false;
        }
        "set_notice_region" => {
            let mut hit = state.hit.write().map_err(err)?;
            hit.notice = match payload(args).get("region").cloned().unwrap_or(Value::Null) {
                Value::Null => None,
                value => {
                    let region: HitRegion = serde_json::from_value(value).map_err(err)?;
                    if !region.is_valid() {
                        return Err("Invalid notice region geometry".into());
                    }
                    Some(region)
                }
            };
        }
        "set_main_force_cursor_ignore" => {
            state.hit.write().map_err(err)?.force_ignore = args
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or("Missing enabled state")?;
            main_window(app)?
                .set_ignore_cursor_events(true)
                .map_err(err)?;
        }
        "set_main_swallow_mouse" => {
            let enabled = args
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or("Missing enabled state")?;
            state.hit.write().map_err(err)?.swallow = enabled;
            if enabled {
                main_window(app)?
                    .set_ignore_cursor_events(false)
                    .map_err(err)?;
            }
        }
        "set_main_window_fullscreen_shell" => fullscreen_shell(app)?,
        "set_main_window_monitor_bounds" => {
            let p = payload(args);
            let x = number(p, "x")?;
            let y = number(p, "y")?;
            let width = number(p, "width")?;
            let height = number(p, "height")?;
            if width <= 0. || height <= 0. {
                return Err("Monitor dimensions must be positive".into());
            }
            let w = main_window(app)?;
            state.hit.write().map_err(err)?.force_ignore = true;
            w.set_ignore_cursor_events(true).map_err(err)?;
            let target = w
                .available_monitors()
                .map_err(err)?
                .into_iter()
                .find(|m| {
                    m.position().x == x as i32
                        && m.position().y == y as i32
                        && m.size().width == width as u32
                        && m.size().height == height as u32
                })
                .ok_or("Selected display is no longer available")?;
            let scale = target.scale_factor();
            w.set_position(LogicalPosition::new(x / scale, y / scale))
                .map_err(err)?;
            w.set_size(LogicalSize::new(width / scale, height / scale))
                .map_err(err)?;
        }
        "finish_main_window_monitor_switch" => {
            state.hit.write().map_err(err)?.force_ignore = false;
        }
        "set_main_window_content_bounds" => {
            let p = payload(args);
            let width = number(p, "width")?;
            let height = number(p, "height")?;
            if width <= 0. || height <= 0. {
                return Err("Window dimensions must be positive".into());
            }
            let w = main_window(app)?;
            if p.get("x").is_some() || p.get("y").is_some() {
                w.set_position(LogicalPosition::new(number(p, "x")?, number(p, "y")?))
                    .map_err(err)?;
            }
            w.set_size(LogicalSize::new(width, height)).map_err(err)?;
        }
        "apply_main_window_position" => {
            let top = payload(args)
                .get("alwaysOnTop")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            main_window(app)?.set_always_on_top(top).map_err(err)?;
            // In the original fullscreen shell the Vue dock, not the OS window,
            // applies the selected corner; moving this window would move it twice.
            fullscreen_shell(app)?;
        }
        "sync_main_window_to_current_monitor" => {
            let w = main_window(app)?;
            w.set_visible_on_all_workspaces(true).map_err(err)?;
        }
        "sync_aux_windows_to_main_monitor" => {
            for (name, w) in app.webview_windows() {
                if name != "main" && name != "toast" {
                    center_on_main(app, &w)?;
                }
            }
        }
        "get_main_desktop_safe_area" => return safe_area(app),
        "get_main_taskbar_insets" => return Ok(safe_area(app)?["insets"].clone()),
        "set_overlay_cursor" | "set_native_input_cursor" => {
            let kind = args
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("default");
            let cursor = match kind {
                "pointer" => tauri::CursorIcon::Hand,
                "grab" => tauri::CursorIcon::Grab,
                "grabbing" => tauri::CursorIcon::Grabbing,
                "default" => tauri::CursorIcon::Default,
                _ => return Err("Unsupported cursor".into()),
            };
            main_window(app)?.set_cursor_icon(cursor).map_err(err)?;
        }
        "ensure_main_taskbar_icon" => {
            app.set_dock_visibility(true).map_err(err)?;
            main_window(app)?.set_skip_taskbar(false).map_err(err)?;
        }
        "set_main_skip_taskbar" => {
            let skip = args.get("skip").and_then(Value::as_bool).ok_or("Missing skip state")?;
            app.set_dock_visibility(!skip).map_err(err)?;
            main_window(app)?.set_skip_taskbar(skip).map_err(err)?;
        }
        "show_toast_overlay" => {
            let w = if let Some(w) = app.get_webview_window("toast") {
                w
            } else {
                build_window(app, "toast", "/toast", (620., 160.), false)?
            };
            center_on_main(app, &w)?;
            w.set_ignore_cursor_events(true).map_err(err)?;
            w.show().map_err(err)?;
        }
        "sync_native_menu" => return crate::menu::sync(app, args),
        "popup_main_dock_context_menu" => {
            return crate::menu::popup(
                app,
                &main_window(app)?,
                number(args, "x")?,
                number(args, "y")?,
            )
        }
        "exit_app" => {
            state.stop.store(true, Ordering::Release);
            app.exit(0);
        }
        "open_input_monitoring_settings" => {
            // The in-window banner is the only persistent hint once the one-time
            // alert has been dismissed, so let the user jump straight to the pane.
            std::process::Command::new("open")
                .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent")
                .spawn()
                .map_err(|error| format!("input_monitoring_settings_failed: {error}"))?;
        }
        _ => return Err(format!("Unsupported native command {command} from {label}")),
    }
    Ok(Value::Null)
}

/// Ask once per launch while the permission is missing. A user who declines is
/// not interrupted again during the same run; the in-window notice and the menu
/// item remain as reminders.
fn ask_input_access_once(app: &AppHandle) -> bool {
    let Ok(dir) = app.path().app_data_dir() else { return true };
    mark_input_access_asked(&dir.join("input-permission-asked"))
}

/// The system only shows its Input Monitoring alert once per app identity, but
/// calling `CGRequestListenEventAccess` again re-opens it. Persist the marker so
/// a launch never re-asks: after the first attempt the menu item and the notice
/// button (which opens the exact Settings pane) are the only recovery paths.
fn mark_input_access_asked(marker: &std::path::Path) -> bool {
    if marker.exists() {
        return false;
    }
    if let Some(parent) = marker.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            eprintln!("Could not create the input access marker directory: {error}");
            return true;
        }
    }
    match std::fs::write(marker, b"1") {
        Ok(()) => true,
        Err(error) => {
            eprintln!("Could not record the input access request: {error}");
            true
        }
    }
}

pub fn setup(app: &AppHandle) -> Result<(), String> {
    let w = build_window(app, "main", "/", (900., 650.), false)?;
    fullscreen_shell(app)?;
    refresh_geometry(app, "main")?;
    w.set_ignore_cursor_events(true).map_err(err)?;
    w.show().map_err(err)?;
    if cfg!(debug_assertions) && std::env::args().any(|arg| arg == "--inspect") {
        w.open_devtools();
    }
    let app_for_input = app.clone();
    app.state::<crate::input::InputMonitor>()
        .set_member_resolver(move |x, y| {
            let state = app_for_input.state::<WindowState>();
            let geometry = state.main_geometry.read().ok()?;
            let (x, y) = (
                x / geometry.scale - geometry.left,
                y / geometry.scale - geometry.top,
            );
            let hit = state.hit.read().ok()?;
            hit.input_regions
                .iter()
                .chain(hit.regions.iter())
                .rev()
                .find(|r| r.contains(x, y))
                .and_then(|r| r.member_id)
        })?;
    let app_for_auxiliary = app.clone();
    app.state::<crate::input::InputMonitor>()
        .set_auxiliary_hit_test(move |x, y| {
            let state = app_for_auxiliary.state::<WindowState>();
            let Ok(geometry) = state.main_geometry.read() else {
                return false;
            };
            let (x, y) = (x / geometry.scale, y / geometry.scale);
            // The pet's overlay window floats above every auxiliary window, so
            // wherever it covers the panel the pet has to win. Without this the
            // overlap is claimed by the panel, which suppresses both the native
            // drag and the panel's own clicks — neither side responds.
            let over_pet = state.hit.read().is_ok_and(|hit| {
                let (local_x, local_y) = (x - geometry.left, y - geometry.top);
                hit.input_regions
                    .iter()
                    .chain(hit.regions.iter())
                    .any(|region| region.contains(local_x, local_y))
            });
            if over_pet {
                return false;
            }
            state
                .auxiliary
                .read()
                .is_ok_and(|regions| {
                    // Only the panel runs its own DOM click counter
                    // (`onMousedownCapture` → `pet-clap`). The login/about/update
                    // windows do not, so their rectangles must keep counting
                    // through the global tap instead of being suppressed here.
                    regions
                        .iter()
                        .any(|(label, region)| label == "panel" && region.contains(x, y))
                })
        })?;
    let app_for_permission = app.clone();
    app.listen("mac-request-input-access", move |_| {
        let app = app_for_permission.clone();
        let handle = app.clone();
        if let Err(error) = app.run_on_main_thread(move || {
            if let Err(error) = crate::input::request_input_access(handle) {
                eprintln!("Input access request failed: {error}");
            }
        }) {
            eprintln!("Could not schedule input access request: {error}");
        }
    });
    // Input Monitoring can only be granted by the user in System Settings. Ask
    // once at startup (that is what registers the app in the pane), then keep
    // watching so the tap starts as soon as the switch is enabled instead of
    // requiring a relaunch.
    std::thread::spawn({
        let app = app.clone();
        move || {
            use std::time::Duration;
            std::thread::sleep(Duration::from_millis(1500));
            if app.state::<WindowState>().stop.load(Ordering::Acquire) {
                return;
            }
            if !crate::input::listen_access_granted() && ask_input_access_once(&app) {
                let handle = app.clone();
                let scheduler = app.clone();
                if let Err(error) = scheduler.run_on_main_thread(move || {
                    // Bring the app forward so the system TCC prompt is visible,
                    // then let macOS show its own "…想接收来自任何应用程序的按键"
                    // dialog. No custom alert here: two prompts at once confused
                    // users and the native one is the authoritative one.
                    if let Some(mtm) = objc2::MainThreadMarker::new() {
                        objc2_app_kit::NSApplication::sharedApplication(mtm).activate();
                    }
                    if let Err(error) = crate::input::request_input_access(handle) {
                        eprintln!("Startup input access request failed: {error}");
                    }
                }) {
                    eprintln!("Could not schedule startup input request: {error}");
                }
            }
            let mut granted = crate::input::listen_access_granted();
            crate::menu::set_input_access_state(&app, granted);
            if granted {
                // Permission may already be granted when the app (re)starts; the
                // transition watcher alone would then never start the tap.
                let handle = app.clone();
                let scheduler = app.clone();
                if let Err(error) = scheduler.run_on_main_thread(move || {
                    if let Err(error) = crate::input::resume_and_publish(handle) {
                        eprintln!("Input monitor startup resume failed: {error}");
                    }
                }) {
                    eprintln!("Could not schedule input resume: {error}");
                }
            }
            loop {
                std::thread::sleep(Duration::from_millis(1500));
                if app.state::<WindowState>().stop.load(Ordering::Acquire) {
                    break;
                }
                let now = crate::input::listen_access_granted();
                if now != granted {
                    crate::menu::set_input_access_state(&app, now);
                }
                if now && !granted {
                    let handle = app.clone();
                    let scheduler = app.clone();
                    if let Err(error) = scheduler.run_on_main_thread(move || {
                        if let Err(error) = crate::input::resume_and_publish(handle) {
                            eprintln!("Input monitor resume failed: {error}");
                        }
                    }) {
                        eprintln!("Could not schedule input resume: {error}");
                    }
                }
                granted = now;
            }
        }
    });
    std::thread::spawn({
        let app = app.clone();
        move || {
            let mut previous = None;
            loop {
                if app.state::<WindowState>().stop.load(Ordering::Acquire) {
                    break;
                }
                if let Some(w) = app.get_webview_window("main") {
                    let result = (|| -> Result<(), String> {
                        let scale = w.scale_factor().map_err(err)?;
                        let origin = w.inner_position().map_err(err)?;
                        let cursor = w.cursor_position().map_err(err)?;
                        let primary_scale = w
                            .primary_monitor()
                            .map_err(err)?
                            .ok_or("No primary display")?
                            .scale_factor();
                        let x = cursor.x / primary_scale - origin.x as f64 / scale;
                        let y = cursor.y / primary_scale - origin.y as f64 / scale;
                        let state = app.state::<WindowState>();
                        let h = state.hit.read().map_err(err)?;
                        let ignored = ignores_cursor(&h, x, y);
                        drop(h);
                        if previous != Some(ignored) {
                            w.set_ignore_cursor_events(ignored).map_err(err)?;
                            previous = Some(ignored);
                        }
                        Ok(())
                    })();
                    if let Err(error) = result {
                        eprintln!("Hit region update stopped: {error}");
                        break;
                    }
                } else {
                    break;
                }
                std::thread::sleep(Duration::from_millis(16));
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(left: f64, top: f64, width: f64, height: f64) -> HitRegion {
        HitRegion { left, top, width, height, member_id: None }
    }

    #[test]
    fn input_access_is_requested_once_per_install() {
        let dir = std::env::temp_dir().join(format!("wuwa-asked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let marker = dir.join("input-permission-asked");
        assert!(mark_input_access_asked(&marker), "the first launch asks the system");
        assert!(marker.exists());
        assert!(!mark_input_access_asked(&marker), "later launches stay quiet");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn notice_region_receives_clicks_outside_the_pet() {
        let mut hit = HitState::default();
        hit.regions = vec![region(500., 500., 100., 100.)];
        assert!(ignores_cursor(&hit, 20., 60.), "the banner area must start click-through");
        hit.notice = Some(region(16., 52., 460., 44.));
        assert!(!ignores_cursor(&hit, 20., 60.), "the notice rectangle must accept the click");
        assert!(ignores_cursor(&hit, 20., 200.), "the rest of the overlay stays click-through");
    }

    #[test]
    fn clearing_the_notice_returns_it_to_click_through() {
        let mut hit = HitState::default();
        hit.notice = Some(region(16., 52., 460., 44.));
        assert!(!ignores_cursor(&hit, 20., 60.));
        hit.notice = None;
        assert!(ignores_cursor(&hit, 20., 60.));
    }

    #[test]
    fn window_passthrough_still_lets_the_notice_be_dismissed() {
        let mut hit = HitState::default();
        hit.window_passthrough = true;
        hit.notice = Some(region(16., 52., 460., 44.));
        assert!(!ignores_cursor(&hit, 20., 60.), "the close button must remain reachable");
    }

    #[test]
    fn forced_ignore_and_swallow_take_precedence_over_the_notice() {
        let mut hit = HitState {
            notice: Some(region(16., 52., 460., 44.)),
            ..HitState::default()
        };
        hit.force_ignore = true;
        assert!(ignores_cursor(&hit, 20., 60.));
        hit.force_ignore = false;
        hit.swallow = true;
        assert!(!ignores_cursor(&hit, 20., 60.));
    }
}
