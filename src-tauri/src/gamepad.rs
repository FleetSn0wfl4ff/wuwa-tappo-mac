//! Game controller button presses: the macOS counterpart of the official
//! Windows `Windows.Gaming.Input` bridge.
//!
//! The frontend listens for `gamepad-button-press` and owns the game-mode and
//! reporting gates. This module only reports physical button presses — face,
//! D-pad, shoulder, trigger, stick click and menu/option buttons — and never
//! treats thumbstick axis movement as a click.

use block2::RcBlock;
use objc2::{
    rc::Retained,
    runtime::{AnyObject, Bool, ProtocolObject},
    MainThreadMarker,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol, NSOperationQueue};
use objc2_game_controller::{
    GCController, GCControllerButtonInput, GCControllerDidConnectNotification,
    GCControllerDidDisconnectNotification, GCExtendedGamepad,
};
use std::{
    cell::RefCell,
    ptr::NonNull,
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tauri::{AppHandle, Emitter};

thread_local! {
    static MONITOR: RefCell<Option<Monitor>> = const { RefCell::new(None) };
}

struct Monitor {
    center: Retained<NSNotificationCenter>,
    observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
    controllers: BTreeMap<usize, Retained<GCController>>,
    /// Set to false on shutdown so already scheduled handler blocks stop
    /// delivering events into a closing application.
    active: Arc<AtomicBool>,
    previous_background: bool,
    app: AppHandle,
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
        for observer in &self.observers {
            let object: &AnyObject = (**observer).as_ref();
            unsafe { self.center.removeObserver(object) };
        }
        self.controllers.clear();
        unsafe { GCController::setShouldMonitorBackgroundEvents(self.previous_background) };
    }
}

fn buttons(gamepad: &GCExtendedGamepad) -> Vec<Retained<GCControllerButtonInput>> {
    let mut collected = unsafe {
        let dpad = gamepad.dpad();
        vec![
            gamepad.buttonA(),
            gamepad.buttonB(),
            gamepad.buttonX(),
            gamepad.buttonY(),
            dpad.up(),
            dpad.down(),
            dpad.left(),
            dpad.right(),
            gamepad.leftShoulder(),
            gamepad.rightShoulder(),
            gamepad.leftTrigger(),
            gamepad.rightTrigger(),
            gamepad.buttonMenu(),
        ]
    };
    unsafe {
        collected.extend(gamepad.buttonOptions());
        collected.extend(gamepad.leftThumbstickButton());
        collected.extend(gamepad.rightThumbstickButton());
    }
    // Some controllers alias two logical buttons onto one physical control;
    // deduplicate so one press cannot be counted twice.
    let mut seen = BTreeSet::new();
    collected.retain(|button| seen.insert(Retained::as_ptr(button) as usize));
    collected
}

fn attach(controller: &Retained<GCController>, app: &AppHandle, active: &Arc<AtomicBool>) {
    let Some(gamepad) = (unsafe { controller.extendedGamepad() }) else {
        return;
    };
    for button in buttons(&gamepad) {
        // A button already held when the controller connects must be released
        // before it contributes another click.
        let pressed = Arc::new(AtomicBool::new(unsafe { button.isPressed() }));
        let active = active.clone();
        let app = app.clone();
        let handler = RcBlock::new(
            move |_: NonNull<GCControllerButtonInput>, _: f32, down: Bool| {
                let was_pressed = pressed.swap(down.as_bool(), Ordering::AcqRel);
                if was_pressed || !down.as_bool() || !active.load(Ordering::Acquire) {
                    return;
                }
                // The controller remains usable by the foreground game; this
                // event only feeds the pet's own click counter.
                if let Err(error) = app.emit("gamepad-button-press", ()) {
                    eprintln!("Game controller event delivery failed: {error}");
                }
            },
        );
        unsafe { button.setPressedChangedHandler(RcBlock::as_ptr(&handler)) };
    }
}

fn refresh(monitor: &mut Monitor) {
    debug_assert!(MainThreadMarker::new().is_some());
    let controllers = unsafe { GCController::controllers() };
    let connected: BTreeSet<usize> = controllers
        .iter()
        .map(|controller| Retained::as_ptr(&controller) as usize)
        .collect();
    monitor.controllers.retain(|id, _| connected.contains(id));
    for controller in controllers.iter() {
        let id = Retained::as_ptr(&controller) as usize;
        if monitor.controllers.contains_key(&id) {
            continue;
        }
        attach(&controller, &monitor.app, &monitor.active);
        monitor.controllers.insert(id, controller);
    }
}

/// Register for controller connect/disconnect notifications. Must run on the
/// main thread: the notification queue and controller discovery are
/// thread-affine.
pub fn start(app: AppHandle) -> Result<(), String> {
    MainThreadMarker::new().ok_or("Game controllers must start on the main thread")?;
    if MONITOR.with(|slot| slot.borrow().is_some()) {
        return Ok(());
    }
    let center = NSNotificationCenter::defaultCenter();
    let active = Arc::new(AtomicBool::new(true));
    let previous_background = unsafe { GCController::shouldMonitorBackgroundEvents() };
    // A desktop pet must keep counting while another application is frontmost.
    // This observes presses without capturing or suppressing the game's input.
    unsafe { GCController::setShouldMonitorBackgroundEvents(true) };

    MONITOR.with(|slot| {
        *slot.borrow_mut() = Some(Monitor {
            center: center.clone(),
            observers: Vec::new(),
            controllers: BTreeMap::new(),
            active: active.clone(),
            previous_background,
            app: app.clone(),
        });
    });

    for name in unsafe {
        [
            GCControllerDidConnectNotification,
            GCControllerDidDisconnectNotification,
        ]
    } {
        let handler = RcBlock::new(move |_: NonNull<NSNotification>| {
            MONITOR.with(|slot| {
                if let Some(monitor) = slot.borrow_mut().as_mut() {
                    if monitor.active.load(Ordering::Acquire) {
                        refresh(monitor);
                    }
                }
            });
        });
        let observer = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(name),
                None,
                Some(&NSOperationQueue::mainQueue()),
                &handler,
            )
        };
        MONITOR.with(|slot| {
            if let Some(monitor) = slot.borrow_mut().as_mut() {
                monitor.observers.push(observer);
            }
        });
    }

    // Controllers that were connected before the app started never emit a
    // connect notification, so attach the current set explicitly.
    MONITOR.with(|slot| {
        if let Some(monitor) = slot.borrow_mut().as_mut() {
            refresh(monitor);
        }
    });
    Ok(())
}

/// Detach observers, drop controller handlers and restore the system default.
pub fn stop() -> Result<(), String> {
    if MainThreadMarker::new().is_none() {
        return Err("Game controllers must stop on the main thread".into());
    }
    MONITOR.with(|slot| slot.borrow_mut().take());
    Ok(())
}
