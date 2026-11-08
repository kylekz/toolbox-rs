//! Scenario files: JSONL where each line is an automation command, possibly
//! carrying runner-only assertion keys, or a runner-only step (restarting the
//! app, or one of the app's own runner actions). See docs/automation.md.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use gpui_automation_protocol::matcher::{Expectation, compact};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::app::Extension;

/// Keys the runner consumes; they are never sent to the app.
pub(crate) const RUNNER_KEYS: &[&str] = &[
    "expect",
    "expect_result",
    "expect_error",
    "platforms",
    "runner_timeout_ms",
    "save",
];

/// Variables the runner defines itself; `save` can't take their names.
pub const BUILTIN_VARS: &[&str] = &["data_dir", "out_dir", "workspace"];

/// The keys of a line that is the runner's own step rather than a command: an app's actions
/// may not take them.
pub(crate) const STEP_KEYS: &[&str] = &["cmd", "restart", "id"];

/// The header keys the runner takes itself; an app's own may not reuse them.
pub(crate) const HEADER_KEYS: &[&str] = &[
    "app_args",
    "window_size",
    "env",
    "description",
    "platforms",
    "foreground",
    "opt_in",
];

/// Environment the runner sets itself; a header `env` may not override it.
fn runner_env(ext: &dyn Extension) -> Vec<&'static str> {
    let mut env = vec![
        ext.app_profile().data_dir_var,
        "E2E_OUT_DIR",
        "E2E_DATA_DIR",
    ];
    env.extend_from_slice(ext.reserved_env());
    env
}

/// The optional first line: `{"scenario": {...}}`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    /// Extra app arguments after the runner's own flags.
    #[serde(default)]
    pub app_args: Vec<String>,
    /// `WxH` in logical pixels (default 1440x900).
    pub window_size: Option<String>,
    /// Extra environment for the app.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Free text shown in verbose output.
    pub description: Option<String>,
    /// Operating systems the scenario runs on, as `std::env::consts::OS`
    /// names; empty means all. Others skip it (platform-specific chrome).
    #[serde(default)]
    pub platforms: Vec<String>,
    /// The app's own header keys ([`crate::RunnerApp::HEADER_KEYS`]), checked at load and
    /// handed to its fixture.
    #[serde(skip)]
    pub app: Map<String, Value>,
    /// Run this scenario's app in the foreground (focused, in front) even
    /// when the runner runs in the background: for behaviour gpui only has in
    /// an active window, such as focus and blur listeners or the caret and
    /// selection of text inputs.
    #[serde(default)]
    pub foreground: bool,
    /// A variable of the app's (its [`crate::Profile::env_prefix`]) the runner's own
    /// environment must set to `1` for the scenario to run (`SLIDE_PLAYER_REAL`): checks that
    /// need a real engine or take the focus. Others skip it. The app doesn't get it.
    pub opt_in: Option<String>,
}

/// One line of a scenario.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// 1-based line number in the file, also used as the request id.
    pub line: usize,
    pub action: Action,
    /// Checked against a fresh `state` after the action. Kept as JSON
    /// (validated at load) so `${...}` variables can be substituted per run.
    pub expect: Option<Value>,
    /// Checked against the action's own result: a command's `result`, or a
    /// restart's `ready` event.
    pub expect_result: Option<Value>,
    /// The command must fail with an error containing this text.
    pub expect_error: Option<String>,
    /// Screenshot name, when the runner chooses the path.
    pub screenshot: Option<String>,
    /// Explicit reply timeout from `runner_timeout_ms`.
    pub timeout: Option<Duration>,
    /// `save`: variables to set from the action's result, as (name, JSON
    /// pointer) pairs; later lines use them as `${name}`.
    pub save: Vec<(String, String)>,
    /// `platforms`: the operating systems this line runs on, named as in the
    /// header's (empty: all); elsewhere the runner passes over it.
    pub platforms: Vec<String>,
}

/// What a step does before its `expect` is checked.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// An automation command sent to the app (runner keys removed).
    Command(Map<String, Value>),
    /// Quit the app (unless a `quit` step already did) and start it again
    /// with the same data dir and environment.
    Restart(Restart),
    /// One of the app's own runner actions (`{"gh_log": {}}`), run by its fixture.
    App(Value),
    /// Nothing: the line only asserts on `state`.
    Check,
}

/// `{"restart": {"app_args"?: [...], "data_dir"?: bool}}`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Restart {
    /// Replaces the header's `app_args` for this launch and every later one.
    pub app_args: Option<Vec<String>>,
    /// `false` launches without `--data-dir` (this and every later launch; `true` puts it back),
    /// so the app finds its data dir as a normal start does: through its data-dir variable
    /// ([`crate::Profile::data_dir_var`]), which the runner always points at the scenario's own
    /// data dir.
    pub data_dir: Option<bool>,
}

impl Step {
    /// Whether the line runs on `os` (a `std::env::consts::OS` value).
    pub fn runs_on(&self, os: &str) -> bool {
        self.platforms.is_empty() || self.platforms.iter().any(|p| p == os)
    }

    /// Expectations with the run's variables substituted.
    pub fn expectations(raw: &Option<Value>, vars: &[(&str, String)]) -> Result<Vec<Expectation>> {
        let Some(raw) = raw else {
            return Ok(Vec::new());
        };
        let mut value = raw.clone();
        substitute(&mut value, vars);
        Expectation::parse_list(&value).map_err(|err| anyhow!(err))
    }

    /// The automation command this step sends, if it sends one.
    pub fn cmd(&self) -> Option<&str> {
        self.request()
            .and_then(|request| request.get("cmd"))
            .and_then(Value::as_str)
    }

    pub fn request(&self) -> Option<&Map<String, Value>> {
        match &self.action {
            Action::Command(request) => Some(request),
            _ => None,
        }
    }

    /// The window selector this step's command addresses, reused for the
    /// `state` fetched to check `expect`.
    pub fn window(&self) -> Option<&Value> {
        self.request().and_then(|request| request.get("window"))
    }

    /// How long to wait for the reply: a base allowance plus whatever the
    /// command itself is asked to wait for.
    pub fn reply_timeout(&self, base: Duration) -> Duration {
        if let Some(timeout) = self.timeout {
            return timeout;
        }
        let Some(request) = self.request() else {
            return base;
        };
        let millis = |key: &str| request.get(key).and_then(Value::as_u64).unwrap_or(0);
        let own = millis("ms")
            .max(millis("timeout_ms"))
            .max(millis("hold_ms") + millis("step_ms") * millis("steps").max(1));
        base + Duration::from_millis(own)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Scenario {
    pub name: String,
    pub path: PathBuf,
    pub header: Header,
    pub steps: Vec<Step>,
    /// The app's window size when the header names none ([`crate::Profile`]).
    pub default_window_size: &'static str,
}

impl Scenario {
    pub(crate) fn load(path: &Path, ext: &dyn Extension) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let name = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .ok_or_else(|| anyhow!("{} has no file name", path.display()))?;
        Self::parse(&name, path, &text, ext).with_context(|| format!("in {}", path.display()))
    }

    pub(crate) fn parse(name: &str, path: &Path, text: &str, ext: &dyn Extension) -> Result<Self> {
        let profile = ext.app_profile();
        let mut header = None;
        let mut steps = Vec::new();
        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            let trimmed = raw.trim_start_matches('\u{feff}').trim();
            if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with('#') {
                continue;
            }
            let value: Value = serde_json::from_str(trimmed)
                .with_context(|| format!("line {line}: invalid JSON"))?;
            let Value::Object(mut object) = value else {
                bail!("line {line}: expected a JSON object");
            };
            if let Some(scenario) = object.remove("scenario") {
                if !object.is_empty() || header.is_some() || !steps.is_empty() {
                    bail!("line {line}: the scenario header must be the first line, on its own");
                }
                header = Some(
                    parse_header(scenario, ext, path)
                        .with_context(|| format!("line {line}: bad scenario header"))?,
                );
                continue;
            }
            steps.push(
                parse_step(line, object, ext, &profile).with_context(|| format!("line {line}"))?,
            );
        }
        if steps.is_empty() {
            bail!("the scenario has no steps");
        }
        let header = match header {
            Some(header) => header,
            None => parse_header(Value::Object(Map::new()), ext, path)?,
        };
        if let Some(size) = &header.window_size {
            gpui_automation_protocol::parse_window_size(size)?;
        }
        let runner_env = runner_env(ext);
        if let Some(name) = header.env.keys().find(|name| {
            runner_env
                .iter()
                .any(|owned| owned.eq_ignore_ascii_case(name))
        }) {
            bail!("the scenario header's env can't set {name}: the runner sets it");
        }
        if let Some(unknown) = header
            .platforms
            .iter()
            .find(|os| !profile.platforms.contains(&os.as_str()))
        {
            bail!(
                "unknown platform {unknown:?} in the scenario header; expected one of {}",
                profile.platforms.join(", ")
            );
        }
        if let Some(name) = &header.opt_in
            && (!name.starts_with(profile.env_prefix) || runner_env.contains(&name.as_str()))
        {
            bail!(
                "the scenario header's opt_in must be a {} variable of its own, not {name:?}",
                profile.env_prefix
            );
        }
        check_step_order(&steps)?;
        Ok(Self {
            name: name.to_string(),
            path: path.to_path_buf(),
            header,
            steps,
            default_window_size: profile.default_window_size,
        })
    }

    /// Whether the scenario runs on `os` (a `std::env::consts::OS` value).
    pub fn runs_on(&self, os: &str) -> bool {
        self.header.platforms.is_empty() || self.header.platforms.iter().any(|p| p == os)
    }

    /// The header's `opt_in` variable when `var` (the runner's environment)
    /// doesn't set it to `1`: the scenario is skipped.
    pub fn missing_opt_in(&self, var: impl Fn(&str) -> Option<String>) -> Option<&str> {
        self.header
            .opt_in
            .as_deref()
            .filter(|name| var(name).as_deref() != Some("1"))
    }

    pub fn window_size(&self) -> &str {
        self.header
            .window_size
            .as_deref()
            .unwrap_or(self.default_window_size)
    }
}

/// The header: the runner's keys into [`Header`], the app's into [`Header::app`] after its
/// [`crate::RunnerApp::Header`] accepts them.
fn parse_header(header: Value, ext: &dyn Extension, path: &Path) -> Result<Header> {
    let Value::Object(object) = header else {
        bail!("expected an object, got {}", compact(&header));
    };
    let (mut core, mut app) = (Map::new(), Map::new());
    for (key, value) in object {
        if HEADER_KEYS.contains(&key.as_str()) {
            core.insert(key, value);
        } else if ext.header_keys().contains(&key.as_str()) {
            app.insert(key, value);
        } else {
            let known: Vec<&str> = HEADER_KEYS
                .iter()
                .chain(ext.header_keys())
                .copied()
                .collect();
            bail!("unknown key {key:?}; expected one of {}", known.join(", "));
        }
    }
    let mut header: Header = serde_json::from_value(Value::Object(core))?;
    ext.check_header(&app, path)?;
    header.app = app;
    Ok(header)
}

fn parse_step(
    line: usize,
    mut object: Map<String, Value>,
    ext: &dyn Extension,
    profile: &crate::Profile,
) -> Result<Step> {
    let expect = take_expectations(&mut object, "expect")?;
    let expect_result = take_expectations(&mut object, "expect_result")?;
    let expect_error = match object.remove("expect_error") {
        None => None,
        Some(Value::String(text)) => Some(text),
        Some(other) => bail!("expect_error must be a string, got {}", compact(&other)),
    };
    let timeout = match object.remove("runner_timeout_ms") {
        None => None,
        Some(value) => Some(Duration::from_millis(value.as_u64().ok_or_else(|| {
            anyhow!("runner_timeout_ms must be a non-negative integer")
        })?)),
    };
    let save = parse_save(object.remove("save"), ext.vars())?;
    let platforms = parse_platforms(object.remove("platforms"), profile.platforms)?;
    if !save.is_empty() && expect_error.is_some() {
        bail!("save needs a result, and expect_error means there is none");
    }
    debug_assert!(RUNNER_KEYS.iter().all(|key| !object.contains_key(*key)));
    if expect_error.is_some() && expect_result.is_some() {
        bail!("expect_error and expect_result exclude each other");
    }

    let step = |action| Step {
        line,
        action,
        expect: expect.clone(),
        expect_result: expect_result.clone(),
        expect_error: expect_error.clone(),
        screenshot: None,
        timeout,
        save: save.clone(),
        platforms: platforms.clone(),
    };

    if let Some(restart) = object.remove("restart") {
        ensure_alone(&object, "restart")?;
        if expect_error.is_some() {
            bail!("a restart can't expect an error");
        }
        if timeout.is_some() {
            bail!("runner_timeout_ms doesn't apply to a restart (it uses --ready-timeout-ms)");
        }
        let restart = serde_json::from_value(restart).context("bad restart")?;
        return Ok(step(Action::Restart(restart)));
    }
    if let Some(key) = ext.actions().iter().find(|key| object.contains_key(**key)) {
        ensure_alone_but(&object, key)?;
        if expect_error.is_some() {
            bail!("{key} is a runner action, which can't expect an error");
        }
        let action = Value::Object(object);
        ext.check_action(&action)
            .with_context(|| format!("bad {key}"))?;
        return Ok(step(Action::App(action)));
    }
    if object.is_empty() {
        if expect.is_none() {
            bail!("a line needs a cmd, a restart or an expect");
        }
        if expect_error.is_some() || expect_result.is_some() || !save.is_empty() {
            bail!("expect_error, expect_result and save need a cmd or a restart");
        }
        return Ok(step(Action::Check));
    }

    let cmd = match object.get("cmd") {
        Some(Value::String(cmd)) if !cmd.is_empty() => cmd.clone(),
        Some(other) => bail!("cmd must be a non-empty string, got {}", compact(other)),
        None => bail!("missing \"cmd\""),
    };
    if object.contains_key("id") {
        bail!("don't set \"id\": the runner numbers requests by line");
    }
    let mut screenshot = None;
    if cmd == "screenshot" {
        match (object.remove("name"), object.contains_key("path")) {
            (Some(Value::String(name)), false) => screenshot = Some(sanitize_name(&name)?),
            (Some(_), true) => bail!("give a screenshot either a name or a path, not both"),
            (Some(other), false) => {
                bail!("screenshot name must be a string, got {}", compact(&other))
            }
            (None, true) => {}
            (None, false) => bail!("a screenshot needs a name (or an explicit path)"),
        }
    }
    if cmd == "quit" && expect.is_some() {
        bail!("quit ends the app, so there is no state to expect afterwards");
    }
    Ok(Step {
        screenshot,
        ..step(Action::Command(object))
    })
}

/// A line's `platforms`: a list of the header's platform names.
fn parse_platforms(platforms: Option<Value>, known: &[&str]) -> Result<Vec<String>> {
    let Some(platforms) = platforms else {
        return Ok(Vec::new());
    };
    let names: Vec<String> = serde_json::from_value(platforms.clone()).map_err(|_| {
        anyhow!(
            "platforms must be a list of platform names, got {}",
            compact(&platforms)
        )
    })?;
    if let Some(unknown) = names.iter().find(|os| !known.contains(&os.as_str())) {
        bail!(
            "unknown platform {unknown:?} in platforms; expected one of {}",
            known.join(", ")
        );
    }
    Ok(names)
}

/// `{"name": "/json/pointer", ...}`: variable names are identifiers other
/// than the runner's own; pointers are RFC 6901 (`""` is the whole result).
fn parse_save(save: Option<Value>, app_vars: &[&str]) -> Result<Vec<(String, String)>> {
    let Some(save) = save else {
        return Ok(Vec::new());
    };
    let Value::Object(map) = save else {
        bail!(
            "save must map variable names to JSON pointers, got {}",
            compact(&save)
        );
    };
    map.into_iter()
        .map(|(name, pointer)| {
            let valid_name = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !valid_name {
                bail!("save: {name:?} is not a variable name (letters, digits and _)");
            }
            if BUILTIN_VARS.contains(&name.as_str()) || app_vars.contains(&name.as_str()) {
                bail!("save: ${{{name}}} is the runner's own variable");
            }
            match pointer {
                Value::String(pointer) if pointer.is_empty() || pointer.starts_with('/') => {
                    Ok((name, pointer))
                }
                other => bail!("save: {name} needs a JSON pointer, got {}", compact(&other)),
            }
        })
        .collect()
}

/// A runner-only step (`restart`) takes no command arguments.
fn ensure_alone(object: &Map<String, Value>, key: &str) -> Result<()> {
    if object.contains_key("cmd") {
        bail!("{key} and cmd exclude each other: put them on separate lines");
    }
    if !object.is_empty() {
        let extra: Vec<&str> = object.keys().map(String::as_str).collect();
        bail!("unknown key(s) on a {key} line: {}", extra.join(", "));
    }
    Ok(())
}

/// An app action's line holds that action's key and the runner's own keys alone.
fn ensure_alone_but(object: &Map<String, Value>, key: &str) -> Result<()> {
    if let Some(other) = object.keys().find(|other| *other != key) {
        bail!("unknown key {other:?} on a {key} line: a runner action takes no command arguments");
    }
    Ok(())
}

/// Checks that need the whole file: once the app has quit only a restart
/// brings it back.
fn check_step_order(steps: &[Step]) -> Result<()> {
    let mut quit = false;
    for step in steps {
        match &step.action {
            Action::Restart(_) => quit = false,
            Action::App(_) if quit && step.expect.is_none() => {}
            _ if quit => bail!(
                "line {}: after quit only a restart, or a runner action without expect, may follow",
                step.line
            ),
            _ => quit = step.cmd() == Some("quit"),
        }
    }
    Ok(())
}

fn take_expectations(object: &mut Map<String, Value>, key: &str) -> Result<Option<Value>> {
    match object.remove(key) {
        None => Ok(None),
        Some(value) => {
            Expectation::parse_list(&value).map_err(|err| anyhow!("{key}: {err}"))?;
            Ok(Some(value))
        }
    }
}

/// Screenshot names become file names, so keep them portable.
fn sanitize_name(name: &str) -> Result<String> {
    let clean: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let clean = clean.trim_matches(['-', '.']).to_string();
    if clean.is_empty() {
        bail!("screenshot name {name:?} has no usable characters");
    }
    Ok(clean)
}

/// Replace `${name}` variables (`${data_dir}`, `${out_dir}`, `${workspace}`
/// and those set by `save`) in every string of a request,
/// so scenarios can refer to the per-run directories.
pub fn substitute(value: &mut Value, vars: &[(&str, String)]) {
    match value {
        Value::String(text) => {
            if text.contains("${") {
                for (name, replacement) in vars {
                    *text = text.replace(&format!("${{{name}}}"), replacement);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| substitute(item, vars)),
        Value::Object(map) => map.values_mut().for_each(|item| substitute(item, vars)),
        _ => {}
    }
}

/// Expand scenario arguments: files are taken as-is, directories contribute
/// their `*.jsonl` files, and `*`/`?` wildcards in the file-name part are
/// expanded here because Windows shells don't.
pub fn discover(args: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for arg in args {
        let text = arg.to_string_lossy();
        if text.contains(['*', '?']) {
            let dir = arg
                .parent()
                .filter(|dir| !dir.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let pattern = arg
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .ok_or_else(|| anyhow!("{text}: wildcards are only supported in file names"))?;
            if dir.to_string_lossy().contains(['*', '?']) {
                bail!("{text}: wildcards are only supported in file names");
            }
            let mut matched = list_dir(dir, |name| wildcard_match(&pattern, name))?;
            if matched.is_empty() {
                bail!("{text} matches no files");
            }
            found.append(&mut matched);
        } else if arg.is_dir() {
            let mut matched = list_dir(arg, |name| name.ends_with(".jsonl"))?;
            if matched.is_empty() {
                bail!("{} contains no .jsonl scenarios", arg.display());
            }
            found.append(&mut matched);
        } else if arg.is_file() {
            found.push(arg.clone());
        } else {
            bail!("{} does not exist", arg.display());
        }
    }
    found.dedup();
    Ok(found)
}

fn list_dir(dir: &Path, keep: impl Fn(&str) -> bool) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("listing {}", dir.display()))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_file() && keep(&name) {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

/// `*` matches any run of characters, `?` exactly one. Case-insensitive,
/// like the Windows file system.
pub fn wildcard_match(pattern: &str, name: &str) -> bool {
    fn go(pattern: &[char], name: &[char]) -> bool {
        match pattern.split_first() {
            None => name.is_empty(),
            Some(('*', rest)) => (0..=name.len()).any(|skip| go(rest, &name[skip..])),
            Some(('?', rest)) => !name.is_empty() && go(rest, &name[1..]),
            Some((c, rest)) => name
                .split_first()
                .is_some_and(|(n, name)| n.eq_ignore_ascii_case(c) && go(rest, name)),
        }
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    go(&pattern, &name)
}

#[cfg(test)]
#[path = "scenario_tests.rs"]
mod tests;
