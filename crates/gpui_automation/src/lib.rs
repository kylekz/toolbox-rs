//! JSONL automation for gpui apps (docs/automation.md).
//!
//! gpui: **yes**. Apps depend on this crate unconditionally and turn the driver on with its
//! `enabled` feature from their own `automation` feature. Without `enabled`, the same names are
//! no-op stand-ins: [`AutomationTargetExt::automation_target`] is a plain
//! `into_any_element()`, [`open_window`] is `App::open_window`, [`is_background`] is `false`,
//! so views need no `cfg` and the shipped build carries none of the driver.
//!
//! An app started with `--automation` calls [`start`] with its main window
//! and an entity implementing [`AutomationHost`]. From then on every line on
//! stdin is a command, every command gets exactly one JSON reply line on
//! stdout, and stdout carries nothing else (logs go through `log`, which the
//! app should route to stderr). Closing stdin quits the app, so a crashed
//! driver never leaves a window behind. With [`Options::pipe`] the same
//! protocol runs over a Windows named pipe instead, which drivers can attach
//! to (and leave) while the app keeps running.
//!
//! What the library provides generically:
//! - input: `key`, `keys`, `type`, `click`, `mouse_move`, `mouse_down`,
//!   `mouse_up`, `drag` (frame-paced), `scroll`, `drop_files` (real
//!   `FileDropEvent`s, so `on_drop::<ExternalPaths>` runs for real);
//! - windows: `windows`, `window`, `resize`, `fullscreen`, `screenshot`;
//! - synchronisation on the real frame loop: `wait_frames`, `settle`,
//!   `wait_ready`, `wait_for`/`assert` (JSON pointers over `state`), `wait`;
//! - named targets via [`AutomationTargetExt::automation_target`], used by
//!   `click{target}`, `bounds`, `query` and every other pointer command.
//!
//! What the app provides through [`AutomationHost`]: the `state` document
//! (the assertion surface), a `settled` predicate and any custom commands.
//! Root views should also render [`pointer_guard`] so the real mouse can't
//! interfere with scripted input. Apps that open every window through
//! [`open_window`] can also run in the background ([`set_background`]):
//! their windows never take the focus and stay behind every other window.
//! docs/automation.md documents the protocol and the scenario format.

#[cfg(feature = "enabled")]
pub mod background;
#[cfg(all(feature = "enabled", windows))]
mod background_windows;
#[cfg(feature = "enabled")]
mod capture;
#[cfg(all(feature = "enabled", target_os = "macos"))]
mod capture_macos;
#[cfg(all(feature = "enabled", windows))]
mod capture_windows;
#[cfg(all(feature = "enabled", target_os = "linux"))]
mod capture_x11;
#[cfg(not(feature = "enabled"))]
mod disabled;
#[cfg(feature = "enabled")]
mod driver;
#[cfg(feature = "enabled")]
mod frames;
#[cfg(feature = "enabled")]
mod guard;
#[cfg(all(feature = "enabled", windows))]
mod hit_test_windows;
#[cfg(feature = "enabled")]
pub mod keys;
#[cfg(feature = "enabled")]
mod pipe;
#[cfg(all(feature = "enabled", windows))]
mod resize_windows;
#[cfg(feature = "enabled")]
mod targets;
#[cfg(feature = "enabled")]
mod transport;
#[cfg(feature = "enabled")]
mod windows;

#[cfg(feature = "enabled")]
use gpui_automation_protocol::commands;
pub use gpui_automation_protocol::{matcher, parse_window_size, protocol};

#[cfg(not(feature = "enabled"))]
pub use disabled::{
    AutomationTargetExt, is_active, is_background, is_capturing, is_synthetic_input, open_window,
    pointer_guard, record_target, register_window, set_background, unregister_window,
};

#[cfg(feature = "enabled")]
mod enabled;
#[cfg(feature = "enabled")]
pub use enabled::*;
