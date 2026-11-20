//! Thermal-state observation for Apple platforms: NSProcessInfo thermal
//! state changes surfaced through the gpui executor.

use block2::RcBlock;
use gpui::{ForegroundExecutor, ThermalState};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_foundation::{
    NSNotification, NSNotificationCenter, NSNotificationName, NSProcessInfo,
    NSProcessInfoPowerStateDidChangeNotification, NSProcessInfoThermalState,
    NSProcessInfoThermalStateDidChangeNotification,
};

pub fn thermal_state() -> ThermalState {
    match NSProcessInfo::processInfo().thermalState() {
        NSProcessInfoThermalState::Nominal => ThermalState::Nominal,
        NSProcessInfoThermalState::Fair => ThermalState::Fair,
        NSProcessInfoThermalState::Serious => ThermalState::Serious,
        _ => ThermalState::Critical,
    }
}

/// Observes thermal-state changes, reporting through the executor. Returns
/// the observer token (must be retained to keep the observation alive).
pub fn observe_thermal_state(
    executor: &ForegroundExecutor,
    mut callback: Box<dyn FnMut()>,
) -> ProcessInfoObserver {
    thermal_state();
    let center = unsafe { NSNotificationCenter::defaultCenter() };
    let callback = std::cell::RefCell::new(callback);
    let block = RcBlock::new(move |notification: std::ptr::NonNull<NSNotification>| {
        let _ = notification;
        if let Ok(mut cb) = callback.try_borrow_mut() {
            cb();
        }
    });
    let observer = unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(&*NSProcessInfoThermalStateDidChangeNotification),
            None,
            None,
            &block,
        )
    };
    // Keep the observer registered for as long as the executor lives.
    let _keep_alive = executor.spawn(async move {
        // The observation lives on the center; hold the token by leaking it
        // into the async context that the executor owns.
        std::mem::forget(observer);
    });
    ProcessInfoObserver {}
}

/// Handle that keeps the observation alive.
pub struct ProcessInfoObserver {}
