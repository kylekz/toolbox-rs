//! What an app tells the runner about itself ([`RunnerApp`]): its executable, the environment it
//! reads, and its own scenario extensions: header keys, runner actions and the fixture that
//! serves them for a scenario ([`Fixture`]), such as a mock server or a fake `gh`
//! (docs/automation.md "Runner extensions").

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

/// The facts about an app that every scenario run needs.
#[derive(Debug, Clone)]
pub struct Profile {
    /// The app's `e2e` bin's `env!("CARGO_MANIFEST_DIR")`: the workspace (`${workspace}`) and
    /// Cargo's target folder are found from it.
    pub manifest_dir: &'static str,
    /// The app's executable name without `.exe`: the default app is `<target>/debug/<name>`.
    pub name: &'static str,
    /// How to build the app, for the error when it's missing (`cargo build -p slide`).
    pub build_hint: &'static str,
    /// Every environment variable the app reads starts with this (`SLIDE_`). The runner removes
    /// every inherited one, so nothing from the shell it runs in reaches a scenario's app, and a
    /// header's `opt_in` must name one.
    pub env_prefix: &'static str,
    /// The app's data-dir variable (`SLIDE_DATA_DIR`). The runner sets it to the scenario's data
    /// dir as well as passing `--data-dir`, so no scenario reaches the real one.
    pub data_dir_var: &'static str,
    /// `WxH` in logical pixels when a header names none.
    pub default_window_size: &'static str,
    /// The platforms (`std::env::consts::OS` names) a header's or line's `platforms` may name.
    pub platforms: &'static [&'static str],
}

/// Where a scenario's fixture lives, for [`RunnerApp::fixture`].
pub struct FixtureCx<'a> {
    /// The scenario file's name without `.jsonl`.
    pub scenario: &'a str,
    /// The scenario file; files a header names are relative to its folder.
    pub scenario_path: &'a Path,
    /// The scenario's output folder (`<out>/<scenario>`): logs, screenshots.
    pub out_dir: &'a Path,
    /// The scenario's data dir, the app's working directory.
    pub data_dir: &'a Path,
    /// The workspace the runner was started for.
    pub workspace: &'a Path,
    /// Cargo's target folder, for the app's own helper binaries (`<target>/debug/mock_gh`).
    pub target_dir: &'a Path,
    /**
     * The header's `env`, which every launch gets before the fixture's own ([`Fixture::env`]):
     * a variable the fixture only gives a default leaves one named here alone, and a sidecar
     * it starts can be given the same environment as the app.
     */
    pub env: &'a BTreeMap<String, String>,
}

/// An app's half of the runner. Each app has a small `e2e` bin:
///
/// ```ignore
/// fn main() -> std::process::ExitCode {
///     gpui_automation_runner::main(SlideRunner)
/// }
/// ```
///
/// An app with no extensions uses [`NoHeader`] and [`NoAction`] and keeps the default
/// [`fixture`](Self::fixture).
pub trait RunnerApp: Send + Sync + 'static {
    /// The app's own header keys, as one struct that rejects unknown fields
    /// (`#[serde(deny_unknown_fields)]`). The runner removes its own keys from the header first.
    type Header: DeserializeOwned + Default + Send + Sync + 'static;
    /// The app's runner actions, externally tagged: a line `{"gh_log": {}}` is the variant
    /// `gh_log`. Each runs in the runner process ([`Fixture::run`]), never in the app.
    type Action: DeserializeOwned + Send + Sync + 'static;
    /// The header keys [`Self::Header`] takes. They may not be the runner's own.
    const HEADER_KEYS: &'static [&'static str] = &[];
    /// The keys that start a line of [`Self::Action`]. They may not be the runner's own.
    const ACTIONS: &'static [&'static str] = &[];
    /// `${...}` variables the fixture defines ([`Fixture::vars`]), which `save` can't take.
    const VARS: &'static [&'static str] = &[];
    /// Environment the fixture sets ([`Fixture::env`]), which a header's `env` may not.
    const RESERVED_ENV: &'static [&'static str] = &[];

    fn profile(&self) -> Profile;

    /// Lines appended to the runner's `--help` options for [`Self::option`]'s flags.
    fn usage(&self) -> &'static str {
        ""
    }

    /// A command-line flag the runner doesn't know: return `true` when it's the app's.
    /// `value` takes the flag's value from the command line.
    fn option(&mut self, flag: &str, value: &mut dyn FnMut() -> Result<String>) -> Result<bool> {
        let _ = (flag, value);
        Ok(false)
    }

    /// Load-time checks of a header beyond what deserializing it checks, such as that the
    /// files it names exist next to `scenario_path`.
    fn check(&self, header: &Self::Header, scenario_path: &Path) -> Result<()> {
        let _ = (header, scenario_path);
        Ok(())
    }

    /// Load-time checks of the header's `env` beyond the runner's own (it may not set the
    /// runner's or the fixture's variables): variables no scenario of the app may set, say.
    fn check_env(&self, env: &BTreeMap<String, String>) -> Result<()> {
        let _ = env;
        Ok(())
    }

    /// The fixture serving one scenario run, made before its app's first launch and dropped
    /// after its last exit.
    fn fixture(
        &self,
        header: &Self::Header,
        cx: &FixtureCx,
    ) -> Result<Box<dyn Fixture<Self::Action>>> {
        let _ = (header, cx);
        Ok(Box::new(NoFixture))
    }
}

/// One scenario run's app-specific services: the variables, environment and arguments every
/// launch of its app gets, and the runner actions its lines ask for.
pub trait Fixture<A>: Send {
    /// `${...}` variables for app args, commands and expectations ([`RunnerApp::VARS`]).
    fn vars(&self) -> Vec<(&'static str, String)> {
        Vec::new()
    }

    /// Environment for every launch: a value to set, or `None` to remove.
    fn env(&self) -> Vec<(OsString, Option<OsString>)> {
        Vec::new()
    }

    /// Arguments for every launch, after the runner's own flags and before the header's
    /// `app_args`.
    fn args(&self) -> Vec<String> {
        Vec::new()
    }

    /// Runs a runner action. The result is what the line's `expect_result` and `save` see.
    fn run(&mut self, action: &A) -> Result<Value>;

    /// For an action whose result can lag behind the app (a log another process writes): run
    /// it again until the line's `expect_result` holds, for this long at most.
    fn retry_for(&self, action: &A) -> Option<Duration> {
        let _ = action;
        None
    }

    /**
     * After the app's last exit, whatever the scenario's outcome so far: checks such as that no
     * child process outlived the app, and their cleanup. An error fails a scenario that passed
     * and is added to the message of one that failed.
     */
    fn finish(&mut self) -> Result<()> {
        Ok(())
    }
}

/// A header with no app keys.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoHeader {}

/// No app actions.
#[derive(Debug, Deserialize)]
pub enum NoAction {}

/// The fixture of an app with no extensions.
pub struct NoFixture;

impl<A> Fixture<A> for NoFixture {
    fn run(&mut self, _action: &A) -> Result<Value> {
        bail!("this app has no runner actions")
    }
}

/// [`RunnerApp`] without its types, for the scenario parser and the session: app header keys
/// and actions stay JSON until a fixture needs them typed.
pub(crate) trait Extension: Sync {
    fn header_keys(&self) -> &'static [&'static str];
    fn actions(&self) -> &'static [&'static str];
    fn vars(&self) -> &'static [&'static str];
    fn reserved_env(&self) -> &'static [&'static str];
    fn app_profile(&self) -> Profile;
    /// Deserializes and checks the app's part of a header, and checks its `env`.
    fn check_header(
        &self,
        header: &Map<String, Value>,
        env: &BTreeMap<String, String>,
        scenario_path: &Path,
    ) -> Result<()>;
    /// Deserializes an action line (`{"gh_log": {...}}`).
    fn check_action(&self, action: &Value) -> Result<()>;
    fn app_fixture(
        &self,
        header: &Map<String, Value>,
        cx: &FixtureCx,
    ) -> Result<Box<dyn ErasedFixture>>;
}

pub(crate) trait ErasedFixture: Send {
    fn vars(&self) -> Vec<(&'static str, String)>;
    fn env(&self) -> Vec<(OsString, Option<OsString>)>;
    fn args(&self) -> Vec<String>;
    fn run(&mut self, action: &Value) -> Result<Value>;
    fn retry_for(&self, action: &Value) -> Result<Option<Duration>>;
    fn finish(&mut self) -> Result<()>;
}

fn typed_header<H: DeserializeOwned>(header: &Map<String, Value>) -> Result<H> {
    serde_json::from_value(Value::Object(header.clone())).map_err(|err| anyhow!("{err}"))
}

fn typed_action<T: DeserializeOwned>(action: &Value) -> Result<T> {
    serde_json::from_value(action.clone()).map_err(|err| anyhow!("{err}"))
}

impl<T: RunnerApp> Extension for T {
    fn header_keys(&self) -> &'static [&'static str] {
        T::HEADER_KEYS
    }

    fn actions(&self) -> &'static [&'static str] {
        T::ACTIONS
    }

    fn vars(&self) -> &'static [&'static str] {
        T::VARS
    }

    fn reserved_env(&self) -> &'static [&'static str] {
        T::RESERVED_ENV
    }

    fn app_profile(&self) -> Profile {
        RunnerApp::profile(self)
    }

    fn check_header(
        &self,
        header: &Map<String, Value>,
        env: &BTreeMap<String, String>,
        scenario_path: &Path,
    ) -> Result<()> {
        let typed: T::Header = typed_header(header)?;
        self.check(&typed, scenario_path)?;
        self.check_env(env)
    }

    fn check_action(&self, action: &Value) -> Result<()> {
        typed_action::<T::Action>(action).map(drop)
    }

    fn app_fixture(
        &self,
        header: &Map<String, Value>,
        cx: &FixtureCx,
    ) -> Result<Box<dyn ErasedFixture>> {
        let typed: T::Header = typed_header(header)?;
        Ok(Box::new(Typed::<T> {
            inner: RunnerApp::fixture(self, &typed, cx)?,
        }))
    }
}

struct Typed<T: RunnerApp> {
    inner: Box<dyn Fixture<T::Action>>,
}

impl<T: RunnerApp> ErasedFixture for Typed<T> {
    fn vars(&self) -> Vec<(&'static str, String)> {
        self.inner.vars()
    }

    fn env(&self) -> Vec<(OsString, Option<OsString>)> {
        self.inner.env()
    }

    fn args(&self) -> Vec<String> {
        self.inner.args()
    }

    fn run(&mut self, action: &Value) -> Result<Value> {
        self.inner.run(&typed_action(action)?)
    }

    fn retry_for(&self, action: &Value) -> Result<Option<Duration>> {
        Ok(self.inner.retry_for(&typed_action(action)?))
    }

    fn finish(&mut self) -> Result<()> {
        self.inner.finish()
    }
}

/// A profile's paths: the default app, and where helper binaries are.
pub(crate) fn default_app(profile: &Profile, target_dir: &Path) -> PathBuf {
    target_dir
        .join("debug")
        .join(format!("{}{}", profile.name, std::env::consts::EXE_SUFFIX))
}
