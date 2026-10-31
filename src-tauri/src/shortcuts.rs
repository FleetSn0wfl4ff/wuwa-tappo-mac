//! Global desktop shortcuts: the macOS counterpart of the official Windows
//! `RegisterHotKey` bindings introduced in 1.0.3.
//!
//! The frontend owns the settings UI and the key recording; this module owns
//! validation, persistence and the system-wide registration. Bindings travel in
//! the official payload shape: a Windows virtual-key code plus a modifier
//! bitmask (`1` Alt, `2` Ctrl, `4` Shift, `8` Win). The bitmask is kept as-is so
//! the settings panel renders the user's own choice, while registration
//! translates each binding into a Carbon hot key.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{ffi::c_void, path::PathBuf, sync::atomic::AtomicBool};
use std::sync::{atomic::Ordering, Mutex};
use tauri::{AppHandle, Emitter, Manager};

pub const COMMANDS: &[&str] = &["get_app_shortcuts", "set_app_shortcuts", "set_shortcut_capture"];

/// Official Windows virtual-key codes for the F-row, which 1.0.3 ships as the
/// default bindings (F7 / F8 / F9). A Mac reaches them as Fn+F7 and up.
const VK_F7: u32 = 118;
const VK_F8: u32 = 119;
const VK_F9: u32 = 120;

const MOD_ALT: u32 = 1;
const MOD_CONTROL: u32 = 2;
const MOD_SHIFT: u32 = 4;
const MOD_COMMAND: u32 = 8;

/// `CGEventFlags` bits that carry modifier state on the observed keyboard
/// stream. Caps Lock and the secondary-Fn flag are deliberately excluded, so
/// Fn+F7 still matches an unmodified binding.
const FLAG_SHIFT: u64 = 0x0002_0000;
const FLAG_CONTROL: u64 = 0x0004_0000;
const FLAG_ALTERNATE: u64 = 0x0008_0000;
const FLAG_COMMAND: u64 = 0x0010_0000;

/// Carbon modifier mask (`cmdKey`/`shiftKey`/`optionKey`/`controlKey`).
const CARBON_COMMAND: u32 = 0x0100;
const CARBON_SHIFT: u32 = 0x0200;
const CARBON_OPTION: u32 = 0x0800;
const CARBON_CONTROL: u32 = 0x1000;

const SIGNATURE: u32 = 0x5754_504d; // 'WTPM'
const ACTION_TOGGLE_PET: &str = "togglePet";
const ACTION_GAME_MODE: &str = "gameMode";
const ACTION_SWITCH_SCREEN: &str = "switchScreen";
const HOTKEY_TOGGLE_PET: u32 = 1;
const HOTKEY_GAME_MODE: u32 = 2;
const HOTKEY_SWITCH_SCREEN: u32 = 3;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Binding {
    #[serde(default)]
    pub modifiers: u32,
    #[serde(default)]
    pub key: u32,
}

impl Binding {
    pub fn is_bound(&self) -> bool {
        self.key != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    pub game_mode: Binding,
    pub switch_screen: Binding,
    pub toggle_pet: Binding,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            game_mode: Binding { modifiers: 0, key: VK_F7 },
            switch_screen: Binding { modifiers: 0, key: VK_F8 },
            toggle_pet: Binding { modifiers: 0, key: VK_F9 },
        }
    }
}

impl Config {
    fn bindings(&self) -> [(&'static str, Binding); 3] {
        [
            (ACTION_TOGGLE_PET, self.toggle_pet),
            (ACTION_GAME_MODE, self.game_mode),
            (ACTION_SWITCH_SCREEN, self.switch_screen),
        ]
    }
}

/// Translate the official modifier bitmask into Carbon's hot key mask. The
/// official "Win" bit becomes Command on macOS; that is the only semantic
/// mismatch, and the settings panel keeps labelling it as the user typed it.
pub fn carbon_modifiers(modifiers: u32) -> u32 {
    let mut mask = 0;
    if modifiers & MOD_ALT != 0 {
        mask |= CARBON_OPTION;
    }
    if modifiers & MOD_CONTROL != 0 {
        mask |= CARBON_CONTROL;
    }
    if modifiers & MOD_SHIFT != 0 {
        mask |= CARBON_SHIFT;
    }
    if modifiers & MOD_COMMAND != 0 {
        mask |= CARBON_COMMAND;
    }
    mask
}

/// Derive the official modifier bitmask from observed `CGEventFlags`.
pub fn modifiers_from_flags(flags: u64) -> u32 {
    let mut modifiers = 0;
    if flags & FLAG_ALTERNATE != 0 {
        modifiers |= MOD_ALT;
    }
    if flags & FLAG_CONTROL != 0 {
        modifiers |= MOD_CONTROL;
    }
    if flags & FLAG_SHIFT != 0 {
        modifiers |= MOD_SHIFT;
    }
    if flags & FLAG_COMMAND != 0 {
        modifiers |= MOD_COMMAND;
    }
    modifiers
}

/// Map a Windows virtual-key code to a macOS virtual keycode (`kVK_*`).
///
/// Covers every key the official recorder can produce: letters, digits, F1–F24,
/// navigation keys and the punctuation row. Codes outside this set are reported
/// as `unavailable` instead of registering a silent no-op hot key.
pub fn macos_keycode(vk: u32) -> Option<u16> {
    let key = match vk {
        0x08 => 51,  // Backspace
        0x09 => 48,  // Tab
        0x0d => 36,  // Return
        0x1b => 53,  // Escape
        0x20 => 49,  // Space
        0x21 => 116, // PageUp
        0x22 => 121, // PageDown
        0x23 => 119, // End
        0x24 => 115, // Home
        0x25 => 123, // ArrowLeft
        0x26 => 126, // ArrowUp
        0x27 => 124, // ArrowRight
        0x28 => 125, // ArrowDown
        0x2d => 114, // Insert (Help occupies the slot on Mac)
        0x2e => 117, // Forward delete
        0x30 => 29,  // 0
        0x31 => 18,  // 1
        0x32 => 19,  // 2
        0x33 => 20,  // 3
        0x34 => 21,  // 4
        0x35 => 23,  // 5
        0x36 => 22,  // 6
        0x37 => 26,  // 7
        0x38 => 28,  // 8
        0x39 => 25,  // 9
        0x41 => 0,   // A
        0x42 => 11,  // B
        0x43 => 8,   // C
        0x44 => 2,   // D
        0x45 => 14,  // E
        0x46 => 3,   // F
        0x47 => 5,   // G
        0x48 => 4,   // H
        0x49 => 34,  // I
        0x4a => 38,  // J
        0x4b => 40,  // K
        0x4c => 37,  // L
        0x4d => 46,  // M
        0x4e => 45,  // N
        0x4f => 31,  // O
        0x50 => 35,  // P
        0x51 => 12,  // Q
        0x52 => 15,  // R
        0x53 => 1,   // S
        0x54 => 17,  // T
        0x55 => 32,  // U
        0x56 => 9,   // V
        0x57 => 13,  // W
        0x58 => 7,   // X
        0x59 => 16,  // Y
        0x5a => 6,   // Z
        0x70 => 122, // F1
        0x71 => 120, // F2
        0x72 => 99,  // F3
        0x73 => 118, // F4
        0x74 => 96,  // F5
        0x75 => 97,  // F6
        0x76 => 98,  // F7
        0x77 => 100, // F8
        0x78 => 101, // F9
        0x79 => 109, // F10
        0x7a => 103, // F11
        0x7b => 111, // F12
        0x7c => 105, // F13
        0x7d => 107, // F14
        0x7e => 113, // F15
        0x7f => 106, // F16
        0x80 => 64,  // F17
        0x81 => 79,  // F18
        0x82 => 80,  // F19
        0x83 => 90,  // F20
        // macOS exposes no virtual keycodes for F21–F24.
        0xba => 41, // ;
        0xbb => 24, // =
        0xbc => 43, // ,
        0xbd => 27, // -
        0xbe => 47, // .
        0xbf => 44, // /
        0xc0 => 50, // `
        0xdb => 33, // [
        0xdc => 42, // backslash
        0xdd => 30, // ]
        0xde => 39, // '
        _ => return None,
    };
    Some(key)
}

/// The action an observed key event would trigger, if any. Pins the matching
/// rules used by the native registration path.
pub fn action_for(config: &Config, keycode: u16, modifiers: u32) -> Option<&'static str> {
    config
        .bindings()
        .into_iter()
        .find(|(_, binding)| {
            binding.is_bound()
                && macos_keycode(binding.key) == Some(keycode)
                && binding.modifiers == modifiers
        })
        .map(|(action, _)| action)
}

/// Validate and canonicalise a binding set the way the settings panel expects:
/// an empty key clears the action, unknown keys are unusable, and two actions
/// may not share one combination.
pub fn normalize(mut config: Config) -> Result<Config, String> {
    for binding in [
        &mut config.game_mode,
        &mut config.switch_screen,
        &mut config.toggle_pet,
    ] {
        if !binding.is_bound() {
            *binding = Binding::default();
            continue;
        }
        if binding.modifiers & !(MOD_ALT | MOD_CONTROL | MOD_SHIFT | MOD_COMMAND) != 0 {
            return Err("unavailable".into());
        }
        if macos_keycode(binding.key).is_none() {
            return Err("unavailable".into());
        }
    }
    let mut seen: Vec<(u32, u32)> = Vec::new();
    for (_, binding) in config.bindings() {
        if !binding.is_bound() {
            continue;
        }
        let combination = (binding.modifiers, binding.key);
        if seen.contains(&combination) {
            return Err("duplicate".into());
        }
        seen.push(combination);
    }
    Ok(config)
}

// ---------------------------------------------------------------------------
// Carbon event hot keys
// ---------------------------------------------------------------------------

type OsStatus = i32;
type EventRef = *mut c_void;
type EventHandlerRef = *mut c_void;
type EventHandlerUPP = Option<unsafe extern "C" fn(*mut c_void, EventRef, *mut c_void) -> OsStatus>;
type EventTargetRef = *mut c_void;
type EventHotKeyRef = *mut c_void;

#[repr(C)]
struct EventHotKeyId {
    signature: u32,
    id: u32,
}

#[repr(C)]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

const K_EVENT_CLASS_KEYBOARD: u32 = 0x6b65_7962; // 'keyb'
const K_EVENT_HOT_KEY_PRESSED: u32 = 5;
const K_EVENT_PARAM_DIRECT_OBJECT: u32 = 0x2d2d_2d2d; // '----'
const TYPE_EVENT_HOT_KEY_ID: u32 = 0x686b_6964; // 'hkid'

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn GetApplicationEventTarget() -> EventTargetRef;
    fn InstallEventHandler(
        target: EventTargetRef,
        handler: EventHandlerUPP,
        num_types: usize,
        types: *const EventTypeSpec,
        user_data: *mut c_void,
        out_ref: *mut EventHandlerRef,
    ) -> OsStatus;
    fn RemoveEventHandler(handler: EventHandlerRef) -> OsStatus;
    fn RegisterEventHotKey(
        hot_key_code: u32,
        modifiers: u32,
        hot_key_id: EventHotKeyId,
        target: EventTargetRef,
        options: u32,
        out_ref: *mut EventHotKeyRef,
    ) -> OsStatus;
    fn UnregisterEventHotKey(hot_key: EventHotKeyRef) -> OsStatus;
    fn GetEventParameter(
        event: EventRef,
        name: u32,
        desired_type: u32,
        actual_type: *mut u32,
        buffer_size: usize,
        actual_size: *mut usize,
        data: *mut c_void,
    ) -> OsStatus;
}

struct HotkeyContext {
    app: AppHandle,
}

unsafe extern "C" fn hotkey_pressed(
    _call: *mut c_void,
    event: EventRef,
    user_data: *mut c_void,
) -> OsStatus {
    if user_data.is_null() || event.is_null() {
        return 0;
    }
    let context = unsafe { &*(user_data as *const HotkeyContext) };
    let mut id = EventHotKeyId {
        signature: 0,
        id: 0,
    };
    let status = unsafe {
        GetEventParameter(
            event,
            K_EVENT_PARAM_DIRECT_OBJECT,
            TYPE_EVENT_HOT_KEY_ID,
            std::ptr::null_mut(),
            std::mem::size_of::<EventHotKeyId>(),
            std::ptr::null_mut(),
            (&mut id as *mut EventHotKeyId).cast(),
        )
    };
    if status != 0 || id.signature != SIGNATURE {
        return 0;
    }
    let action = match id.id {
        HOTKEY_TOGGLE_PET => ACTION_TOGGLE_PET,
        HOTKEY_GAME_MODE => ACTION_GAME_MODE,
        HOTKEY_SWITCH_SCREEN => ACTION_SWITCH_SCREEN,
        _ => return 0,
    };
    // Delivered on the main thread; the pet shell only listens while it runs.
    let _ = context.app.emit("app-shortcut", action);
    0
}

#[derive(Default)]
struct Registration {
    handler: EventHandlerRef,
    context: *mut HotkeyContext,
    hotkeys: Vec<EventHotKeyRef>,
}

// The Carbon handler and hot key refs may only be touched from the main thread.
// Every caller reaches this module through the main-thread command dispatcher,
// and the mutex serialises those calls.
unsafe impl Send for Registration {}

impl Registration {
    fn unregister(&mut self) {
        for hotkey in std::mem::take(&mut self.hotkeys) {
            unsafe {
                UnregisterEventHotKey(hotkey);
            }
        }
    }

    fn ensure_handler(&mut self, app: &AppHandle) -> Result<(), String> {
        if !self.handler.is_null() {
            return Ok(());
        }
        let types = [EventTypeSpec {
            event_class: K_EVENT_CLASS_KEYBOARD,
            event_kind: K_EVENT_HOT_KEY_PRESSED,
        }];
        let context = Box::into_raw(Box::new(HotkeyContext { app: app.clone() }));
        let mut handler: EventHandlerRef = std::ptr::null_mut();
        let status = unsafe {
            InstallEventHandler(
                GetApplicationEventTarget(),
                Some(hotkey_pressed),
                1,
                types.as_ptr(),
                context.cast(),
                &mut handler,
            )
        };
        if status != 0 || handler.is_null() {
            unsafe {
                drop(Box::from_raw(context));
            }
            return Err("unavailable".into());
        }
        self.handler = handler;
        self.context = context;
        Ok(())
    }

    fn install(&mut self, app: &AppHandle, config: &Config) -> Result<(), String> {
        self.ensure_handler(app)?;
        let mut registered: Vec<EventHotKeyRef> = Vec::new();
        for (hotkey_id, binding) in [
            (HOTKEY_TOGGLE_PET, config.toggle_pet),
            (HOTKEY_GAME_MODE, config.game_mode),
            (HOTKEY_SWITCH_SCREEN, config.switch_screen),
        ] {
            if !binding.is_bound() {
                continue;
            }
            let Some(code) = macos_keycode(binding.key) else {
                for hotkey in registered {
                    unsafe {
                        UnregisterEventHotKey(hotkey);
                    }
                }
                return Err("unavailable".into());
            };
            let mut hotkey: EventHotKeyRef = std::ptr::null_mut();
            let status = unsafe {
                RegisterEventHotKey(
                    u32::from(code),
                    carbon_modifiers(binding.modifiers),
                    EventHotKeyId {
                        signature: SIGNATURE,
                        id: hotkey_id,
                    },
                    GetApplicationEventTarget(),
                    0,
                    &mut hotkey,
                )
            };
            // A non-zero status means the system (or another app) already owns
            // this combination; the settings panel shows it as occupied.
            if status != 0 || hotkey.is_null() {
                for hotkey in registered {
                    unsafe {
                        UnregisterEventHotKey(hotkey);
                    }
                }
                return Err("occupied".into());
            }
            registered.push(hotkey);
        }
        self.hotkeys = registered;
        Ok(())
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.unregister();
        if !self.handler.is_null() {
            unsafe {
                RemoveEventHandler(self.handler);
            }
            self.handler = std::ptr::null_mut();
        }
        if !self.context.is_null() {
            unsafe {
                drop(Box::from_raw(self.context));
            }
            self.context = std::ptr::null_mut();
        }
    }
}

pub struct ShortcutHost {
    config: Mutex<Config>,
    capturing: AtomicBool,
    active: AtomicBool,
    path: PathBuf,
    registration: Mutex<Registration>,
}

impl ShortcutHost {
    pub fn new(path: PathBuf) -> Self {
        Self {
            config: Mutex::new(Config::default()),
            capturing: AtomicBool::new(false),
            active: AtomicBool::new(false),
            path,
            registration: Mutex::new(Registration::default()),
        }
    }

    /// Read the stored bindings. A missing file keeps the official defaults; a
    /// malformed one is reported so the caller can log it without breaking boot.
    pub fn load(&self) -> Result<(), String> {
        let stored = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(format!("shortcuts_read_failed: {error}")),
        };
        let parsed: Config =
            serde_json::from_str(&stored).map_err(|error| format!("shortcuts_parse_failed: {error}"))?;
        let normalized = normalize(parsed)?;
        *self
            .config
            .lock()
            .map_err(|_| "shortcut_config_lock_poisoned".to_string())? = normalized;
        Ok(())
    }

    pub fn snapshot(&self) -> Result<Config, String> {
        self.config
            .lock()
            .map(|config| *config)
            .map_err(|_| "shortcut_config_lock_poisoned".to_string())
    }

    pub fn capturing(&self) -> bool {
        self.capturing.load(Ordering::Acquire)
    }

    /// Install the stored bindings once, without rewriting them. Called during
    /// setup so a saved combination works before the pet shell finishes booting.
    pub fn activate(&self, app: &AppHandle) -> Result<(), String> {
        let config = self.snapshot()?;
        let mut registration = self
            .registration
            .lock()
            .map_err(|_| "shortcut_registration_lock_poisoned".to_string())?;
        self.install(&mut registration, app, &config)
    }

    /// True when the combination belongs to a registered shortcut, so the
    /// global input stream must not count it as a pet interaction.
    pub fn matches(&self, keycode: u16, modifiers: u32) -> bool {
        if !self.active.load(Ordering::Acquire) || self.capturing() {
            return false;
        }
        match self.config.lock() {
            Ok(config) => action_for(&config, keycode, modifiers).is_some(),
            Err(_) => false,
        }
    }

    fn install(&self, registration: &mut Registration, app: &AppHandle, config: &Config) -> Result<(), String> {
        self.active.store(false, Ordering::Release);
        let result = registration.install(app, config);
        self.active.store(result.is_ok(), Ordering::Release);
        result
    }

    fn clear(&self, registration: &mut Registration) {
        registration.unregister();
        self.active.store(false, Ordering::Release);
    }

    /// Suspend or resume the global registration while the settings panel
    /// records a combination, mirroring the official capture mode.
    pub fn set_capture(&self, app: &AppHandle, active: bool) -> Result<(), String> {
        let config = self.snapshot()?;
        self.capturing.store(active, Ordering::Release);
        let mut registration = self
            .registration
            .lock()
            .map_err(|_| "shortcut_registration_lock_poisoned".to_string())?;
        self.clear(&mut registration);
        if active {
            return Ok(());
        }
        self.install(&mut registration, app, &config)
    }

    /// Replace the binding set: normalize, re-register, then persist. A failed
    /// registration restores the previous set so the user never loses working
    /// shortcuts to a partially applied change.
    pub fn apply(&self, app: &AppHandle, requested: Config) -> Result<Config, String> {
        let normalized = normalize(requested)?;
        let previous = self.snapshot()?;
        let capturing = self.capturing();
        let mut registration = self
            .registration
            .lock()
            .map_err(|_| "shortcut_registration_lock_poisoned".to_string())?;
        self.clear(&mut registration);
        if !capturing {
            if let Err(error) = self.install(&mut registration, app, &normalized) {
                let _ = self.install(&mut registration, app, &previous);
                return Err(error);
            }
        }
        if let Err(error) = persist(&self.path, &normalized) {
            self.clear(&mut registration);
            let _ = self.install(&mut registration, app, &previous);
            return Err(error);
        }
        drop(registration);
        *self
            .config
            .lock()
            .map_err(|_| "shortcut_config_lock_poisoned".to_string())? = normalized;
        Ok(normalized)
    }
}

fn persist(path: &PathBuf, config: &Config) -> Result<(), String> {
    let text = serde_json::to_string_pretty(config)
        .map_err(|error| format!("shortcuts_encode_failed: {error}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("shortcuts_directory_failed: {error}"))?;
    }
    std::fs::write(path, format!("{text}\n"))
        .map_err(|error| format!("shortcuts_write_failed: {error}"))
}

/// Main-thread command dispatcher for the shortcut surface.
pub fn dispatch(app: &AppHandle, command: &str, args: &Value) -> Result<Value, String> {
    let host = app.state::<std::sync::Arc<ShortcutHost>>();
    match command {
        "get_app_shortcuts" => Ok(json!(host.snapshot()?)),
        "set_shortcut_capture" => {
            let active = args.get("active").and_then(Value::as_bool).unwrap_or(false);
            host.set_capture(app, active)?;
            Ok(Value::Null)
        }
        "set_app_shortcuts" => {
            let config = args
                .get("config")
                .ok_or_else(|| "set_app_shortcuts requires a config payload".to_string())?;
            let requested: Config = serde_json::from_value(config.clone())
                .map_err(|error| {
                    eprintln!("[shortcuts] invalid config: {error}");
                    "unavailable".to_string()
                })?;
            let saved = host.apply(app, requested)?;
            app.emit("app-shortcuts-changed", saved)
                .map_err(|error| format!("shortcuts_delivery_failed: {error}"))?;
            Ok(json!(saved))
        }
        _ => Err(format!("Unsupported shortcut command {command}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_official_f7_f8_f9_bindings() {
        let config = Config::default();
        assert_eq!(config.game_mode, Binding { modifiers: 0, key: VK_F7 });
        assert_eq!(config.switch_screen, Binding { modifiers: 0, key: VK_F8 });
        assert_eq!(config.toggle_pet, Binding { modifiers: 0, key: VK_F9 });
        assert_eq!(macos_keycode(config.game_mode.key), Some(98));
        assert_eq!(macos_keycode(config.switch_screen.key), Some(100));
        assert_eq!(macos_keycode(config.toggle_pet.key), Some(101));
    }

    #[test]
    fn modifier_bitmask_matches_the_official_windows_layout() {
        assert_eq!(modifiers_from_flags(0), 0);
        assert_eq!(modifiers_from_flags(FLAG_ALTERNATE), 1);
        assert_eq!(modifiers_from_flags(FLAG_CONTROL), 2);
        assert_eq!(modifiers_from_flags(FLAG_SHIFT), 4);
        assert_eq!(modifiers_from_flags(FLAG_COMMAND), 8);
        assert_eq!(
            modifiers_from_flags(FLAG_CONTROL | FLAG_SHIFT | FLAG_COMMAND | 0x800000),
            2 | 4 | 8
        );
        assert_eq!(carbon_modifiers(1 | 2 | 4 | 8), CARBON_OPTION | CARBON_CONTROL | CARBON_SHIFT | CARBON_COMMAND);
    }

    #[test]
    fn fn_modified_function_keys_still_match_unmodified_bindings() {
        // Fn+F7 reports the secondary-Fn flag together with kVK_F7.
        let flags = 0x0080_0000;
        assert_eq!(modifiers_from_flags(flags), 0);
        assert_eq!(
            action_for(&Config::default(), 98, modifiers_from_flags(flags)),
            Some("gameMode")
        );
    }

    #[test]
    fn duplicate_and_unknown_bindings_are_rejected_before_registration() {
        let mut duplicate = Config::default();
        duplicate.toggle_pet = duplicate.game_mode;
        assert_eq!(normalize(duplicate).unwrap_err(), "duplicate");

        let mut unsupported = Config::default();
        unsupported.toggle_pet = Binding { modifiers: 0, key: 0xff };
        assert_eq!(normalize(unsupported).unwrap_err(), "unavailable");

        let mut bad_modifier = Config::default();
        bad_modifier.toggle_pet = Binding { modifiers: 16, key: VK_F9 };
        assert_eq!(normalize(bad_modifier).unwrap_err(), "unavailable");

        let cleared = Config {
            game_mode: Binding { modifiers: 0, key: 0 },
            switch_screen: Binding { modifiers: 0, key: 0 },
            toggle_pet: Binding { modifiers: 0, key: 0 },
        };
        assert_eq!(normalize(cleared).unwrap(), cleared);
        assert_eq!(action_for(&cleared, 98, 0), None);
    }

    #[test]
    fn rebinding_uses_the_recorded_key_and_modifier_pair() {
        let config = Config {
            game_mode: Binding { modifiers: 1 | 2, key: 0x47 }, // Ctrl+Alt+G
            ..Config::default()
        };
        assert_eq!(action_for(&config, 5, 3), Some("gameMode"));
        assert_eq!(action_for(&config, 5, 0), None, "missing modifiers must not fire");
        assert_eq!(action_for(&config, 98, 3), None, "wrong key must not fire");
        assert_eq!(action_for(&config, 101, 0), Some("togglePet"));
    }

    #[test]
    fn persisted_shape_round_trips_through_the_official_payload() {
        let json = serde_json::json!({
            "config": {"gameMode": {"modifiers": 0, "key": 118}, "togglePet": {"modifiers": 2, "key": 120}}
        });
        let parsed: Config = serde_json::from_value(json["config"].clone()).unwrap();
        assert_eq!(parsed.game_mode.key, VK_F7);
        assert_eq!(parsed.toggle_pet, Binding { modifiers: 2, key: VK_F9 });
        assert_eq!(parsed.switch_screen, Config::default().switch_screen);
        let encoded = serde_json::to_value(parsed).unwrap();
        assert_eq!(encoded["gameMode"]["key"], 118);
        assert_eq!(encoded["switchScreen"]["key"], VK_F8);
    }
}
