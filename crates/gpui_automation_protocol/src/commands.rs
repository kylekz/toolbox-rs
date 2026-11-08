//! Built-in command arguments. Every argument struct rejects unknown fields
//! so a misspelt option fails the command instead of being ignored.

use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::matcher::Expectation;

/// Default for `wait_ready`, `settle`, `wait_for` and `fullscreen` timeouts.
pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;

/// A command the library implements itself. Anything else is offered to the
/// host's `gpui_automation::AutomationHost::custom`.
#[derive(Debug)]
pub enum Builtin {
    State,
    Windows,
    Actions,
    Action(ActionArgs),
    Key(KeyArgs),
    Keys(KeysArgs),
    Type(TypeArgs),
    Click(ClickArgs),
    MouseMove(PointerArgs),
    MouseDown(ButtonArgs),
    MouseUp(ButtonArgs),
    Drag(DragArgs),
    Scroll(ScrollArgs),
    DropFiles(DropFilesArgs),
    Resize(ResizeArgs),
    WindowControl(PointerArgs),
    DpiChanged(DpiArgs),
    Fullscreen(FullscreenArgs),
    Window(WindowArgs),
    Screenshot(ScreenshotArgs),
    Wait(WaitArgs),
    WaitReady(TimeoutArgs),
    WaitFor(WaitForArgs),
    Assert(Expectation),
    Settle(TimeoutArgs),
    WaitFrames(WaitFramesArgs),
    Bounds(BoundsArgs),
    Query(QueryArgs),
    Quit,
}

impl Builtin {
    /// Every built-in command name, in documentation order.
    pub const NAMES: &[&str] = &[
        "state",
        "windows",
        "actions",
        "action",
        "key",
        "keys",
        "type",
        "click",
        "mouse_move",
        "mouse_down",
        "mouse_up",
        "drag",
        "scroll",
        "drop_files",
        "resize",
        "window_control",
        "dpi_changed",
        "fullscreen",
        "window",
        "screenshot",
        "wait",
        "wait_ready",
        "wait_for",
        "assert",
        "settle",
        "wait_frames",
        "bounds",
        "query",
        "quit",
    ];

    /// Decode a built-in, or hand `args` back when `cmd` is not one so the
    /// host gets a chance at it.
    pub fn parse(cmd: &str, args: Map<String, Value>) -> Parsed {
        let parsed = match cmd {
            "state" => no_args(cmd, args).map(|()| Self::State),
            "windows" => no_args(cmd, args).map(|()| Self::Windows),
            "actions" => no_args(cmd, args).map(|()| Self::Actions),
            "action" => from_args(args).map(Self::Action),
            "key" => from_args(args).map(Self::Key),
            "keys" => from_args(args).map(Self::Keys),
            "type" => from_args(args).map(Self::Type),
            "click" => from_args(args).map(Self::Click),
            "mouse_move" => from_args(args).map(Self::MouseMove),
            "mouse_down" => from_args(args).map(Self::MouseDown),
            "mouse_up" => from_args(args).map(Self::MouseUp),
            "drag" => from_args(args).map(Self::Drag),
            "scroll" => from_args(args).map(Self::Scroll),
            "drop_files" => from_args(args).map(Self::DropFiles),
            "resize" => from_args(args).map(Self::Resize),
            "window_control" => from_args(args).map(Self::WindowControl),
            "dpi_changed" => from_args(args).map(Self::DpiChanged),
            "fullscreen" => from_args(args).map(Self::Fullscreen),
            "window" => from_args(args).map(Self::Window),
            "screenshot" => from_args(args).map(Self::Screenshot),
            "wait" => from_args(args).map(Self::Wait),
            "wait_ready" => from_args(args).map(Self::WaitReady),
            "wait_for" => WaitForArgs::parse(&args).map(Self::WaitFor),
            "assert" => Expectation::parse_map(&args, &[])
                .map_err(|err| anyhow!(err))
                .map(Self::Assert),
            "settle" => from_args(args).map(Self::Settle),
            "wait_frames" => from_args(args).map(Self::WaitFrames),
            "bounds" => from_args(args).map(Self::Bounds),
            "query" => from_args(args).map(Self::Query),
            "quit" => no_args(cmd, args).map(|()| Self::Quit),
            _ => return Parsed::Custom(args),
        };
        Parsed::Builtin(parsed.with_context(|| format!("bad arguments for {cmd}")))
    }
}

/// Outcome of [`Builtin::parse`].
#[derive(Debug)]
pub enum Parsed {
    Builtin(Result<Builtin>),
    /// Not a built-in: the untouched arguments, for the host.
    Custom(Map<String, Value>),
}

fn from_args<T: DeserializeOwned>(args: Map<String, Value>) -> Result<T> {
    Ok(serde_json::from_value(Value::Object(args))?)
}

fn no_args(cmd: &str, args: Map<String, Value>) -> Result<()> {
    match args.keys().next() {
        Some(key) => bail!("{cmd} takes no arguments, got {key:?}"),
        None => Ok(()),
    }
}

/// Where a pointer command acts: window coordinates in logical pixels, or
/// the centre of a registered automation target's visible bounds.
#[derive(Debug, Clone, PartialEq)]
pub enum Position {
    Point(f32, f32),
    Target(String),
    /// A target's centre moved by `dx`, `dy` (e.g. onto a row's top edge).
    TargetOffset(String, f32, f32),
}

/// Combine the flat `x`/`y`/`target` fields most pointer commands take.
/// `Ok(None)` means none were given, which some commands accept (they reuse
/// the last pointer position).
pub fn position(
    x: Option<f32>,
    y: Option<f32>,
    target: Option<String>,
) -> Result<Option<Position>> {
    match (x, y, target) {
        (Some(x), Some(y), None) => Ok(Some(Position::Point(x, y))),
        (None, None, Some(target)) => Ok(Some(Position::Target(target))),
        (None, None, None) => Ok(None),
        _ => bail!("give either both x and y, or a target"),
    }
}

/// A point given as `[x, y]`, `{"x": .., "y": ..}`, `{"target": ".."}` (with
/// an optional `dx`/`dy` from the target's centre) or a bare target name.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum PointArg {
    Pair([f32; 2]),
    Xy {
        x: f32,
        y: f32,
    },
    Target {
        target: String,
        #[serde(default)]
        dx: f32,
        #[serde(default)]
        dy: f32,
    },
    Name(String),
}

impl From<PointArg> for Position {
    fn from(arg: PointArg) -> Self {
        match arg {
            PointArg::Pair([x, y]) | PointArg::Xy { x, y } => Position::Point(x, y),
            PointArg::Target { target, dx, dy } if dx != 0.0 || dy != 0.0 => {
                Position::TargetOffset(target, dx, dy)
            }
            PointArg::Target { target, .. } | PointArg::Name(target) => Position::Target(target),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionArgs {
    pub name: String,
    #[serde(default)]
    pub data: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyArgs {
    pub keystroke: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeysArgs {
    pub keys: KeysSpec,
}

/// `"ctrl-k down"` or `["ctrl-k", "down"]`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum KeysSpec {
    Sequence(String),
    List(Vec<String>),
}

impl KeysSpec {
    pub fn joined(&self) -> String {
        match self {
            Self::Sequence(sequence) => sequence.clone(),
            Self::List(keys) => keys.join(" "),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypeArgs {
    pub text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClickArgs {
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub target: Option<String>,
    /// `left` (default), `right` or `middle`.
    pub button: Option<String>,
    /// Modifier spec held during the click, e.g. `"ctrl-shift"`.
    pub modifiers: Option<String>,
    /// Legacy shorthand from `diff`'s scripts; ORed into `modifiers`.
    #[serde(default)]
    pub shift: bool,
    /// 2 for a double click, 3 for a triple click.
    pub count: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PointerArgs {
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub target: Option<String>,
    pub modifiers: Option<String>,
}

/// `dpi_changed`: the display scale Windows announces, as a DPI (96 is 100 %).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DpiArgs {
    pub dpi: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ButtonArgs {
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub target: Option<String>,
    pub button: Option<String>,
    pub modifiers: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DragArgs {
    /// Omit to continue a drag left pressed by `"release": false`.
    pub from: Option<PointArg>,
    pub to: PointArg,
    /// Intermediate moves between `from` and `to` (default 10).
    pub steps: Option<u32>,
    /// Extra delay after each move, on top of the rendered frame every move
    /// already waits for.
    pub step_ms: Option<u64>,
    /// How long to hover at `to` before releasing (hover-to-expand timers).
    pub hold_ms: Option<u64>,
    /// `false` keeps the button down at `to` so a script can screenshot or
    /// assert mid-drag and continue with another `drag` or `mouse_up`.
    pub release: Option<bool>,
    pub button: Option<String>,
    pub modifiers: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScrollArgs {
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub target: Option<String>,
    #[serde(default)]
    pub dx: f32,
    #[serde(default)]
    pub dy: f32,
    /// `px` (default) for a precise delta, `lines` for wheel notches.
    pub unit: Option<String>,
    pub modifiers: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DropFilesArgs {
    /// Files to drag in. Omit to continue a drag left pending by
    /// `"submit": false`.
    pub paths: Option<Vec<PathBuf>>,
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub target: Option<String>,
    /// Hover time before dropping (drop-zone highlight, hover timers).
    pub hold_ms: Option<u64>,
    /// `false` leaves the files hovering over the window (default `true`).
    pub submit: Option<bool>,
    /// Drag the files back out instead of dropping them.
    #[serde(default)]
    pub cancel: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResizeArgs {
    pub w: f32,
    pub h: f32,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FullscreenArgs {
    pub on: bool,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowArgs {
    // No "activate" option on purpose: gpui's Windows `activate_window`
    // synthesizes a real Alt keypress through `SendInput`, which reaches the
    // user's desktop and can drop the window into its system-menu loop.
    // Synthetic input and screenshots work without OS activation.
    pub index: Option<usize>,
    pub name: Option<String>,
}

/// Where a screenshot's pixels come from (`screenshot`'s `source`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureSource {
    /// The window's own composed frame: `PrintWindow(PW_RENDERFULLCONTENT)`
    /// on Windows, a Core Graphics image of the window on macOS. It works while other
    /// windows cover the window, so background runs use it. On Windows it
    /// includes Mica but not Acrylic's blur-behind, so translucent surfaces
    /// over Acrylic come out composited over black (`gpui_automation`'s `capture_windows`).
    #[default]
    Window,
    /// The window's client rect copied from the desktop as it is shown,
    /// blur-behind included. Only meaningful for a window in front (a
    /// foreground run). On Windows the capture fails, rather than copying
    /// another window's pixels, when the window isn't the foreground window
    /// or something covers its client area. On macOS it is the window and
    /// everything under it, composed as the screen shows them (what shows
    /// through translucency included) but without any window above it; a
    /// background window is in front there anyway (`gpui_automation::background`), so
    /// background runs can use it.
    Screen,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenshotArgs {
    pub path: PathBuf,
    /// Where the pixels come from; the window's own frame unless asked.
    #[serde(default)]
    pub source: CaptureSource,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitArgs {
    pub ms: u64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeoutArgs {
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitFramesArgs {
    #[serde(default = "one")]
    pub n: u32,
}

fn one() -> u32 {
    1
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundsArgs {
    pub target: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryArgs {
    #[serde(default)]
    pub prefix: String,
}

#[derive(Debug)]
pub struct WaitForArgs {
    pub expectation: Expectation,
    pub timeout_ms: u64,
    pub interval_ms: u64,
}

impl WaitForArgs {
    fn parse(args: &Map<String, Value>) -> Result<Self> {
        let expectation =
            Expectation::parse_map(args, &["timeout_ms", "interval_ms"]).map_err(|e| anyhow!(e))?;
        let millis = |key: &str, default: u64| -> Result<u64> {
            match args.get(key) {
                None => Ok(default),
                Some(value) => value
                    .as_u64()
                    .ok_or_else(|| anyhow!("{key} must be a non-negative integer")),
            }
        };
        Ok(Self {
            expectation,
            timeout_ms: millis("timeout_ms", DEFAULT_TIMEOUT_MS)?,
            interval_ms: millis("interval_ms", 25)?.max(1),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn parse(value: Value) -> Result<Builtin> {
        let Value::Object(mut args) = value else {
            panic!("test input must be an object")
        };
        let Some(Value::String(cmd)) = args.remove("cmd") else {
            panic!("test input needs a cmd")
        };
        match Builtin::parse(&cmd, args) {
            Parsed::Builtin(parsed) => parsed,
            Parsed::Custom(_) => panic!("{cmd} is not a builtin"),
        }
    }

    #[test]
    fn screenshots_come_from_the_window_unless_the_screen_is_asked_for() {
        let source = |value: Value| match parse(value) {
            Ok(Builtin::Screenshot(args)) => Ok(args.source),
            Ok(other) => panic!("parsed as {other:?}"),
            Err(err) => Err(format!("{err:#}")),
        };
        assert_eq!(
            source(json!({"cmd": "screenshot", "path": "a.png"})),
            Ok(CaptureSource::Window)
        );
        assert_eq!(
            source(json!({"cmd": "screenshot", "path": "a.png", "source": "window"})),
            Ok(CaptureSource::Window)
        );
        assert_eq!(
            source(json!({"cmd": "screenshot", "path": "a.png", "source": "screen"})),
            Ok(CaptureSource::Screen)
        );
        let err =
            source(json!({"cmd": "screenshot", "path": "a.png", "source": "desktop"})).unwrap_err();
        assert!(err.contains("unknown variant"), "{err}");
    }

    #[test]
    fn every_documented_name_is_a_builtin() {
        for name in Builtin::NAMES {
            assert!(
                matches!(Builtin::parse(name, Map::new()), Parsed::Builtin(_)),
                "{name} is listed but not parsed"
            );
        }
        let mut args = Map::new();
        args.insert("value".into(), json!(5));
        match Builtin::parse("set_counter", args) {
            Parsed::Custom(args) => assert_eq!(args.get("value"), Some(&json!(5))),
            Parsed::Builtin(_) => panic!("set_counter is not a builtin"),
        }
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let err = parse(json!({"cmd": "key", "keystrokes": "a"})).unwrap_err();
        assert!(format!("{err:#}").contains("keystrokes"), "{err:#}");
        let err = parse(json!({"cmd": "state", "verbose": true})).unwrap_err();
        assert!(format!("{err:#}").contains("takes no arguments"), "{err:#}");
        let err = parse(json!({"cmd": "click", "x": 1, "y": 2, "buton": "left"})).unwrap_err();
        assert!(format!("{err:#}").contains("buton"), "{err:#}");
    }

    #[test]
    fn click_accepts_coordinates_or_target() {
        let Builtin::Click(args) = parse(json!({"cmd": "click", "x": 10, "y": 20.5})).unwrap()
        else {
            panic!("expected click")
        };
        assert_eq!(
            position(args.x, args.y, args.target).unwrap(),
            Some(Position::Point(10.0, 20.5))
        );
        let Builtin::Click(args) = parse(json!({"cmd": "click", "target": "a/b"})).unwrap() else {
            panic!("expected click")
        };
        assert_eq!(
            position(args.x, args.y, args.target).unwrap(),
            Some(Position::Target("a/b".into()))
        );
    }

    #[test]
    fn position_requires_both_coordinates_or_a_target() {
        assert!(position(Some(1.0), None, None).is_err());
        assert!(position(Some(1.0), Some(2.0), Some("t".into())).is_err());
        assert_eq!(position(None, None, None).unwrap(), None);
    }

    #[test]
    fn drag_points_take_every_documented_shape() {
        let Builtin::Drag(args) = parse(json!({
            "cmd": "drag", "from": [1, 2], "to": "list/row/3", "steps": 4, "hold_ms": 600
        }))
        .unwrap() else {
            panic!("expected drag")
        };
        assert_eq!(
            Position::from(args.from.unwrap()),
            Position::Point(1.0, 2.0)
        );
        assert_eq!(
            Position::from(args.to),
            Position::Target("list/row/3".into())
        );
        assert_eq!(args.steps, Some(4));
        assert_eq!(args.hold_ms, Some(600));

        let Builtin::Drag(args) = parse(json!({
            "cmd": "drag", "from": {"target": "item"}, "to": {"x": 5, "y": 6}, "release": false
        }))
        .unwrap() else {
            panic!("expected drag")
        };
        assert_eq!(
            Position::from(args.from.unwrap()),
            Position::Target("item".into())
        );
        assert_eq!(Position::from(args.to), Position::Point(5.0, 6.0));
        assert_eq!(args.release, Some(false));

        let Builtin::Drag(args) = parse(json!({
            "cmd": "drag", "to": {"target": "row", "dy": -12.5}
        }))
        .unwrap() else {
            panic!("expected drag")
        };
        assert_eq!(
            Position::from(args.to),
            Position::TargetOffset("row".into(), 0.0, -12.5)
        );
    }

    #[test]
    fn keys_accept_a_string_or_a_list() {
        let Builtin::Keys(args) = parse(json!({"cmd": "keys", "keys": "ctrl-k down"})).unwrap()
        else {
            panic!("expected keys")
        };
        assert_eq!(args.keys.joined(), "ctrl-k down");
        let Builtin::Keys(args) = parse(json!({"cmd": "keys", "keys": ["a", "b"]})).unwrap() else {
            panic!("expected keys")
        };
        assert_eq!(args.keys.joined(), "a b");
    }

    #[test]
    fn wait_for_splits_expectation_and_timing() {
        let Builtin::WaitFor(args) = parse(json!({
            "cmd": "wait_for", "pointer": "/story/status", "equals": "complete", "timeout_ms": 500
        }))
        .unwrap() else {
            panic!("expected wait_for")
        };
        assert_eq!(args.expectation.pointer, "/story/status");
        assert_eq!(args.timeout_ms, 500);
        assert_eq!(args.interval_ms, 25);
        assert!(parse(json!({"cmd": "wait_for", "pointer": "/a"})).is_err());
        assert!(
            parse(json!({"cmd": "wait_for", "pointer": "/a", "exists": true, "timeout": 1}))
                .is_err()
        );
    }

    #[test]
    fn defaults_apply() {
        let Builtin::WaitFrames(args) = parse(json!({"cmd": "wait_frames"})).unwrap() else {
            panic!("expected wait_frames")
        };
        assert_eq!(args.n, 1);
        let Builtin::Query(args) = parse(json!({"cmd": "query"})).unwrap() else {
            panic!("expected query")
        };
        assert_eq!(args.prefix, "");
        let Builtin::DropFiles(args) =
            parse(json!({"cmd": "drop_files", "paths": ["a.png"], "target": "zone"})).unwrap()
        else {
            panic!("expected drop_files")
        };
        assert_eq!(args.paths.unwrap(), vec![PathBuf::from("a.png")]);
        assert!(!args.cancel);
    }
}
