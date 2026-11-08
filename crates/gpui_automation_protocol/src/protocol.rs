//! Wire format: one JSON object per line in, exactly one JSON line out per
//! request.
//!
//! Request: `{"id": <any>, "cmd": "<name>", "window": <name|index>, ...args}`.
//! `id` is echoed back verbatim (`null` when absent) and `window` optionally
//! addresses a window other than the selected one. Everything else is the
//! command's arguments.
//!
//! Two keys apply to every command: `settle` ([`Settle`]: what the reply waits for after input)
//! and `selector` (a [`selector`](parse_selector) naming the element a pointer command acts on,
//! instead of `target`).
//!
//! Reply: `{"id": .., "ok": true, "result": ..}` or
//! `{"id": .., "ok": false, "error": ".."}`. Besides replies the app emits
//! unsolicited events: lines with `"event"` and never `"id"`, which drivers record rather than
//! take for replies. An event caused by a request names it as `"for": <id>`. The first is
//! `{"event": "ready", "protocol": 1, "commands": [...], "capabilities": [...], ..}`
//! ([`VERSION`], [`capability`]).

use std::fmt;

use serde_json::{Map, Value, json};

/// The protocol's version, in the `ready` event. It goes up only for a change that breaks
/// drivers; new commands and keys don't change it.
pub const VERSION: u64 = 1;

/// What the `ready` event's `capabilities` may list: what this app process can do, so a
/// driver can tell an unsupported command from a broken one.
pub mod capability {
    /// `screenshot` of the window's own frame.
    pub const SCREENSHOT: &str = "screenshot";
    /// `screenshot` with `"source": "screen"`, the desktop as shown.
    pub const SCREEN_CAPTURE: &str = "screen_capture";
    /// Windows open unfocused and out of the way (`--automation-background`).
    pub const BACKGROUND: &str = "background";
}

/// What a command's reply waits for after the input it sends (`"settle"` on any request).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Settle {
    /// One drawn frame of the window, so the reply sees the input's effects (the default).
    #[default]
    Frame,
    /// Nothing: reply once the input is dispatched.
    None,
}

impl Settle {
    pub fn from_value(value: &Value) -> Result<Self, String> {
        match value.as_str() {
            Some("frame") => Ok(Self::Frame),
            Some("none") => Ok(Self::None),
            Some("quiet") => Err("settle \"quiet\" is reserved and not implemented yet".into()),
            _ => Err(format!(
                "settle must be \"frame\" or \"none\", got {}",
                crate::matcher::compact(value)
            )),
        }
    }
}

/// The prefixes a selector may start with. Only `target:` resolves today; the others are
/// reserved for gpui-base's element registry (docs/PLAN.md §6), so a selector written now never
/// changes meaning when they arrive.
const SELECTOR_PREFIXES: &[&str] = &["target", "id", "label", "text", "role", "path"];

/// A selector (`"target:sidebar/new-folder"`) as the `target` name it names. Every other
/// reserved prefix, scoping with `>>` and `nth=` fail as not implemented yet; an unknown prefix
/// fails as unknown.
pub fn parse_selector(selector: &str) -> Result<String, String> {
    if selector.contains(">>") {
        return Err(format!(
            "selector {selector:?}: scoping with >> is not implemented yet"
        ));
    }
    let Some((prefix, rest)) = selector.split_once(':') else {
        return Err(format!(
            "selector {selector:?} needs a prefix, such as target:<name>"
        ));
    };
    match prefix {
        "target" if !rest.is_empty() => Ok(rest.to_string()),
        "target" => Err(format!("selector {selector:?} names no target")),
        reserved if SELECTOR_PREFIXES.contains(&reserved) => Err(format!(
            "selector {selector:?}: the {reserved}: prefix is reserved and not implemented yet"
        )),
        unknown => Err(format!(
            "selector {selector:?}: unknown prefix {unknown:?}; expected one of {}",
            SELECTOR_PREFIXES.join(", ")
        )),
    }
}

/// A decoded request line.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// Echoed in the reply; `Value::Null` when the request had none.
    pub id: Value,
    pub cmd: String,
    pub window: Option<WindowSelector>,
    pub settle: Settle,
    /// The command's arguments: every field except `id`, `cmd`, `window` and `settle`, with a
    /// `selector` resolved into `target`.
    pub args: Map<String, Value>,
}

/// Which window a command addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowSelector {
    /// Position in the window list (see the `windows` command).
    Index(usize),
    /// A name registered with `gpui_automation::register_window`.
    Name(String),
}

impl WindowSelector {
    pub fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::String(name) if !name.is_empty() => Ok(Self::Name(name.clone())),
            Value::Number(n) => n
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .map(Self::Index)
                .ok_or_else(|| format!("window index must be a non-negative integer, got {n}")),
            other => Err(format!(
                "window must be a name or an index, got {}",
                crate::matcher::compact(other)
            )),
        }
    }
}

impl fmt::Display for WindowSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Index(index) => write!(f, "#{index}"),
            Self::Name(name) => write!(f, "{name:?}"),
        }
    }
}

/// A request that could not be decoded, with whatever `id` could be salvaged
/// so the driver can still correlate the error reply.
#[derive(Debug, Clone, PartialEq)]
pub struct BadRequest {
    pub id: Value,
    pub message: String,
}

pub fn parse_request(line: &str) -> Result<Request, BadRequest> {
    let value: Value = serde_json::from_str(line).map_err(|err| BadRequest {
        id: Value::Null,
        message: format!("invalid JSON: {err}"),
    })?;
    let Value::Object(mut args) = value else {
        return Err(BadRequest {
            id: Value::Null,
            message: "a request must be a JSON object".into(),
        });
    };
    let id = args.remove("id").unwrap_or(Value::Null);
    let bad = |message: String| BadRequest {
        id: id.clone(),
        message,
    };
    let cmd = match args.remove("cmd") {
        Some(Value::String(cmd)) if !cmd.is_empty() => cmd,
        Some(other) => {
            return Err(bad(format!(
                "cmd must be a non-empty string, got {}",
                crate::matcher::compact(&other)
            )));
        }
        None => return Err(bad("missing \"cmd\"".into())),
    };
    let window = match args.remove("window") {
        None | Some(Value::Null) => None,
        Some(value) => Some(WindowSelector::from_value(&value).map_err(bad)?),
    };
    let settle = match args.remove("settle") {
        None | Some(Value::Null) => Settle::default(),
        Some(value) => Settle::from_value(&value).map_err(bad)?,
    };
    if let Some(selector) = args.remove("selector") {
        let Value::String(selector) = selector else {
            return Err(bad(format!(
                "selector must be a string, got {}",
                crate::matcher::compact(&selector)
            )));
        };
        if args.contains_key("target") {
            return Err(bad("give a selector or a target, not both".into()));
        }
        args.insert(
            "target".into(),
            Value::String(parse_selector(&selector).map_err(bad)?),
        );
    }
    Ok(Request {
        id,
        cmd,
        window,
        settle,
        args,
    })
}

pub fn ok_reply(id: Value, result: Value) -> Value {
    json!({"id": id, "ok": true, "result": result})
}

pub fn error_reply(id: Value, error: &anyhow::Error) -> Value {
    json!({"id": id, "ok": false, "error": format!("{error:#}")})
}

pub fn event(name: &str, mut fields: Map<String, Value>) -> Value {
    fields.insert("event".into(), Value::String(name.into()));
    Value::Object(fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn settle_defaults_to_a_frame_and_reserves_quiet() {
        let request = parse_request(r#"{"cmd": "click", "target": "a"}"#).unwrap();
        assert_eq!(request.settle, Settle::Frame);
        let request =
            parse_request(r#"{"cmd": "click", "target": "a", "settle": "none"}"#).unwrap();
        assert_eq!(request.settle, Settle::None);
        assert!(!request.args.contains_key("settle"));
        let err = parse_request(r#"{"id": 3, "cmd": "click", "settle": "quiet"}"#).unwrap_err();
        assert_eq!(err.id, json!(3));
        assert!(err.message.contains("reserved"), "{}", err.message);
        assert!(parse_request(r#"{"cmd": "click", "settle": "soon"}"#).is_err());
    }

    #[test]
    fn selectors_resolve_target_names_and_reserve_the_rest() {
        let request =
            parse_request(r#"{"cmd": "click", "selector": "target:sidebar/new"}"#).unwrap();
        assert_eq!(
            Value::Object(request.args),
            json!({"target": "sidebar/new"})
        );
        for (selector, error) in [
            ("role:button[Save]", "reserved"),
            ("label:Search", "reserved"),
            ("target:a >> nth=0", "not implemented"),
            ("color:red", "unknown prefix"),
            ("sidebar/new", "needs a prefix"),
            ("target:", "names no target"),
        ] {
            let err = parse_selector(selector).unwrap_err();
            assert!(err.contains(error), "{selector}: {err}");
        }
        let both = parse_request(r#"{"cmd": "click", "selector": "target:a", "target": "b"}"#);
        assert!(both.unwrap_err().message.contains("not both"));
    }

    #[test]
    fn parses_id_cmd_window_and_args() {
        let request =
            parse_request(r#"{"id": 7, "cmd": "click", "window": "reader", "x": 1, "y": 2}"#)
                .unwrap();
        assert_eq!(request.id, json!(7));
        assert_eq!(request.cmd, "click");
        assert_eq!(request.window, Some(WindowSelector::Name("reader".into())));
        assert_eq!(Value::Object(request.args), json!({"x": 1, "y": 2}));
    }

    #[test]
    fn window_index_and_missing_id() {
        let request = parse_request(r#"{"cmd": "state", "window": 1}"#).unwrap();
        assert_eq!(request.id, Value::Null);
        assert_eq!(request.window, Some(WindowSelector::Index(1)));
        assert!(request.args.is_empty());
    }

    #[test]
    fn errors_keep_the_id_when_possible() {
        let err = parse_request(r#"{"id": "a"}"#).unwrap_err();
        assert_eq!(err.id, json!("a"));
        assert!(err.message.contains("missing \"cmd\""));

        let err = parse_request(r#"{"id": 3, "cmd": "state", "window": -1}"#).unwrap_err();
        assert_eq!(err.id, json!(3));
        assert!(err.message.contains("non-negative"), "{}", err.message);

        let err = parse_request(r#"{"id": 3, "cmd": 5}"#).unwrap_err();
        assert!(err.message.contains("cmd must be"));

        let err = parse_request("not json").unwrap_err();
        assert_eq!(err.id, Value::Null);
        assert!(err.message.starts_with("invalid JSON"));

        let err = parse_request("[1]").unwrap_err();
        assert!(err.message.contains("JSON object"));
    }

    #[test]
    fn replies_have_the_documented_shape() {
        assert_eq!(
            ok_reply(json!(1), json!({"a": 1})),
            json!({"id": 1, "ok": true, "result": {"a": 1}})
        );
        let error = anyhow::anyhow!("inner").context("outer");
        assert_eq!(
            error_reply(Value::Null, &error),
            json!({"id": null, "ok": false, "error": "outer: inner"})
        );
        assert_eq!(event("ready", Map::new()), json!({"event": "ready"}));
    }
}
