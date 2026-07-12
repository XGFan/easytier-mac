//! EasyTier macOS native GUI bridge (M2).
//!
//! Exposes the GUI's mechanism layer over a stable C ABI
//! (`include/easytier_bridge.h`): the supervisor control driver, the core RPC
//! surface, config validation, and install/conflict helpers, all behind an opaque
//! `EtbHandle` with a built-in tokio runtime. Structured data crosses the boundary
//! as JSON strings; supervisor lifecycle events are pushed through a C callback.
//! Policy (auto-restart, launch restore, settings persistence) lives in the Swift
//! app, per DESIGN §9.
//!
//! The `conflict`/`install`/`proto`/`rpc`/`supervisor_client` modules are migrated
//! from `gui/src-tauri/src` (which stays the source of truth until native parity);
//! `ffi` is the new C entry-point layer.

#[cfg(not(target_os = "macos"))]
compile_error!("easytier-mac-bridge targets macOS only");

mod conflict;
mod install;
mod proto;
mod rpc;
mod supervisor_client;

pub mod ffi;

pub use ffi::{EtbEventCb, EtbHandle};
