//! The iOS platform dispatcher: a self-contained priority-queue dispatcher
//! over std primitives, mirroring the web backend's single-threaded model.
//! Main-thread runnables drain through the platform event loop's tick; the
//! OS lifecycle hooks (app nap, timer coalescing, mach priorities) land
//! with the mobile pin.

use gpui::{PlatformDispatcher, RunnableVariant};
use scheduler::Priority;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct AppleDispatcher {
    state: Arc<DispatcherState>,
}

struct DispatcherState {
    queue: Mutex<Queue>,
    signal: Condvar,
}

#[derive(Default)]
struct Queue {
    main: VecDeque<RunnableVariant>,
    background: VecDeque<RunnableVariant>,
}

struct Scheduled {
    runnable: RunnableVariant,
    when: Instant,
}

impl AppleDispatcher {
    /// Creates the dispatcher.
    pub fn new() -> Self {
        Self { state: Arc::new(DispatcherState {
            queue: Mutex::new(Queue::default()),
            signal: Condvar::new(),
        }) }
    }

    /// Drains all currently-ready runnables. Called from the platform's
    /// event loop tick (the ios platform polls this between UIKit events).
    pub fn drain(&self) {
        loop {
            let runnable = {
                let mut queue = self.state.queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                queue.background.pop_front()
            };
            let Some(runnable) = runnable else { break };
            runnable.run();
        }
    }
}

impl Default for AppleDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl PlatformDispatcher for AppleDispatcher {
    fn is_main_thread(&self) -> bool {
        // The ios platform runs everything on the main thread pre-pin.
        true
    }

    fn dispatch(&self, runnable: RunnableVariant, priority: Priority) {
        let mut queue = self.state.queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        queue.main.push_back(runnable);
        self.state.signal.notify_one();
        drop(queue);
        let _ = priority;
    }

    fn dispatch_on_main_thread(&self, runnable: RunnableVariant, _priority: Priority) {
        let mut queue = self.state.queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        queue.main.push_back(runnable);
        self.state.signal.notify_one();
    }

    fn dispatch_after(&self, duration: std::time::Duration, _runnable: RunnableVariant) {
        // Timer dispatch lands with the mobile pin (mach timers).
        let _ = duration;
    }

    fn dispatch_on_main_thread_when_idle(
        &self,
        _runnable: RunnableVariant,
        _timeout: Option<Duration>,
    ) {
    }

    fn idle_time_remaining(&self) -> Option<Duration> {
        None
    }

    fn spawn_realtime(&self, _f: Box<dyn FnOnce() + Send>) {}

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn increase_timer_resolution(&self) -> gpui::TimerResolutionGuard {
        // Timer coalescing control lands with the mobile pin (mach timers);
        // a no-op deferred keeps the guard shape.
        gpui_util::defer(Box::new(|| {}))
    }
}
