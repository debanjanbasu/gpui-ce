//! `NativeActivity` event-loop glue.
//!
//! GPUI construction is deferred until Android supplies an `ANativeWindow`:
//! `Application::run` opens its first window synchronously, so constructing it
//! earlier would make `AndroidPlatform::open_window` race the surface. Once
//! launched, this thread owns both Android's looper and GPUI's foreground
//! dispatcher.

use super::{ACTIVE_PLATFORM, AndroidDisplay, NATIVE_WINDOW};
use android_activity::{AndroidApp, MainEvent, PollEvent};
use gpui::AppLifecyclePhase;
use std::{ffi::c_void, ptr::NonNull, time::Duration};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// Runs one NativeActivity, invoking `launch` after its first surface exists.
///
/// The launch callback normally calls the application's ordinary `main`.
/// `AndroidPlatform::run` returns after registering GPUI's boot closure, so
/// this function resumes ownership of the native loop and pumps foreground
/// work for the remainder of the activity lifetime.
pub fn run(app: AndroidApp, launch: impl FnOnce()) {
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );

    let mut launch = Some(launch);
    let mut launched = false;
    let mut surface_ready = false;
    let mut destroyed = false;

    while !destroyed {
        app.poll_events(Some(FRAME_INTERVAL), |event| {
            let PollEvent::Main(event) = event else {
                return;
            };
            match event {
                MainEvent::InitWindow { .. } => {
                    set_native_window(&app, true);
                    update_display(&app);
                    surface_ready = true;
                    log::info!("GPUI Android native surface created");
                }
                MainEvent::TerminateWindow { .. } => {
                    set_native_window(&app, false);
                    surface_ready = false;
                    log::info!("GPUI Android native surface destroyed");
                }
                MainEvent::WindowResized { .. } | MainEvent::ConfigChanged { .. } => {
                    update_display(&app);
                }
                MainEvent::Start => emit_lifecycle(AppLifecyclePhase::Foreground),
                MainEvent::Resume { .. } | MainEvent::GainedFocus => {
                    emit_lifecycle(AppLifecyclePhase::Active);
                }
                MainEvent::Pause | MainEvent::LostFocus => {
                    emit_lifecycle(AppLifecyclePhase::Inactive);
                }
                MainEvent::Stop => emit_lifecycle(AppLifecyclePhase::Background),
                MainEvent::LowMemory => {
                    if let Some(state) = active_platform()
                        && let Some(callback) = state.memory_warning.lock().as_mut()
                    {
                        callback();
                    }
                }
                MainEvent::Destroy => {
                    destroyed = true;
                }
                _ => {}
            }
        });

        if destroyed {
            break;
        }

        if surface_ready && !launched {
            // Keep the native window alive for the whole boot. In particular,
            // wgpu creates its surface synchronously inside `open_window`.
            launch
                .take()
                .expect("Android launch callback invoked more than once")();
            launched = true;
            log::info!("GPUI Android application launched");
        }

        if !launched {
            continue;
        }

        if let Some(state) = active_platform() {
            // A runnable can enqueue follow-up work, so drain to a fixed point
            // rather than starving continuations until the next native event.
            while state.dispatcher.run_one_main() {}
        }
    }

    NATIVE_WINDOW.with(|slot| slot.set(NonNull::dangling()));
    ACTIVE_PLATFORM.with(|slot| *slot.borrow_mut() = None);
    log::info!("GPUI Android activity destroyed");
}

fn active_platform() -> Option<std::rc::Rc<super::AndroidPlatformState>> {
    ACTIVE_PLATFORM.with(|slot| slot.borrow().clone())
}

fn set_native_window(app: &AndroidApp, present: bool) {
    let pointer = if present {
        app.native_window()
            .map(|window| window.ptr().cast::<c_void>())
            .unwrap_or_else(NonNull::dangling)
    } else {
        NonNull::dangling()
    };
    NATIVE_WINDOW.with(|slot| slot.set(pointer));
    if let Some(state) = active_platform() {
        *state.native_window.lock() = pointer;
    }
}

fn update_display(app: &AndroidApp) {
    let Some(window) = app.native_window() else {
        return;
    };
    let density = app
        .config()
        .density()
        .map_or(1.0, |dpi| f32::max(dpi as f32 / 160.0, 0.01));
    AndroidDisplay::update(window.width(), window.height(), density);
}

fn emit_lifecycle(phase: AppLifecyclePhase) {
    if let Some(state) = active_platform()
        && let Some(callback) = state.app_lifecycle.lock().as_mut()
    {
        callback(phase);
    }
}
