//! Native Android platform support for GPUI.
//!
//! Implements the [`gpui::Platform`] trait over `android-activity`
//! (NativeActivity): the activity's event loop drives pumping, the wgpu
//! renderer (Vulkan first, GLES fallback — gpui_wgpu's preference for
//! non-Apple/non-Windows targets) draws into the activity surface, and text
//! runs through the shared Parley/Swash stack (Android guarantees no system
//! font database — the app bundles its fonts).

use dispatcher::AndroidDispatcher;
use display::AndroidDisplay;
use window::AndroidWindow;

use gpui::{
    Action, AnyWindowHandle, AppLifecyclePhase, BackgroundExecutor, ClipboardItem,
    DummyKeyboardMapper, ForegroundExecutor, Keymap, Menu, MenuItem, OwnedMenu, PathPromptOptions,
    Platform, PlatformDisplay, PlatformKeyboardLayout, PlatformKeyboardMapper, PlatformTextSystem,
    PlatformWindow, Result, Task, ThermalState, WindowAppearance, WindowParams,
};
use gpui_parley::ParleyTextSystem;
use gpui_render::SwashGlyphRasterizer;
use parking_lot::Mutex;
use std::{
    ptr::NonNull,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

mod dispatcher;
mod display;
mod window;

pub(crate) struct AndroidPlatformState {
    dispatcher: Arc<AndroidDispatcher>,
    text_system: Arc<dyn PlatformTextSystem>,
    active_window: Mutex<Option<AnyWindowHandle>>,
    displays_changed: Mutex<Option<Box<dyn FnMut()>>>,
    app_lifecycle: Mutex<Option<Box<dyn FnMut(AppLifecyclePhase)>>>,
    memory_warning: Mutex<Option<Box<dyn FnMut()>>>,
    /// The activity's `ANativeWindow` pointer, set by the event loop on
    /// `onNativeWindowCreated` and cleared on `onNativeWindowDestroyed`.
    /// Null (dangling) until the first native window arrives.
    native_window: Mutex<NonNull<std::ffi::c_void>>,
}

impl AndroidPlatformState {
    /// The current `ANativeWindow` pointer. Panics before the first
    /// `onNativeWindowCreated` (dangling sentinel).
    pub(crate) fn native_window(&self) -> NonNull<std::ffi::c_void> {
        let window = *self.native_window.lock();
        assert!(
            window != NonNull::dangling(),
            "GPUI Android: open_window called before the NativeActivity surface exists; \
             the event loop must deliver onNativeWindowCreated first"
        );
        window
    }
}

/// The Android platform: one display, one fullscreen window, NativeActivity
/// lifecycle.
pub struct AndroidPlatform(Rc<AndroidPlatformState>);

impl AndroidPlatform {
    /// Creates the platform with its dispatcher and text system.
    pub fn new() -> Self {
        let dispatcher = Arc::new(AndroidDispatcher::new());
        let text_system: Arc<dyn PlatformTextSystem> =
            Arc::new(ParleyTextSystem::new(SwashGlyphRasterizer::default()));
        Self(Rc::new(AndroidPlatformState {
            dispatcher,
            text_system,
            active_window: Mutex::new(None),
            displays_changed: Mutex::new(None),
            app_lifecycle: Mutex::new(None),
            memory_warning: Mutex::new(None),
            native_window: Mutex::new(NonNull::dangling()),
        }))
    }

    pub(crate) fn state(&self) -> &Rc<AndroidPlatformState> {
        &self.0
    }
}

impl Default for AndroidPlatform {
    fn default() -> Self {
        Self::new()
    }
}

/// A simple Android keyboard layout (the soft keyboard owns real input).
struct AndroidKeyboardLayout;

impl PlatformKeyboardLayout for AndroidKeyboardLayout {
    fn id(&self) -> &str {
        "android-default"
    }

    fn name(&self) -> &str {
        "Android Default"
    }
}

impl Platform for AndroidPlatform {
    fn background_executor(&self) -> BackgroundExecutor {
        BackgroundExecutor::new(self.0.dispatcher.clone())
    }

    fn foreground_executor(&self) -> ForegroundExecutor {
        ForegroundExecutor::new(self.0.dispatcher.clone())
    }

    fn text_system(&self) -> Arc<dyn PlatformTextSystem> {
        self.0.text_system.clone()
    }

    fn run(&self, on_finish_launching: Box<dyn 'static + FnOnce()>) {
        // The NativeActivity glue (android_main → this platform) owns the
        // loop; gpui's `Application::run` hands the boot closure over and
        // the activity's event callbacks drive everything after it.
        on_finish_launching();
    }

    fn quit(&self) {
        // Android apps cannot programmatically quit; the system or the user
        // terminates them.
        log::warn!("Android apps cannot programmatically quit");
    }

    fn restart(&self, _binary_path: Option<PathBuf>, _arguments: Vec<std::ffi::OsString>) {
        log::warn!("Android apps cannot restart themselves");
    }

    fn activate(&self, _ignoring_other_apps: bool) {
        // Single-Activity apps are always active when visible.
    }

    fn hide(&self) {
        // Android apps cannot hide themselves.
    }

    fn hide_other_apps(&self) {}
    fn unhide_other_apps(&self) {}

    fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>> {
        vec![Rc::new(AndroidDisplay::primary())]
    }

    fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        Some(Rc::new(AndroidDisplay::primary()))
    }

    fn active_window(&self) -> Option<AnyWindowHandle> {
        *self.0.active_window.lock()
    }

    fn open_window(
        &self,
        handle: AnyWindowHandle,
        options: WindowParams,
    ) -> anyhow::Result<Box<dyn PlatformWindow>> {
        let window = Box::new(AndroidWindow::new(handle, options, self.0.native_window())?);
        *self.0.active_window.lock() = Some(handle);
        Ok(window)
    }

    fn window_appearance(&self) -> WindowAppearance {
        // The Activity reports the system dark-mode flag through the
        // configuration; until the mobile pin reads it, report Light.
        WindowAppearance::Light
    }

    fn open_url(&self, _url: &str) {
        // Lands with the mobile pin (JNI Intent/ACTION_VIEW).
    }

    fn on_open_urls(&self, _callback: Box<dyn FnMut(Vec<String>)>) {
        // Lands with the mobile pin (onNewIntent forwarding).
    }

    fn register_url_scheme(&self, _url: &str) -> Task<Result<()>> {
        // Android URL schemes are declared in the manifest's intent filters.
        Task::ready(Ok(()))
    }

    fn prompt_for_paths(
        &self,
        _options: PathPromptOptions,
    ) -> futures::channel::oneshot::Receiver<Result<Option<Vec<PathBuf>>>> {
        let (tx, rx) = futures::channel::oneshot::channel();
        if tx
            .send(Err(anyhow::anyhow!(
                "File picker not yet implemented for Android"
            )))
            .is_err()
        {
            log::debug!("GPUI Android: file picker receiver was dropped");
        }
        rx
    }

    fn prompt_for_new_path(
        &self,
        _directory: &Path,
        _suggested_name: Option<&str>,
    ) -> futures::channel::oneshot::Receiver<Result<Option<PathBuf>>> {
        let (tx, rx) = futures::channel::oneshot::channel();
        if tx
            .send(Err(anyhow::anyhow!(
                "Save dialog not yet implemented for Android"
            )))
            .is_err()
        {
            log::debug!("GPUI Android: save dialog receiver was dropped");
        }
        rx
    }

    fn can_select_mixed_files_and_dirs(&self) -> bool {
        false
    }

    fn reveal_path(&self, _path: &Path) {
        // No desktop file manager to reveal into.
    }

    fn open_with_system(&self, _path: &Path) {
        // Lands with the mobile pin (JNI ACTION_VIEW on the mime type).
    }

    fn on_quit(&self, mut callback: Box<dyn FnMut() -> bool>) {
        // Android termination cannot be vetoed; run the callback once for
        // its side effects only.
        callback();
    }

    fn on_reopen(&self, _callback: Box<dyn FnMut()>) {}
    fn on_system_wake(&self, _callback: Box<dyn FnMut()>) {}

    fn on_app_lifecycle(&self, callback: Box<dyn FnMut(AppLifecyclePhase)>) {
        *self.0.app_lifecycle.lock() = Some(callback);
    }

    fn on_memory_warning(&self, callback: Box<dyn FnMut()>) {
        *self.0.memory_warning.lock() = Some(callback);
    }

    fn set_menus(&self, _menus: Vec<Menu>, _keymap: &Keymap) {
        // Android has no menu bar.
    }

    fn set_dock_menu(&self, _menu: Vec<MenuItem>, _keymap: &Keymap) {
        // Android has no dock.
    }

    fn on_app_menu_action(&self, _callback: Box<dyn FnMut(&dyn Action)>) {
        // Not applicable on Android.
    }

    fn on_will_open_app_menu(&self, _callback: Box<dyn FnMut()>) {}

    fn on_validate_app_menu_command(&self, _callback: Box<dyn FnMut(&dyn Action) -> bool>) {}

    fn get_menus(&self) -> Option<Vec<OwnedMenu>> {
        None
    }

    fn thermal_state(&self) -> ThermalState {
        ThermalState::Nominal
    }

    fn on_thermal_state_change(&self, _callback: Box<dyn FnMut()>) {
        // Lands with the mobile pin (power + thermal intents).
    }

    fn keyboard_layout(&self) -> Box<dyn PlatformKeyboardLayout> {
        Box::new(AndroidKeyboardLayout)
    }

    fn on_keyboard_layout_change(&self, _callback: Box<dyn FnMut()>) {
        // The soft keyboard owns input; layout changes are invisible here.
    }

    fn keyboard_mapper(&self) -> Rc<dyn PlatformKeyboardMapper> {
        Rc::new(DummyKeyboardMapper)
    }

    fn read_from_clipboard(&self) -> Option<ClipboardItem> {
        // Lands with the mobile pin (JNI ClipboardManager).
        None
    }

    fn write_to_clipboard(&self, _item: ClipboardItem) {
        // Lands with the mobile pin (JNI ClipboardManager).
    }

    fn write_credentials(&self, _url: &str, _username: &str, _password: &[u8]) -> Task<Result<()>> {
        Task::ready(Err(anyhow::anyhow!("credentials not supported on Android")))
    }

    fn read_credentials(&self, _url: &str) -> Task<Result<Option<(String, Vec<u8>)>>> {
        Task::ready(Err(anyhow::anyhow!("credentials not supported on Android")))
    }

    fn delete_credentials(&self, _url: &str) -> Task<Result<()>> {
        Task::ready(Err(anyhow::anyhow!("credentials not supported on Android")))
    }

    fn app_path(&self) -> Result<PathBuf> {
        Ok(PathBuf::from("/"))
    }

    fn path_for_auxiliary_executable(&self, _name: &str) -> Result<PathBuf> {
        anyhow::bail!("auxiliary executables are not supported on Android")
    }

    fn set_cursor_style(&self, _style: gpui::CursorStyle) {
        // Touch surface; there is no mouse cursor.
    }

    fn hide_cursor_until_mouse_moves(&self) {
        // Touch surface; there is no mouse cursor.
    }

    fn is_cursor_visible(&self) -> bool {
        false
    }

    fn should_auto_hide_scrollbars(&self) -> bool {
        true
    }
}
