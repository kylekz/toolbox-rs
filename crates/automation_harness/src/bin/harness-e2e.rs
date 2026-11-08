//! The scenario runner set up for the harness: the whole of an app's `e2e` bin when the app has
//! no runner extensions.

use std::process::ExitCode;

use gpui_automation_runner::{NoAction, NoHeader, Profile, RunnerApp};

struct Harness;

impl RunnerApp for Harness {
    type Header = NoHeader;
    type Action = NoAction;

    fn profile(&self) -> Profile {
        Profile {
            manifest_dir: env!("CARGO_MANIFEST_DIR"),
            name: "harness",
            build_hint: "cargo build -p automation_harness",
            env_prefix: "HARNESS_",
            data_dir_var: "HARNESS_DATA_DIR",
            default_window_size: "1440x900",
            platforms: &["windows", "linux", "macos"],
        }
    }
}

fn main() -> ExitCode {
    gpui_automation_runner::main(Harness)
}
