//! Android display handling backed by `ANativeWindow` metrics.
//!
//! An Android app sees exactly one usable display: the panel its Activity's
//! window is on. `AndroidDisplay` carries the window's current geometry;
//! [`AndroidDisplay::update`] refreshes it on resize/rotation, called from
//! the platform's event loop.

use anyhow::Result;
use gpui::{Bounds, DisplayId, Pixels, PlatformDisplay, px, size};
use parking_lot::RwLock;
use std::sync::LazyLock;
use uuid::Uuid;

/// The process-wide display state, captured from the activity's native
/// window. Initialized to a sane default before the first frame arrives.
static DISPLAY: LazyLock<RwLock<DisplayState>> =
    LazyLock::new(|| RwLock::new(DisplayState::DEFAULT));

#[derive(Debug, Clone, Copy)]
struct DisplayState {
    /// `ANativeWindow` buffer size, physical pixels.
    width_px: i32,
    height_px: i32,
    /// Android density (dpi / 160) — the scale factor.
    density: f32,
}

impl DisplayState {
    const DEFAULT: Self = Self {
        width_px: 1080,
        height_px: 2280,
        density: 2.625,
    };

    fn bounds_in_points(&self) -> Bounds<Pixels> {
        Bounds {
            origin: Default::default(),
            size: size(
                px(self.width_px as f32 / self.density),
                px(self.height_px as f32 / self.density),
            ),
        }
    }
}

fn display_state() -> &'static RwLock<DisplayState> {
    &DISPLAY
}

/// The Activity's display, described in logical pixels.
#[derive(Debug)]
pub struct AndroidDisplay {
    id: DisplayId,
}

impl AndroidDisplay {
    /// The single display every Android app sees.
    pub fn primary() -> Self {
        Self {
            id: DisplayId::new(0),
        }
    }

    /// Refreshes the cached geometry after a window resize or rotation.
    pub fn update(width: i32, height: i32, density: f32) {
        *display_state().write() = DisplayState {
            width_px: width,
            height_px: height,
            density,
        };
    }

    /// Current `ANativeWindow` buffer size in physical pixels.
    pub fn physical_size() -> (i32, i32) {
        let state = *display_state().read();
        (state.width_px, state.height_px)
    }

    /// The Android scale factor (`AConfiguration_density` / 160).
    pub fn scale_factor() -> f32 {
        display_state().read().density
    }
}

impl PlatformDisplay for AndroidDisplay {
    fn id(&self) -> DisplayId {
        self.id
    }

    fn uuid(&self) -> Result<Uuid> {
        // Android gives apps no persistent per-panel UUID; derive a stable
        // one from the panel geometry, mirroring the iOS backend's
        // deterministic-UUID approach.
        let state = *display_state().read();
        let bytes = format!(
            "android-display-{}-{}-{}",
            state.width_px, state.height_px, (state.density * 100.0) as u32
        );
        Ok(Uuid::new_v5(&Uuid::NAMESPACE_OID, bytes.as_bytes()))
    }

    fn bounds(&self) -> Bounds<Pixels> {
        display_state().read().bounds_in_points()
    }
}
