//! Android window implementation backed by the `NativeActivity` surface.
//!
//! Android windows are fundamentally different from desktop windows:
//! - Exactly one fullscreen surface, owned by the Activity (`ANativeWindow`).
//! - No title bar, no chrome, no window decorations of any kind.
//! - Touch-driven input (delivered by the platform's event loop).
//! - `WindowInsets` from the system bars / display cutout / IME flow through
//!   [`AndroidWindow::set_insets`], called from the platform loop.

use super::display::AndroidDisplay;
use gpui::{
    AnyWindowHandle, Bounds, Capslock, DevicePixels, DispatchEventResult, GpuSpecs, Modifiers,
    Pixels, PlatformAtlas, PlatformDisplay, PlatformInput, PlatformInputHandler, PlatformWindow,
    Point, PromptButton, PromptLevel, RequestFrameOptions, Scene, Size, WindowAppearance,
    WindowBackgroundAppearance, WindowBounds, WindowControlArea, WindowInsets, WindowParams, px,
    size,
};
use gpui_ce_wgpu::{GpuContext, WgpuRenderer, WgpuSurfaceConfig};
use parking_lot::Mutex;
use raw_window_handle::{
    AndroidDisplayHandle, AndroidNdkWindowHandle, DisplayHandle, HasDisplayHandle,
    HasWindowHandle, RawDisplayHandle, RawWindowHandle, WindowHandle,
};
use std::{
    ptr::NonNull,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicI32, Ordering},
    },
};

/// The raw-window-handle surface the wgpu renderer presents to.
///
/// The pointer borrows from the `ANativeWindow` the activity holds while the
/// surface is live; the renderer only dereferences it during surface
/// creation, which happens between `onNativeWindowCreated` and
/// `onNativeWindowDestroyed`.
#[derive(Debug, Clone, Copy)]
struct NativeSurface {
    native_window: NonNull<std::ffi::c_void>,
}

// Safety: the `ANativeWindow` pointer is valid for the window's lifetime and
// only handed to wgpu for surface creation; see the struct docs.
unsafe impl Send for NativeSurface {}
unsafe impl Sync for NativeSurface {}

impl HasWindowHandle for NativeSurface {
    fn window_handle(&self) -> Result<WindowHandle<'_>, raw_window_handle::HandleError> {
        let handle = AndroidNdkWindowHandle::new(self.native_window);
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::AndroidNdk(handle)) })
    }
}

impl HasDisplayHandle for NativeSurface {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, raw_window_handle::HandleError> {
        let handle = AndroidDisplayHandle::new();
        Ok(unsafe { DisplayHandle::borrow_raw(RawDisplayHandle::Android(handle)) })
    }
}

#[derive(Default)]
struct WindowCallbacks {
    request_frame: Option<Box<dyn FnMut(RequestFrameOptions)>>,
    input: Option<Box<dyn FnMut(PlatformInput) -> DispatchEventResult>>,
    active_status_change: Option<Box<dyn FnMut(bool)>>,
    hover_status_change: Option<Box<dyn FnMut(bool)>>,
    resize: Option<Box<dyn FnMut(Size<Pixels>, f32)>>,
    moved: Option<Box<dyn FnMut()>>,
    should_close: Option<Box<dyn FnMut() -> bool>>,
    close: Option<Box<dyn FnOnce()>>,
    appearance_changed: Option<Box<dyn FnMut()>>,
}

struct WindowState {
    bounds: Bounds<Pixels>,
    scale_factor: f32,
    title: String,
    input_handler: Option<PlatformInputHandler>,
    is_active: bool,
    appearance: WindowAppearance,
    insets: WindowInsets,
    mouse_position: Point<Pixels>,
    modifiers: Modifiers,
}

pub(crate) struct AndroidWindow {
    _handle: AnyWindowHandle,
    native_window: Mutex<NativeSurface>,
    /// The wgpu device/adapter slot shared across windows of the process.
    gpu_context: GpuContext,
    renderer: Mutex<Option<WgpuRenderer>>,
    state: Mutex<WindowState>,
    callbacks: Mutex<WindowCallbacks>,
    sprite_atlas: Arc<dyn PlatformAtlas>,
    /// Physical buffer size the renderer was last configured for, tracked
    /// atomically so `draw` can reconfigure the swapchain after a resize.
    configured_width: AtomicI32,
    configured_height: AtomicI32,
}

impl AndroidWindow {
    /// Creates the platform window for `handle` from the activity's current
    /// native window. Called by [`super::AndroidPlatform::open_window`] on
    /// the main thread; `native_window` comes from `onNativeWindowCreated`.
    pub(crate) fn new(
        handle: AnyWindowHandle,
        params: WindowParams,
        native_window: NonNull<std::ffi::c_void>,
    ) -> anyhow::Result<Self> {
        let (physical_width, physical_height) = AndroidDisplay::physical_size();
        let scale_factor = AndroidDisplay::scale_factor();

        let surface = NativeSurface { native_window };
        let gpu_context: GpuContext = Rc::new(std::cell::RefCell::new(None));
        let renderer = WgpuRenderer::new(
            Rc::clone(&gpu_context),
            &surface,
            WgpuSurfaceConfig {
                size: Size {
                    width: DevicePixels(physical_width),
                    height: DevicePixels(physical_height),
                },
                transparent: false,
                preferred_present_mode: None,
            },
            None,
            None,
        )?;
        let sprite_atlas = renderer.sprite_atlas().clone();

        let bounds = Bounds {
            origin: Point::default(),
            size: size(
                px(physical_width as f32 / scale_factor),
                px(physical_height as f32 / scale_factor),
            ),
        };
        let _ = params; // Android windows are always fullscreen.

        Ok(Self {
            _handle: handle,
            native_window: Mutex::new(surface),
            gpu_context,
            renderer: Mutex::new(Some(renderer)),
            state: Mutex::new(WindowState {
                bounds,
                scale_factor,
                title: String::new(),
                input_handler: None,
                is_active: true,
                appearance: WindowAppearance::Light,
                insets: WindowInsets::default(),
                mouse_position: Point::default(),
                modifiers: Modifiers::default(),
            }),
            callbacks: Mutex::new(WindowCallbacks::default()),
            sprite_atlas,
            configured_width: AtomicI32::new(physical_width),
            configured_height: AtomicI32::new(physical_height),
        })
    }

    /// Applies a surface resize (physical pixels + new density). Called from
    /// the platform loop on `onNativeWindowResized` / config change.
    pub(crate) fn surface_resized(&self, width: i32, height: i32, density: f32) {
        AndroidDisplay::update(width, height, density);
        {
            let mut state = self.state.lock();
            state.scale_factor = density;
            state.bounds = Bounds {
                origin: Point::default(),
                size: size(px(width as f32 / density), px(height as f32 / density)),
            };
        }
        self.configured_width.store(width, Ordering::Release);
        self.configured_height.store(height, Ordering::Release);
        let mut callbacks = self.callbacks.lock();
        if let Some(resize) = callbacks.resize.as_mut() {
            resize(size(px(width as f32 / density), px(height as f32 / density)), density);
        }
    }

    /// Replaces the renderer after the surface was destroyed and recreated
    /// (rotation, backgrounding). Drops the old swapchain first.
    pub(crate) fn recreate_renderer(
        &self,
        native_window: NonNull<std::ffi::c_void>,
    ) -> anyhow::Result<()> {
        *self.native_window.lock() = NativeSurface { native_window };
        let (physical_width, physical_height) = AndroidDisplay::physical_size();
        let surface = *self.native_window.lock();
        let renderer = WgpuRenderer::new(
            Rc::clone(&self.gpu_context),
            &surface,
            WgpuSurfaceConfig {
                size: Size {
                    width: DevicePixels(physical_width),
                    height: DevicePixels(physical_height),
                },
                transparent: false,
                preferred_present_mode: None,
            },
            None,
            None,
        )?;
        *self.renderer.lock() = Some(renderer);
        Ok(())
    }

    /// Updates the safe-area / IME insets reported to gpui. Called from the
    /// platform loop with `WindowInsets` parsed from the activity's config.
    pub(crate) fn set_insets(&self, insets: WindowInsets) {
        self.state.lock().insets = insets;
    }

    pub(crate) fn set_appearance(&self, appearance: WindowAppearance) {
        self.state.lock().appearance = appearance;
        if let Some(callback) = self.callbacks.lock().appearance_changed.as_mut() {
            callback();
        }
    }

    pub(crate) fn set_active(&self, active: bool) {
        self.state.lock().is_active = active;
        if let Some(callback) = self.callbacks.lock().active_status_change.as_mut() {
            callback(active);
        }
    }

    /// Delivers one `PlatformInput` into gpui's event pipeline.
    pub(crate) fn dispatch_input(&self, input: PlatformInput) -> DispatchEventResult {
        let mut callbacks = self.callbacks.lock();
        match callbacks.input.as_mut() {
            Some(callback) => callback(input),
            None => DispatchEventResult::default(),
        }
    }

    /// Runs the frame callback registered by gpui's scene scheduler.
    pub(crate) fn request_frame(&self, options: RequestFrameOptions) {
        let mut callbacks = self.callbacks.lock();
        if let Some(callback) = callbacks.request_frame.as_mut() {
            callback(options);
        }
    }
}

impl HasWindowHandle for AndroidWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, raw_window_handle::HandleError> {
        let native_window = self.native_window.lock().native_window;
        let handle = AndroidNdkWindowHandle::new(native_window);
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::AndroidNdk(handle)) })
    }
}

impl HasDisplayHandle for AndroidWindow {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, raw_window_handle::HandleError> {
        let handle = AndroidDisplayHandle::new();
        Ok(unsafe { DisplayHandle::borrow_raw(RawDisplayHandle::Android(handle)) })
    }
}

impl PlatformWindow for AndroidWindow {
    fn bounds(&self) -> Bounds<Pixels> {
        self.state.lock().bounds
    }

    fn is_maximized(&self) -> bool {
        // Android windows are always fullscreen.
        true
    }

    fn window_bounds(&self) -> WindowBounds {
        WindowBounds::Fullscreen(self.state.lock().bounds)
    }

    fn content_size(&self) -> Size<Pixels> {
        self.state.lock().bounds.size
    }

    fn resize(&mut self, _size: Size<Pixels>) {
        // The OS owns the window size; resizes arrive via `surface_resized`.
    }

    fn scale_factor(&self) -> f32 {
        self.state.lock().scale_factor
    }

    fn appearance(&self) -> WindowAppearance {
        self.state.lock().appearance
    }

    fn display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        Some(Rc::new(AndroidDisplay::primary()))
    }

    fn mouse_position(&self) -> Point<Pixels> {
        self.state.lock().mouse_position
    }

    fn modifiers(&self) -> Modifiers {
        self.state.lock().modifiers
    }

    fn capslock(&self) -> Capslock {
        Capslock::default()
    }

    fn set_input_handler(&mut self, input_handler: PlatformInputHandler) {
        self.state.lock().input_handler = Some(input_handler);
    }

    fn take_input_handler(&mut self) -> Option<PlatformInputHandler> {
        self.state.lock().input_handler.take()
    }

    fn prompt(
        &self,
        _level: PromptLevel,
        _msg: &str,
        _detail: Option<&str>,
        _answers: &[PromptButton],
    ) -> Option<futures::channel::oneshot::Receiver<usize>> {
        // No synchronous modal dialogs on Android; app-level prompts are
        // gpui windows the app itself surfaces.
        None
    }

    fn activate(&self, _token: Option<&str>) -> bool {
        // The Activity is always the focused window on Android.
        true
    }

    fn is_active(&self) -> bool {
        self.state.lock().is_active
    }

    fn is_hovered(&self) -> bool {
        // Touch surface: hover starts and ends with the touch itself.
        false
    }

    fn background_appearance(&self) -> WindowBackgroundAppearance {
        WindowBackgroundAppearance::Opaque
    }

    fn set_title(&mut self, title: &str) {
        self.state.lock().title = title.to_string();
        // The Activity label owns the visible title on Android.
    }

    fn set_background_appearance(&self, _background_appearance: WindowBackgroundAppearance) {
        // Always opaque; blur/vibrancy do not exist on Android.
    }

    fn minimize(&self) {
        // Apps cannot minimize themselves; the home gesture does.
    }

    fn zoom(&self) {
        // Fullscreen already.
    }

    fn toggle_fullscreen(&self) {
        // Fullscreen already.
    }

    fn is_fullscreen(&self) -> bool {
        true
    }

    fn on_request_frame(&self, callback: Box<dyn FnMut(RequestFrameOptions)>) {
        self.callbacks.lock().request_frame = Some(callback);
    }

    fn on_input(&self, callback: Box<dyn FnMut(PlatformInput) -> DispatchEventResult>) {
        self.callbacks.lock().input = Some(callback);
    }

    fn on_active_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.callbacks.lock().active_status_change = Some(callback);
    }

    fn on_hover_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.callbacks.lock().hover_status_change = Some(callback);
    }

    fn on_resize(&self, callback: Box<dyn FnMut(Size<Pixels>, f32)>) {
        self.callbacks.lock().resize = Some(callback);
    }

    fn on_moved(&self, _callback: Box<dyn FnMut()>) {
        // The window never moves.
    }

    fn on_should_close(&self, callback: Box<dyn FnMut() -> bool>) {
        self.callbacks.lock().should_close = Some(callback);
    }

    fn on_close(&self, callback: Box<dyn FnOnce()>) {
        self.callbacks.lock().close = Some(callback);
    }

    fn on_appearance_changed(&self, callback: Box<dyn FnMut()>) {
        self.callbacks.lock().appearance_changed = Some(callback);
    }

    fn on_hit_test_window_control(&self, _callback: Box<dyn FnMut() -> Option<WindowControlArea>>) {
        // No window controls on Android.
    }

    fn update_ime_position(&self, _bounds: Bounds<Pixels>) {
        // The IME mirror lands with the mobile pin (InputConnection).
    }

    fn draw(&self, scene: &Scene) {
        let width = self.configured_width.load(Ordering::Acquire);
        let height = self.configured_height.load(Ordering::Acquire);
        let mut renderer_slot = self.renderer.lock();
        let Some(renderer) = renderer_slot.as_mut() else {
            return;
        };
        renderer.update_drawable_size(Size {
            width: DevicePixels(width),
            height: DevicePixels(height),
        });
        renderer.draw(scene);
    }

    fn schedule_frame(&self) {
        self.request_frame(RequestFrameOptions::default());
    }

    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        self.sprite_atlas.clone()
    }

    fn is_subpixel_rendering_supported(&self) -> bool {
        // Vulkan/GLES swapchain targets don't do subpixel LCD rendering.
        false
    }

    fn gpu_specs(&self) -> Option<GpuSpecs> {
        self.renderer.lock().as_ref().map(|r| r.gpu_specs())
    }

    fn insets(&self) -> WindowInsets {
        self.state.lock().insets.clone()
    }
}
