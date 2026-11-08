//! The e2e scenario runner as a library (docs/automation.md "The runner"). Each app has a small
//! `e2e` bin that hands [`main`] its [`RunnerApp`]: the app's executable, the environment it
//! reads, and its own scenario extensions.
//!
//! ```text
//! cargo run -p <app> --bin e2e -- [options] <scenario.jsonl | dir | glob>...
//! ```
//!
//! gpui: **never**. The runner drives the app over its stdio and shares only the JSON matcher
//! with the app's in-process automation (`gpui_automation_protocol`).

mod app;
mod display;
mod foreground;
mod http;
mod process;
mod run;
mod scenario;
mod sidecar;
#[cfg(test)]
mod testing;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};

use crate::app::Extension;
pub use crate::app::{Fixture, FixtureCx, NoAction, NoFixture, NoHeader, Profile, RunnerApp};
pub use crate::http::HttpResponse;
use crate::run::{Config, Outcome};
use crate::scenario::Scenario;
pub use crate::sidecar::Sidecar;

fn usage(profile: &Profile, app_options: &str) -> String {
    let name = profile.name;
    let data_dir_var = profile.data_dir_var;
    format!(
        "\
usage: e2e [options] <scenario.jsonl | directory | glob>...

Runs each scenario against a fresh {name} process (--automation
--automation-background --data-dir <out>/<scenario>/data --window-size WxH,
with {data_dir_var} set to the same data dir) and exits non-zero if any
fails.

options:
  --app <exe>          app under test (default: <target>/debug/{name}, .exe
                       on Windows)
  --filter <text>      only run scenarios whose name contains <text>
  --out <dir>          screenshots and logs (default: <target>/e2e); one run
                       at a time per directory
  --timeout-ms <n>     base reply timeout per command (default 30000)
  --ready-timeout-ms <n>  how long the app may take to start (default 60000)
  --keep-data          keep each scenario's data dir after it passes
  --jobs <n>           run up to <n> scenarios at once (default 1), each with
                       its own app and data dir; background runs only
  --foreground         open the app's windows normally (focused, in front)
                       instead of in the background
  --host-display       Linux: open the app on this session's display (WSLg's
                       under WSL) instead of an Xvfb display the run starts;
                       an exploratory look only: its windows appear there
  -v, --verbose        print every request and reply
  -h, --help           this help
{app_options}"
    )
}

struct Cli {
    app: Option<PathBuf>,
    filter: Option<String>,
    out: Option<PathBuf>,
    step_timeout: Duration,
    ready_timeout: Duration,
    keep_data: bool,
    jobs: usize,
    foreground: bool,
    host_display: bool,
    verbose: bool,
    scenarios: Vec<PathBuf>,
}

fn parse_cli<A: RunnerApp>(
    app: &mut A,
    args: impl IntoIterator<Item = String>,
) -> Result<Option<Cli>> {
    let usage = usage(&app.profile(), app.usage());
    let mut cli = Cli {
        app: None,
        filter: None,
        out: None,
        step_timeout: Duration::from_secs(30),
        ready_timeout: Duration::from_secs(60),
        keep_data: false,
        jobs: 1,
        foreground: false,
        host_display: false,
        verbose: false,
        scenarios: Vec::new(),
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().with_context(|| format!("{name} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--app" => cli.app = Some(value("--app")?.into()),
            "--filter" => cli.filter = Some(value("--filter")?),
            "--out" => cli.out = Some(value("--out")?.into()),
            "--timeout-ms" => cli.step_timeout = millis("--timeout-ms", &value("--timeout-ms")?)?,
            "--ready-timeout-ms" => {
                cli.ready_timeout = millis("--ready-timeout-ms", &value("--ready-timeout-ms")?)?
            }
            "--keep-data" => cli.keep_data = true,
            "--jobs" => {
                let jobs = value("--jobs")?;
                cli.jobs = jobs
                    .parse()
                    .ok()
                    .filter(|jobs| *jobs > 0)
                    .with_context(|| format!("--jobs expects a count above 0, got {jobs:?}"))?;
            }
            "--foreground" => cli.foreground = true,
            "--host-display" => cli.host_display = true,
            "-v" | "--verbose" => cli.verbose = true,
            flag if flag.starts_with('-') => {
                if !app.option(flag, &mut || value(flag))? {
                    bail!("unknown option {flag}\n\n{usage}");
                }
            }
            path => cli.scenarios.push(path.into()),
        }
    }
    if cli.scenarios.is_empty() {
        bail!("no scenarios given\n\n{usage}");
    }
    if cli.foreground && cli.jobs > 1 {
        bail!("--foreground runs one scenario at a time: each takes the focus");
    }
    Ok(Some(cli))
}

fn millis(flag: &str, value: &str) -> Result<Duration> {
    let ms: u64 = value
        .parse()
        .with_context(|| format!("{flag} expects milliseconds, got {value:?}"))?;
    Ok(Duration::from_millis(ms))
}

/// The workspace the app's `e2e` bin was built in: the nearest folder at or above its manifest
/// whose `Cargo.toml` declares `[workspace]`, so default paths work whatever the current
/// directory is. The nearest, never an outer one: a git worktree inside the app's own checkout
/// (`.claude/worktrees/<name>`) has the checkout's workspace above it.
fn workspace_root(manifest_dir: &Path) -> PathBuf {
    manifest_dir
        .ancestors()
        .find(|dir| {
            std::fs::read_to_string(dir.join("Cargo.toml"))
                .is_ok_and(|manifest| manifest.lines().any(|line| line.trim() == "[workspace]"))
        })
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn target_dir(workspace: &Path) -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => workspace.join("target"),
    }
}

/// The app's own extension names may not reuse the runner's: a key the runner adds later must
/// fail loudly rather than be quietly taken by an app.
fn check_extension(ext: &dyn Extension) -> Result<()> {
    let clash = |what: &str, theirs: &[&str], ours: &[&[&str]]| -> Result<()> {
        match theirs
            .iter()
            .find(|name| ours.iter().any(|list| list.contains(name)))
        {
            Some(name) => bail!("the app's {what} {name:?} is the runner's own"),
            None => Ok(()),
        }
    };
    clash("header key", ext.header_keys(), &[scenario::HEADER_KEYS])?;
    clash(
        "action",
        ext.actions(),
        &[scenario::RUNNER_KEYS, scenario::STEP_KEYS],
    )?;
    clash("variable", ext.vars(), &[scenario::BUILTIN_VARS])
}

/**
 * Whether the apps this runner starts may capture the screen. macOS gives a process started
 * from a terminal the terminal's Screen Recording permission; a window's own image needs none.
 */
#[cfg(target_os = "macos")]
fn screen_capture_allowed() -> bool {
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
    }
    // SAFETY: a query with no arguments; it never prompts.
    unsafe { CGPreflightScreenCaptureAccess() }
}

#[cfg(not(target_os = "macos"))]
fn screen_capture_allowed() -> bool {
    true
}

/// Runs the scenarios the command line names against `app` and returns the process's exit
/// code: 0 when every scenario passed, 1 when one failed, 2 for bad usage, 130 when
/// interrupted.
pub fn main<A: RunnerApp>(mut app: A) -> ExitCode {
    match real_main(&mut app) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("e2e: {err:#}");
            ExitCode::from(2)
        }
    }
}

fn real_main<A: RunnerApp>(app: &mut A) -> Result<ExitCode> {
    let profile = app.profile();
    let Some(cli) = parse_cli(app, std::env::args().skip(1))? else {
        print!("{}", usage(&profile, app.usage()));
        return Ok(ExitCode::SUCCESS);
    };
    let app = &*app;
    check_extension(app)?;
    process::install_interrupt_handler()?;

    let workspace = workspace_root(Path::new(profile.manifest_dir));
    let target = target_dir(&workspace);
    let absolute = |path: PathBuf| std::path::absolute(&path).unwrap_or(path);
    let mut config = Config {
        app: absolute(
            cli.app
                .unwrap_or_else(|| app::default_app(&profile, &target)),
        ),
        out_root: absolute(cli.out.unwrap_or_else(|| target.join("e2e"))),
        profile,
        workspace,
        target_dir: target,
        display_env: Vec::new(),
        step_timeout: cli.step_timeout,
        ready_timeout: cli.ready_timeout,
        keep_data: cli.keep_data,
        background: !cli.foreground,
        verbose: cli.verbose,
    };
    // Held until the run ends: a second runner on the same output root would wipe this one's
    // scenario dirs.
    let _out_lock = run::lock_out_root(&config.out_root)?;
    // Linux: an Xvfb display of the run's own, ended with it (`display`).
    let display = display::Display::start(cli.host_display, &config.out_root.join("xvfb.log"))?;
    config.display_env = display.env();
    // macOS draws nothing on a sleeping display.
    let _awake = process::DisplayAwake::hold();
    if !screen_capture_allowed() {
        eprintln!(
            "e2e: this terminal has no Screen Recording permission (System Settings > Privacy & \
             Security > Screen Recording): window screenshots still work, `screen` captures and \
             foreground scenarios' captures fail"
        );
    }
    // A background app must never take the foreground: watch for it across every scenario.
    let watch = config.background.then(foreground::Watch::start);

    let paths = scenario::discover(&cli.scenarios)?;
    let mut scenarios = Vec::new();
    for path in paths {
        let scenario = Scenario::load(&path, app)?;
        if cli
            .filter
            .as_deref()
            .is_none_or(|filter| scenario.name.contains(filter))
        {
            scenarios.push(scenario);
        }
    }
    if scenarios.is_empty() {
        bail!("no scenario matches the filter");
    }

    let mut skipped = 0;
    let mut runnable = Vec::new();
    for scenario in &scenarios {
        if !scenario.runs_on(std::env::consts::OS) {
            skipped += 1;
            println!(
                "skip {}: runs on {} only",
                scenario.name,
                scenario.header.platforms.join(", ")
            );
        } else if let Some(name) = scenario.missing_opt_in(|name| std::env::var(name).ok()) {
            skipped += 1;
            println!("skip {}: set {name}=1 to run it", scenario.name);
        } else {
            runnable.push(scenario);
        }
    }
    let jobs = cli.jobs.min(runnable.len()).max(1);
    println!(
        "e2e: {} scenario(s) against {} in the {} on {}{}",
        runnable.len(),
        config.app.display(),
        if config.background {
            "background"
        } else {
            "foreground"
        },
        display.description,
        if jobs > 1 {
            format!(", {jobs} at a time")
        } else {
            String::new()
        }
    );
    /*
     * Each worker takes the next scenario until none is left or the run is interrupted. A
     * scenario prints its start and its result as whole lines, so parallel ones interleave
     * between lines only.
     */
    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| {
                while !process::interrupted() {
                    let Some(scenario) = runnable.get(next.fetch_add(1, Ordering::SeqCst)) else {
                        break;
                    };
                    let outcome = run_one(scenario, &config, app, watch.as_ref());
                    results
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .push((scenario.name.clone(), outcome));
                }
            });
        }
    });
    let results = results
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let passed = results
        .iter()
        .filter(|(_, outcome)| matches!(outcome, Outcome::Passed { .. }))
        .count();
    let interrupted = process::interrupted()
        || results
            .iter()
            .any(|(_, outcome)| matches!(outcome, Outcome::Interrupted));
    let mut failed: Vec<String> = results
        .into_iter()
        .filter(|(_, outcome)| matches!(outcome, Outcome::Failed { .. }))
        .map(|(name, _)| name)
        .collect();
    failed.sort();

    println!(
        "summary: {passed} passed, {} failed{}{}",
        failed.len(),
        if skipped > 0 {
            format!(", {skipped} skipped on {}", std::env::consts::OS)
        } else {
            String::new()
        },
        if interrupted { ", interrupted" } else { "" }
    );
    if !failed.is_empty() {
        println!("failed: {}", failed.join(", "));
    }
    Ok(if interrupted {
        ExitCode::from(130)
    } else if failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Runs `scenario`, printing its start and its result.
fn run_one(
    scenario: &Scenario,
    config: &Config,
    ext: &dyn Extension,
    watch: Option<&foreground::Watch>,
) -> Outcome {
    println!("---- {} ({})", scenario.name, scenario.path.display());
    if config.verbose
        && let Some(description) = &scenario.header.description
    {
        println!("     {description}");
    }
    let started = Instant::now();
    let outcome = run::run(scenario, config, ext, watch);
    let seconds = started.elapsed().as_secs_f32();
    let out_dir = config.out_root.join(&scenario.name);
    match &outcome {
        Outcome::Passed { steps, screenshots } => println!(
            "ok   {}: {steps} steps, {screenshots} screenshots, {seconds:.1}s -> {}",
            scenario.name,
            out_dir.display()
        ),
        Outcome::Failed { line, message } => {
            let location = match line {
                Some(line) => format!("{}:{line}", scenario.path.display()),
                None => scenario.path.display().to_string(),
            };
            println!(
                "FAIL {} ({seconds:.1}s)\n  at {location}\n  {message}\n  logs and screenshots: {}",
                scenario.name,
                out_dir.display()
            );
        }
        Outcome::Interrupted => println!("INTERRUPTED {}", scenario.name),
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::DemoApp;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn parse(list: &[&str]) -> Result<Option<Cli>> {
        parse_cli(&mut DemoApp, args(list))
    }

    #[test]
    fn cli_parses_options_and_scenarios() {
        let cli = parse(&[
            "--app",
            "h.exe",
            "--filter",
            "smoke",
            "--timeout-ms",
            "500",
            "-v",
            "a.jsonl",
            "tests/e2e",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(cli.app, Some(PathBuf::from("h.exe")));
        assert_eq!(cli.filter.as_deref(), Some("smoke"));
        assert_eq!(cli.step_timeout, Duration::from_millis(500));
        assert!(cli.verbose);
        assert!(!cli.foreground, "background by default");
        assert!(!cli.host_display, "Xvfb by default on Linux");
        assert_eq!(cli.jobs, 1, "one at a time by default");
        assert_eq!(parse(&["--jobs", "4", "a.jsonl"]).unwrap().unwrap().jobs, 4);
        assert!(
            parse(&["--foreground", "a.jsonl"])
                .unwrap()
                .unwrap()
                .foreground
        );
        assert_eq!(
            cli.scenarios,
            [PathBuf::from("a.jsonl"), PathBuf::from("tests/e2e")]
        );
    }

    #[test]
    fn cli_rejects_bad_input() {
        assert!(parse(&[]).is_err());
        assert!(parse(&["--bogus", "a.jsonl"]).is_err());
        assert!(parse(&["--app"]).is_err());
        assert!(parse(&["--timeout-ms", "soon", "a.jsonl"]).is_err());
        assert!(parse(&["--jobs", "0", "a.jsonl"]).is_err());
        assert!(parse(&["--jobs", "2", "--foreground", "a.jsonl"]).is_err());
        assert!(parse(&["--help"]).unwrap().is_none());
    }

    #[test]
    fn usage_names_the_app() {
        let text = usage(&DemoApp.profile(), "  --mock <exe>   the demo's mock\n");
        assert!(text.contains("<target>/debug/demo"), "{text}");
        assert!(text.contains("DEMO_DATA_DIR"), "{text}");
        assert!(
            text.ends_with("  --mock <exe>   the demo's mock\n"),
            "{text}"
        );
    }

    #[test]
    fn workspace_root_is_the_nearest_workspace_not_an_enclosing_checkout() {
        let root = tempfile::tempdir().unwrap();
        let checkout = root.path().join("app");
        let worktree = checkout.join(".claude").join("worktrees").join("feature");
        let member = worktree.join("crates").join("app_e2e");
        std::fs::create_dir_all(&member).unwrap();
        let workspace = "[workspace]\nmembers = [\"crates/*\"]\n";
        std::fs::write(checkout.join("Cargo.toml"), workspace).unwrap();
        std::fs::write(worktree.join("Cargo.toml"), workspace).unwrap();
        std::fs::write(member.join("Cargo.toml"), "[package]\nname = \"app_e2e\"\n").unwrap();
        assert_eq!(workspace_root(&member), worktree);
        assert_eq!(workspace_root(&checkout.join("crates").join("x")), checkout);
    }

    #[test]
    fn workspace_root_is_this_workspace() {
        let root = workspace_root(Path::new(env!("CARGO_MANIFEST_DIR")));
        assert!(
            root.join("crates").join("gpui_automation_runner").is_dir(),
            "{}",
            root.display()
        );
    }

    #[test]
    fn app_extensions_may_not_reuse_the_runners_names() {
        assert!(check_extension(&DemoApp).is_ok());

        struct Clashing;
        impl RunnerApp for Clashing {
            type Header = NoHeader;
            type Action = NoAction;
            const ACTIONS: &'static [&'static str] = &["restart"];
            fn profile(&self) -> Profile {
                DemoApp.profile()
            }
        }
        let err = check_extension(&Clashing).unwrap_err().to_string();
        assert_eq!(err, "the app's action \"restart\" is the runner's own");
    }
}
