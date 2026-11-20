#![cfg(any(target_os = "macos", target_os = "ios"))]
//! Shared Apple platform support for GPUI.
//!
//! This crate contains the Metal renderer and GPU resource management shared
//! by GPUI's Apple platform backends.

mod metal_atlas;
pub mod metal_renderer;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod dispatcher;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod keychain;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod process_info;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use dispatcher::AppleDispatcher;
