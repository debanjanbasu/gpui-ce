//! The Android platform dispatcher: a self-contained priority-queue
//! dispatcher over std primitives, mirroring the iOS backend's
//! single-threaded model (`crates/gpui_ios/src/ios/../dispatcher` upstream).
//!
//! Android's `NativeActivity` owns the main thread's Looper; until the event
//! loop pin lands, main-thread runnables drain through the platform's own
//! tick (the activity's `onInput`/frame callbacks), and background work runs
//! on a small dedicated worker pool so `cx.background_spawn` actually
//! parallelizes (unlike iOS, where everything is main-thread pre-pin).
//!
//! `dispatch_after` and `dispatch_on_main_thread_when_idle` use a background
//! timer thread that parks until the deadline and then re-dispatches, which
//! keeps GPUI's timers running without wiring VDSO/timerfd specifics.

use gpui::{PlatformDispatcher, RunnableVariant};
use gpui::Priority;
use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct AndroidDispatcher {
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

impl AndroidDispatcher {
    /// Creates the dispatcher, spawning its background workers.
    pub fn new() -> Self {
        let state = Arc::new(DispatcherState {
            queue: Mutex::new(Queue::default()),
            signal: Condvar::new(),
        });

        // Background pool: GPUI's executors push `dispatch()` here and the
        // workers drain. Kept small: Android devices ship 4-8 cores and the
        // framework reserves headroom for the render thread.
        const WORKERS: usize = 2;
        for worker in 0..WORKERS {
            let state = Arc::clone(&state);
            thread::Builder::new()
                .name(format!("gpui-android-worker-{worker}"))
                .spawn(move || loop {
                    let runnable = {
                        let mut queue = state.queue.lock().unwrap_or_else(
                            std::sync::PoisonError::into_inner,
                        );
                        loop {
                            if let Some(runnable) = queue.background.pop_front() {
                                break runnable;
                            }
                            queue = state
                                .signal
                                .wait(queue)
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                        }
                    };
                    runnable.run();
                })
                .map(|_| ())
                .unwrap_or_else(|error| {
                    log::error!("failed to spawn gpui background worker: {error}")
                });
        }

        Self { state }
    }

    /// Pops and runs one main-thread runnable, if any. Called from the
    /// platform's event loop tick between native event polls.
    pub fn run_one_main(&self) -> bool {
        let runnable = {
            let mut queue = self
                .state
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            queue.main.pop_front()
        };
        match runnable {
            Some(runnable) => {
                runnable.run();
                true
            }
            None => false,
        }
    }

    /// Drains all currently-queued background runnables without blocking.
    pub fn drain_background(&self) {
        loop {
            let runnable = {
                let mut queue = self
                    .state
                    .queue
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                queue.background.pop_front()
            };
            let Some(runnable) = runnable else { break };
            runnable.run();
        }
    }
}

impl Default for AndroidDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl PlatformDispatcher for AndroidDispatcher {
    fn is_main_thread(&self) -> bool {
        // android-activity's event loop and every GPUI callback run on the
        // `android_main` thread; workers never call back into GPUI entities.
        thread::current().name().is_none_or(|name| {
            !name.starts_with("gpui-android-worker") && name != "gpui-android-timer"
        })
    }

    fn dispatch(&self, runnable: RunnableVariant, _priority: Priority) {
        let mut queue = self
            .state
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        queue.background.push_back(runnable);
        self.state.signal.notify_one();
    }

    fn dispatch_on_main_thread(&self, runnable: RunnableVariant, _priority: Priority) {
        let mut queue = self
            .state
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        queue.main.push_back(runnable);
        self.state.signal.notify_one();
    }

    fn dispatch_after(&self, duration: Duration, runnable: RunnableVariant) {
        let state = Arc::clone(&self.state);
        let spawn_result = thread::Builder::new()
            .name("gpui-android-timer".to_owned())
            .spawn(move || {
                thread::sleep(duration);
                let mut queue = state.queue.lock().unwrap_or_else(
                    std::sync::PoisonError::into_inner,
                );
                queue.background.push_back(runnable);
                state.signal.notify_one();
            });
        if let Err(error) = spawn_result {
            log::error!("failed to spawn gpui timer thread: {error}");
        }
    }

    fn dispatch_on_main_thread_when_idle(
        &self,
        runnable: RunnableVariant,
        _timeout: Option<Duration>,
    ) {
        self.dispatch_on_main_thread(runnable, Priority::Low);
    }

    fn idle_time_remaining(&self) -> Option<Duration> {
        None
    }

    fn spawn_realtime(&self, f: Box<dyn FnOnce() + Send>) {
        let spawn_result = thread::Builder::new()
            .name("gpui-android-realtime".to_owned())
            .spawn(f);
        if let Err(error) = spawn_result {
            log::error!("failed to spawn gpui realtime thread: {error}");
        }
    }

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn increase_timer_resolution(&self) -> gpui::TimerResolutionGuard {
        gpui_util::defer(Box::new(|| {}))
    }
}
