//! The driver's public API, with the `enabled` feature: the host trait and starting automation.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context as _, Result, bail};
use gpui::{AnyWindowHandle, App, Context, Entity, Task, Window};
use serde_json::Value;

pub use crate::background::{is_background, open_window, set_background, window_options};
pub use crate::guard::{is_synthetic_input, pointer_guard};
pub use crate::targets::{AutomationTargetExt, is_capturing, record_target};

/// The app-specific half of automation.
pub trait AutomationHost: Sized + 'static {
    /// The assertion surface: a JSON object describing what the app shows in
    /// `window`. The library adds a `window` key (size, scale, fullscreen,
    /// maximized, activation, bounds, display) and a `windows` key (the
    /// addressable windows); host keys with those names are overwritten. A
    /// non-object is wrapped as `{"app": value}`.
    fn state(&self, window: &Window, cx: &App) -> Value;

    /// `true` when nothing is pending: no in-flight tasks, streams or
    /// animations that would change `state` without further input.
    fn settled(&self, cx: &App) -> bool;

    /// The commands [`custom`](Self::custom) and [`custom_task`](Self::custom_task) know, sent
    /// in the `ready` event so a runner can reject a scenario's unknown command before running
    /// it. Empty (the default) means "not listed": nothing is checked up front.
    fn commands(&self) -> &'static [&'static str] {
        &[]
    }

    /// Handle an app-specific command. Return `None` for commands the host
    /// doesn't know (the driver then reports "unknown command"). `args` is
    /// the request object without `id`, `cmd` and `window`. The reply is sent
    /// after the window has rendered the command's effects.
    fn custom(
        &mut self,
        cmd: &str,
        args: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Result<Value>> {
        let _ = (cmd, args, window, cx);
        None
    }

    /// Like [`custom`](Self::custom), for commands that finish later, such
    /// as database work on a background thread: the reply waits for the task
    /// (within the 120 s cap on every wait, and giving up early if the driver
    /// hangs up), then for the window to render its effects. The driver asks
    /// only when `custom` returned `None` for the command.
    fn custom_task(
        &mut self,
        cmd: &str,
        args: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<Value>>> {
        let _ = (cmd, args, window, cx);
        None
    }
}

/// How [`start`] wires the channel.
#[derive(Debug, Clone)]
pub struct Options {
    /// Name the first window is registered under (default `"main"`).
    pub window_name: String,
    /// Quit the app when stdin reaches EOF (default `true`). Stdio only: a
    /// pipe client disconnecting never quits the app.
    pub quit_on_eof: bool,
    /// Serve the protocol on the Windows named pipe `\\.\pipe\<name>`
    /// (a bare name, or the full path) instead of stdin/stdout, for an
    /// `--automation-pipe <name>` flag. One client at a time; each is greeted
    /// with the `ready` event, and the app keeps running when it leaves.
    /// Stdin is not read in this mode. Fails on other platforms, and when
    /// another process already serves that name.
    pub pipe: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            window_name: "main".into(),
            quit_on_eof: true,
            pipe: None,
        }
    }
}

static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Whether this process is driven by automation. Hosts can use it to skip
/// checks that synthetic input can't satisfy, such as the platform's
/// `Window::is_window_hovered`, which follows the real OS cursor.
pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Start the automation channel. `window` becomes the `"main"` window (see
/// [`Options::window_name`]) and the initially selected one. Replies start
/// after the window has drawn its first frame, announced by a
/// `{"event":"ready"}` line.
pub fn start<H: AutomationHost>(
    window: AnyWindowHandle,
    host: Entity<H>,
    cx: &mut App,
    options: Options,
) -> Result<()> {
    if is_active() {
        bail!("automation is already running in this process");
    }
    let registry = crate::windows::SharedRegistry::default();
    {
        let mut inner = registry.0.borrow_mut();
        inner.register(options.window_name.clone(), window);
        inner.select(window);
    }
    cx.set_global(registry.clone());

    let clients = Arc::new(crate::transport::Clients::default());
    let (rx, output) = match &options.pipe {
        Some(name) => crate::pipe::serve(name, clients.clone()).context("starting automation")?,
        None => {
            let rx = crate::transport::spawn_stdin_pump(clients.clone(), options.quit_on_eof)
                .context("starting automation")?;
            log::info!("automation: listening on stdin");
            (rx, crate::transport::Output::Stdout)
        }
    };
    ACTIVE.store(true, Ordering::Relaxed);
    let driver = crate::driver::Driver::new(host, registry, clients, output);
    cx.spawn(async move |cx| driver.run(rx, cx).await).detach();
    Ok(())
}

/// Make `window` addressable as `name` (`"window": "reader"` on any command,
/// or `window{name}`). Re-registering a name or a window replaces the old
/// entry. A no-op unless automation is running.
pub fn register_window(name: impl Into<String>, window: AnyWindowHandle, cx: &mut App) {
    if let Some(registry) = crate::windows::shared(cx) {
        registry.0.borrow_mut().register(name.into(), window);
    }
}

/// Forget the name registered with [`register_window`]. Closed windows drop
/// out of the list on their own, so this is only needed to reuse a window
/// under a different name. A no-op unless automation is running.
pub fn unregister_window(name: &str, cx: &mut App) {
    if let Some(registry) = crate::windows::shared(cx) {
        registry.0.borrow_mut().unregister(name);
    }
}

/// The name `window` was registered under, for hosts that describe several
/// windows from one [`AutomationHost::state`].
pub fn window_name(window: &Window, cx: &App) -> Option<String> {
    let registry = crate::windows::shared(cx)?;
    let registry = registry.0.borrow();
    registry.name_of(window.window_handle()).map(str::to_string)
}
