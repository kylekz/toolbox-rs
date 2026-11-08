//! The stand-ins without the `enabled` feature: the same names as the driver's, doing nothing
//! (or, for [`open_window`], what gpui does), so views call them unconditionally.

use gpui::{
    AnyElement, AnyWindowHandle, App, Empty, Entity, IntoElement, Render, SharedString, Window,
    WindowHandle, WindowOptions,
};

/// Stand-in for `gpui_automation::pointer_guard`: nothing to guard without automation.
pub fn pointer_guard() -> AnyElement {
    Empty.into_any_element()
}

/// Without automation nothing runs in the background.
pub fn is_background() -> bool {
    false
}

/// Stand-in for `gpui_automation::set_background`: windows open as usual.
pub fn set_background(background: bool) {
    let _ = background;
}

/// Without automation no input is synthetic.
pub fn is_synthetic_input() -> bool {
    false
}

/// Without automation nothing drives the app.
pub fn is_active() -> bool {
    false
}

/// Without automation nothing captures targets.
pub fn is_capturing() -> bool {
    false
}

/// Stand-in for `gpui_automation::record_target`: no driver, no names.
pub fn record_target(
    window: &Window,
    name: impl FnOnce() -> SharedString,
    bounds: gpui::Bounds<gpui::Pixels>,
) {
    let _ = (window, name, bounds);
}

/// Stand-in for `gpui_automation::register_window`: no driver, no names.
pub fn register_window(name: impl Into<String>, window: AnyWindowHandle, cx: &mut App) {
    let _ = (name.into(), window, cx);
}

/// Stand-in for `gpui_automation::unregister_window`.
pub fn unregister_window(name: &str, cx: &mut App) {
    let _ = (name, cx);
}

/// Stand-in for `gpui_automation::open_window`: a plain `App::open_window`.
pub fn open_window<V: 'static + Render>(
    cx: &mut App,
    options: WindowOptions,
    build_root_view: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> anyhow::Result<WindowHandle<V>> {
    cx.open_window(options, build_root_view)
}

/// Stand-in for `gpui_automation::AutomationTargetExt` when automation is compiled out.
pub trait AutomationTargetExt: IntoElement + Sized {
    fn automation_target(self, name: impl Into<SharedString>) -> AnyElement {
        let _ = name;
        self.into_any_element()
    }

    fn automation_target_with(self, name: impl FnOnce() -> SharedString) -> AnyElement {
        let _ = name;
        self.into_any_element()
    }
}

impl<T: IntoElement> AutomationTargetExt for T {}
