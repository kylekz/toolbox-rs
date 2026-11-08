use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

fn parse(text: &str) -> Result<Scenario> {
    Scenario::parse(
        "demo",
        Path::new("demo.jsonl"),
        text,
        &crate::testing::DemoApp,
    )
}

fn expectations(raw: &Option<Value>) -> Vec<Expectation> {
    Step::expectations(raw, &[]).unwrap()
}

#[test]
fn parses_header_comments_and_steps() {
    let scenario = parse(
        r#"
// a comment
{"scenario": {"window_size": "1080x1920", "app_args": ["--x"], "env": {"A": "1"}}}
# another comment
{"cmd": "wait_ready"}
{"cmd": "click", "target": "a", "expect": {"pointer": "/n", "equals": 1}}
{"expect": [{"pointer": "/n", "gte": 1}, {"pointer": "/m", "exists": false}]}
"#,
    )
    .unwrap();
    assert_eq!(scenario.window_size(), "1080x1920");
    assert_eq!(scenario.header.app_args, ["--x"]);
    assert_eq!(scenario.header.env.get("A").map(String::as_str), Some("1"));
    assert_eq!(scenario.steps.len(), 3);
    assert_eq!(scenario.steps[0].line, 5);
    assert_eq!(scenario.steps[0].cmd(), Some("wait_ready"));
    assert_eq!(expectations(&scenario.steps[1].expect).len(), 1);
    assert_eq!(
        Value::Object(scenario.steps[1].request().cloned().unwrap()),
        json!({"cmd": "click", "target": "a"}),
        "runner keys are stripped from the request"
    );
    assert_eq!(scenario.steps[2].action, Action::Check);
    assert_eq!(expectations(&scenario.steps[2].expect).len(), 2);
}

#[test]
fn save_takes_names_and_pointers() {
    let scenario =
        parse(r#"{"cmd": "state", "save": {"review": "/reviews/0/id", "all": ""}}"#).unwrap();
    let mut save = scenario.steps[0].save.clone();
    save.sort();
    assert_eq!(
        save,
        [
            ("all".to_string(), String::new()),
            ("review".to_string(), "/reviews/0/id".to_string())
        ]
    );
    assert_eq!(
        scenario.steps[0].request().unwrap().get("save"),
        None,
        "never sent to the app"
    );
    for (line, error) in [
        (r#"{"cmd": "state", "save": ["x"]}"#, "must map"),
        (
            r#"{"cmd": "state", "save": {"a-b": "/x"}}"#,
            "not a variable name",
        ),
        (
            r#"{"cmd": "state", "save": {"data_dir": "/x"}}"#,
            "runner's own",
        ),
        (
            r#"{"cmd": "state", "save": {"workspace": "/x"}}"#,
            "runner's own",
        ),
        (
            r#"{"cmd": "state", "save": {"x": "x"}}"#,
            "needs a JSON pointer",
        ),
        (
            r#"{"cmd": "fail", "expect_error": "x", "save": {"x": "/x"}}"#,
            "needs a result",
        ),
        (
            r#"{"expect": {"pointer": "", "exists": true}, "save": {"x": "/x"}}"#,
            "need a cmd",
        ),
    ] {
        let err = format!("{:#}", parse(line).unwrap_err());
        assert!(err.contains(error), "{line}: {err}");
    }
}

#[test]
fn headers_can_ask_for_the_foreground() {
    let scenario = parse("{\"scenario\": {\"foreground\": true}}\n{\"cmd\": \"state\"}").unwrap();
    assert!(scenario.header.foreground);
    let scenario = parse("{\"cmd\": \"state\"}").unwrap();
    assert!(!scenario.header.foreground);
}

#[test]
fn opt_in_runs_a_scenario_only_when_its_variable_is_1() {
    let scenario =
        parse("{\"scenario\": {\"opt_in\": \"DEMO_PLAYER_REAL\"}}\n{\"cmd\": \"state\"}").unwrap();
    assert_eq!(scenario.missing_opt_in(|_| None), Some("DEMO_PLAYER_REAL"));
    assert_eq!(
        scenario.missing_opt_in(|_| Some("0".into())),
        Some("DEMO_PLAYER_REAL")
    );
    assert_eq!(scenario.missing_opt_in(|_| Some("1".into())), None);
    assert_eq!(
        parse(r#"{"cmd": "state"}"#)
            .unwrap()
            .missing_opt_in(|_| None),
        None
    );
    for name in ["PLAYER_REAL", "FC_PLAYER_REAL", "DEMO_DATA_DIR"] {
        let header =
            format!("{{\"scenario\": {{\"opt_in\": \"{name}\"}}}}\n{{\"cmd\": \"state\"}}");
        assert!(parse(&header).is_err(), "{name}");
    }
}

#[test]
fn platforms_limit_where_a_scenario_runs() {
    let everywhere = parse(r#"{"cmd": "state"}"#).unwrap();
    assert!(everywhere.runs_on("windows") && everywhere.runs_on("macos"));
    let windows =
        parse("{\"scenario\": {\"platforms\": [\"windows\"]}}\n{\"cmd\": \"state\"}").unwrap();
    assert!(windows.runs_on("windows"));
    assert!(!windows.runs_on("macos"));
}

#[test]
fn platforms_limit_where_a_line_runs() {
    let scenario =
        parse("{\"cmd\": \"state\", \"platforms\": [\"macos\"]}\n{\"cmd\": \"settle\"}").unwrap();
    assert!(scenario.steps[0].runs_on("macos"));
    assert!(!scenario.steps[0].runs_on("windows"));
    assert!(scenario.steps[1].runs_on("windows"));
    // The runner's key: never sent to the app.
    assert!(
        scenario.steps[0]
            .request()
            .unwrap()
            .get("platforms")
            .is_none()
    );
    let err = parse(r#"{"cmd": "state", "platforms": ["darwin"]}"#).unwrap_err();
    assert!(
        format!("{err:#}").contains("unknown platform \"darwin\""),
        "{err:#}"
    );
    let err = parse(r#"{"cmd": "state", "platforms": "macos"}"#).unwrap_err();
    assert!(format!("{err:#}").contains("must be a list"), "{err:#}");
}

#[test]
fn header_defaults_and_validation() {
    let scenario = parse(r#"{"cmd": "state"}"#).unwrap();
    assert_eq!(scenario.header, Header::default());
    assert_eq!(scenario.window_size(), "1440x900");

    let err = parse("{\"cmd\": \"state\"}\n{\"scenario\": {}}").unwrap_err();
    assert!(
        format!("{err:#}").contains("must be the first line"),
        "{err:#}"
    );
    let err = parse("{\"scenario\": {\"gh\": \"x.json\"}}\n{\"cmd\": \"state\"}").unwrap_err();
    assert!(
        format!("{err:#}").contains("bad scenario header"),
        "{err:#}"
    );
    let err =
        parse("{\"scenario\": {\"platforms\": [\"win32\"]}}\n{\"cmd\": \"state\"}").unwrap_err();
    assert!(
        format!("{err:#}").contains("unknown platform \"win32\""),
        "{err:#}"
    );
    let err =
        parse("{\"scenario\": {\"window_size\": \"big\"}}\n{\"cmd\": \"state\"}").unwrap_err();
    assert!(format!("{err:#}").contains("WIDTHxHEIGHT"), "{err:#}");
}

#[test]
fn errors_name_the_line() {
    let cases = [
        ("{\"cmd\": \"state\"}\nnot json", "line 2: invalid JSON"),
        ("[1]", "line 1: expected a JSON object"),
        ("{\"x\": 1}", "line 1: missing \"cmd\""),
        ("{\"cmd\": \"state\", \"id\": 4}", "don't set \"id\""),
        (
            "{\"cmd\": \"state\", \"expect\": {\"pointer\": \"/a\"}}",
            "has no check",
        ),
        (
            "{\"expect_error\": \"x\", \"expect\": {\"pointer\": \"/a\", \"exists\": true}}",
            "need a cmd",
        ),
        ("{}", "a line needs a cmd"),
        ("", "no steps"),
    ];
    for (text, message) in cases {
        let err = format!("{:#}", parse(text).unwrap_err());
        assert!(err.contains(message), "{text:?}: {err}");
    }
}

#[test]
fn screenshots_are_named_by_the_runner() {
    let scenario = parse(
        r#"{"cmd": "screenshot", "name": "empty shell/1"}
{"cmd": "screenshot", "path": "x.png"}"#,
    )
    .unwrap();
    assert_eq!(
        scenario.steps[0].screenshot.as_deref(),
        Some("empty-shell-1")
    );
    assert!(scenario.steps[0].request().unwrap().get("name").is_none());
    assert_eq!(scenario.steps[1].screenshot, None);
    for text in [
        r#"{"cmd": "screenshot"}"#,
        r#"{"cmd": "screenshot", "name": "a", "path": "b"}"#,
        r#"{"cmd": "screenshot", "name": "///"}"#,
    ] {
        assert!(parse(text).is_err(), "{text}");
    }
    // `name` means something else on other commands and is left alone.
    let scenario = parse(r#"{"cmd": "window", "name": "second"}"#).unwrap();
    assert_eq!(
        scenario.steps[0].request().unwrap().get("name"),
        Some(&json!("second"))
    );
}

#[test]
fn reply_timeouts_cover_the_command_own_wait() {
    let base = Duration::from_secs(30);
    let scenario = parse(
        r#"{"cmd": "wait", "ms": 5000}
{"cmd": "wait_for", "pointer": "/a", "exists": true, "timeout_ms": 20000}
{"cmd": "drag", "from": [1, 1], "to": [9, 9], "steps": 10, "step_ms": 100, "hold_ms": 600}
{"cmd": "state", "runner_timeout_ms": 1234}
{"expect": {"pointer": "/a", "exists": true}}"#,
    )
    .unwrap();
    let timeouts: Vec<u128> = scenario
        .steps
        .iter()
        .map(|step| step.reply_timeout(base).as_millis())
        .collect();
    assert_eq!(timeouts, [35_000, 50_000, 31_600, 1234, 30_000]);
}

#[test]
fn expect_error_and_expect_result_parse() {
    let scenario = parse(
        r#"{"cmd": "click", "target": "nope", "expect_error": "no target"}
{"cmd": "echo", "a": 1, "expect_result": {"pointer": "/a", "equals": 1}}"#,
    )
    .unwrap();
    assert_eq!(scenario.steps[0].expect_error.as_deref(), Some("no target"));
    assert_eq!(expectations(&scenario.steps[1].expect_result).len(), 1);
    assert!(
        parse(r#"{"cmd": "x", "expect_error": 3}"#).is_err(),
        "expect_error must be text"
    );
}

#[test]
fn expectations_are_substituted_per_run() {
    let scenario =
        parse(r#"{"cmd": "state", "expect": {"pointer": "/dir", "equals": "${data_dir}"}}"#)
            .unwrap();
    let parsed =
        Step::expectations(&scenario.steps[0].expect, &[("data_dir", "C:/x".into())]).unwrap();
    assert!(parsed[0].evaluate(&json!({"dir": "C:/x"})).is_ok());
}

#[test]
fn substitution_reaches_nested_strings() {
    let mut value = json!({"paths": ["${data_dir}/a.png"], "note": "${out_dir}", "n": 1});
    substitute(
        &mut value,
        &[("data_dir", "C:/tmp".into()), ("out_dir", "out".into())],
    );
    assert_eq!(
        value,
        json!({"paths": ["C:/tmp/a.png"], "note": "out", "n": 1})
    );
}

#[test]
fn restarts_parse() {
    let scenario = parse(
        r#"{"cmd": "state"}
{"restart": {}}
{"restart": {"app_args": ["--counter", "7"]}, "expect": {"pointer": "/counter", "equals": 7}, "expect_result": {"pointer": "/event", "equals": "ready"}}
{"restart": {"data_dir": false}}"#,
    )
    .unwrap();
    assert_eq!(
        scenario.steps[1].action,
        Action::Restart(Restart::default())
    );
    assert_eq!(
        scenario.steps[2].action,
        Action::Restart(Restart {
            app_args: Some(vec!["--counter".into(), "7".into()]),
            data_dir: None,
        })
    );
    assert!(scenario.steps[2].expect.is_some());
    assert_eq!(
        scenario.steps[3].action,
        Action::Restart(Restart {
            app_args: None,
            data_dir: Some(false),
        })
    );
    for (text, message) in [
        (r#"{"restart": {"args": []}}"#, "bad restart"),
        (r#"{"restart": true}"#, "bad restart"),
        (
            r#"{"restart": {}, "expect_error": "x"}"#,
            "can't expect an error",
        ),
        (
            r#"{"restart": {}, "runner_timeout_ms": 5}"#,
            "--ready-timeout-ms",
        ),
        (r#"{"restart": {}, "cmd": "quit"}"#, "exclude each other"),
    ] {
        let err = format!("{:#}", parse(text).unwrap_err());
        assert!(err.contains(message), "{text}: {err}");
    }
}

#[test]
fn only_restarts_follow_quit() {
    let ok = parse(
        r#"{"cmd": "quit"}
{"restart": {}}
{"cmd": "state"}
{"cmd": "quit"}
{"restart": {}}"#,
    );
    assert!(ok.is_ok(), "{:#}", ok.unwrap_err());
    for (text, line) in [
        ("{\"cmd\": \"quit\"}\n{\"cmd\": \"state\"}", 2),
        (
            "{\"cmd\": \"quit\"}\n{\"expect\": {\"pointer\": \"/a\", \"exists\": true}}",
            2,
        ),
    ] {
        let err = format!("{:#}", parse(text).unwrap_err());
        assert!(
            err.contains(&format!("line {line}: after quit only a restart")),
            "{text}: {err}"
        );
    }
    let err = format!(
        "{:#}",
        parse(r#"{"cmd": "quit", "expect": {"pointer": "/a", "exists": true}}"#).unwrap_err()
    );
    assert!(err.contains("no state to expect"), "{err}");
}

#[test]
fn runner_owned_env_is_rejected() {
    for name in [
        "DEMO_DATA_DIR",
        "demo_data_dir",
        "E2E_DATA_DIR",
        "E2E_OUT_DIR",
    ] {
        let text =
            format!("{{\"scenario\": {{\"env\": {{\"{name}\": \"x\"}}}}}}\n{{\"cmd\": \"state\"}}");
        let err = format!("{:#}", parse(&text).unwrap_err());
        assert!(err.contains("the runner sets it"), "{name}: {err}");
    }
    let scenario = parse(
        "{\"scenario\": {\"env\": {\"RUST_LOG\": \"debug\", \"DEMO_PLAYER_REAL\": \"1\"}}}\n{\"cmd\": \"state\"}",
    )
    .unwrap();
    assert_eq!(scenario.header.env["RUST_LOG"], "debug");
}

#[test]
fn wildcards_match_file_names() {
    assert!(wildcard_match("*.jsonl", "smoke.jsonl"));
    assert!(wildcard_match("SMOKE.JSONL", "smoke.jsonl"));
    assert!(wildcard_match("shell_*.jsonl", "shell_layout.jsonl"));
    assert!(wildcard_match("s?oke.*", "smoke.jsonl"));
    assert!(!wildcard_match("*.jsonl", "smoke.json"));
    assert!(!wildcard_match("shell_*", "review.jsonl"));
    assert!(wildcard_match("*", ""));
}

#[test]
fn discover_expands_directories_and_wildcards() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["b.jsonl", "a.jsonl", "notes.md"] {
        std::fs::write(dir.path().join(name), "{}").unwrap();
    }
    let names = |paths: Vec<PathBuf>| -> Vec<String> {
        paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    };
    assert_eq!(
        names(discover(&[dir.path().to_path_buf()]).unwrap()),
        ["a.jsonl", "b.jsonl"]
    );
    assert_eq!(
        names(discover(&[dir.path().join("b*")]).unwrap()),
        ["b.jsonl"]
    );
    assert!(discover(&[dir.path().join("z*")]).is_err());
    assert!(discover(&[dir.path().join("missing.jsonl")]).is_err());
}

#[test]
fn app_header_keys_go_to_the_app_and_unknown_ones_fail() {
    let scenario =
        parse("{\"scenario\": {\"mock\": \"fixtures/a.json\", \"window_size\": \"800x600\"}}\n{\"cmd\": \"state\"}")
            .unwrap();
    assert_eq!(
        scenario.header.app,
        *json!({"mock": "fixtures/a.json"}).as_object().unwrap()
    );
    assert_eq!(scenario.window_size(), "800x600");

    let err = format!(
        "{:#}",
        parse("{\"scenario\": {\"mokc\": \"a.json\"}}\n{\"cmd\": \"state\"}").unwrap_err()
    );
    assert!(err.contains("unknown key \"mokc\""), "{err}");
    assert!(err.contains("app_args") && err.contains("mock"), "{err}");

    let err = format!(
        "{:#}",
        parse("{\"scenario\": {\"mock\": 3}}\n{\"cmd\": \"state\"}").unwrap_err()
    );
    assert!(err.contains("bad scenario header"), "{err}");
}

#[test]
fn app_actions_are_runner_steps() {
    let scenario = parse(
        "{\"mock_log\": {\"wait_ms\": 500}, \"expect_result\": {\"pointer\": \"/calls\", \"len\": 0}}",
    )
    .unwrap();
    assert_eq!(
        scenario.steps[0].action,
        Action::App(json!({"mock_log": {"wait_ms": 500}}))
    );
    assert!(scenario.steps[0].expect_result.is_some());

    for (line, error) in [
        ("{\"mock_log\": {\"wiat_ms\": 1}}", "bad mock_log"),
        (
            "{\"mock_log\": {}, \"cmd\": \"state\"}",
            "takes no command arguments",
        ),
        (
            "{\"mock_log\": {}, \"expect_error\": \"x\"}",
            "can't expect an error",
        ),
    ] {
        let err = format!("{:#}", parse(line).unwrap_err());
        assert!(err.contains(error), "{line}: {err}");
    }
}

#[test]
fn runner_actions_may_follow_a_quit() {
    assert!(parse("{\"cmd\": \"quit\"}\n{\"mock_log\": {}}").is_ok());
    assert!(parse("{\"cmd\": \"quit\"}\n{\"mock_log\": {}, \"expect\": {\"pointer\": \"/a\", \"exists\": true}}").is_err());
    assert!(parse("{\"cmd\": \"quit\"}\n{\"cmd\": \"state\"}").is_err());
}

#[test]
fn the_apps_variables_and_env_are_reserved() {
    let err = format!(
        "{:#}",
        parse("{\"cmd\": \"state\", \"save\": {\"demo_mock_log\": \"/a\"}}").unwrap_err()
    );
    assert!(err.contains("the runner's own variable"), "{err}");
    let err = format!(
        "{:#}",
        parse("{\"scenario\": {\"env\": {\"DEMO_MOCK\": \"x\"}}}\n{\"cmd\": \"state\"}")
            .unwrap_err()
    );
    assert!(err.contains("the runner sets it"), "{err}");
}

#[test]
fn the_app_checks_the_headers_env() {
    let err = format!(
        "{:#}",
        parse("{\"scenario\": {\"env\": {\"DEMO_LIVE_X\": \"1\"}}}\n{\"cmd\": \"state\"}")
            .unwrap_err()
    );
    assert!(err.contains("can't set DEMO_LIVE_X"), "{err}");
    parse("{\"scenario\": {\"env\": {\"DEMO_KEY\": \"x\"}}}\n{\"cmd\": \"state\"}").unwrap();
}
