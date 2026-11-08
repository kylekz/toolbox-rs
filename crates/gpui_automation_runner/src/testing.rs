//! A made-up app for the runner's own tests, with one extension of each kind: a header key
//! (`mock`), a runner action (`mock_log`), a variable (`${demo_mock_log}`), an environment
//! variable only its fixture sets (`DEMO_MOCK`), a default the header's `env` may replace
//! (`DEMO_KEY`) and variables no header may set (`DEMO_LIVE*`).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{Fixture, FixtureCx, Profile, RunnerApp};

pub(crate) struct DemoApp;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DemoHeader {
    mock: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum DemoAction {
    MockLog {
        #[serde(default)]
        wait_ms: Option<u64>,
    },
}

impl RunnerApp for DemoApp {
    type Header = DemoHeader;
    type Action = DemoAction;
    const HEADER_KEYS: &'static [&'static str] = &["mock"];
    const ACTIONS: &'static [&'static str] = &["mock_log"];
    const VARS: &'static [&'static str] = &["demo_mock_log"];
    const RESERVED_ENV: &'static [&'static str] = &["DEMO_MOCK"];

    fn profile(&self) -> Profile {
        Profile {
            manifest_dir: env!("CARGO_MANIFEST_DIR"),
            name: "demo",
            build_hint: "cargo build -p demo",
            env_prefix: "DEMO_",
            data_dir_var: "DEMO_DATA_DIR",
            default_window_size: "1440x900",
            platforms: &["windows", "linux", "macos"],
        }
    }

    fn check_env(&self, env: &BTreeMap<String, String>) -> Result<()> {
        match env.keys().find(|name| name.starts_with("DEMO_LIVE")) {
            Some(name) => bail!("the scenario header's env can't set {name}: live runs are ours"),
            None => Ok(()),
        }
    }

    fn fixture(&self, header: &DemoHeader, cx: &FixtureCx) -> Result<Box<dyn Fixture<DemoAction>>> {
        Ok(Box::new(DemoFixture {
            mock: header.mock.clone(),
            log: cx.out_dir.join("mock.log"),
            default_key: !cx.env.contains_key("DEMO_KEY"),
        }))
    }
}

struct DemoFixture {
    mock: Option<String>,
    log: PathBuf,
    /// The header's `env` names no `DEMO_KEY`, so the fixture sets one.
    default_key: bool,
}

impl Fixture<DemoAction> for DemoFixture {
    fn vars(&self) -> Vec<(&'static str, String)> {
        vec![("demo_mock_log", self.log.display().to_string())]
    }

    fn env(&self) -> Vec<(OsString, Option<OsString>)> {
        self.mock
            .iter()
            .map(|mock| ("DEMO_MOCK".into(), Some(mock.into())))
            .chain(
                self.default_key
                    .then(|| ("DEMO_KEY".into(), Some("dummy".into()))),
            )
            .collect()
    }

    fn run(&mut self, action: &DemoAction) -> Result<Value> {
        let DemoAction::MockLog { .. } = action;
        Ok(json!({"calls": [], "mock": self.mock}))
    }

    fn retry_for(&self, action: &DemoAction) -> Option<Duration> {
        let DemoAction::MockLog { wait_ms } = action;
        wait_ms.map(Duration::from_millis)
    }
}
