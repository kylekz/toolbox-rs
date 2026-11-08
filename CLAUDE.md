# toolbox-rs

Shared crates for the owner's gpui desktop apps (dv, slide, fc). Every crate here replaces copies
that used to live in each app, so a change here reaches all of them: keep the surface small, and
keep what is one app's business in that app (its fixtures, its commands, its values).

- Spec: `docs/PLAN.md` (decisions O1–O6, the crates, the phases; cited as `PLAN §N` / `O<n>`) and
  `docs/automation.md` (the automation protocol, the scenario format, the runner). Read the
  relevant sections before changing anything; when code and spec disagree, fix one of them in the
  same change.
- The apps consume tags (`tag = "v0.1.0"`), never branches. Tags are the owner's: no agent pushes
  one or moves one.

## Toolchain

- Rust 1.98.1 (pinned in `rust-toolchain.toml`), edition 2024, resolver 3.
- The gpui stack is pinned exactly in the root `[workspace.dependencies]`: `gpui-pre =0.3.11`,
  `gpui-pre-platform =0.3.11`, `gpui-component =0.9.0`. toolbox-rs owns this pin for every app
  that depends on it (PLAN O5); bump the family together, as a new minor version.
- Don't guess gpui APIs: grep the sources under
  `~/.cargo/registry/src/index.crates.io-*/gpui-pre-0.3.11/` and `gpui-component-0.9.0/`.

## Crates (`crates/`)

| Crate | Role | gpui? |
|---|---|---|
| `gpui_automation` | the in-app driver (`enabled` feature) or its no-op stand-ins (without) | yes |
| `gpui_automation_protocol` | wire format, matcher, builtin commands' arguments, selectors | **never** |
| `gpui_automation_runner` | the scenario runner as a library: `RunnerApp`, `Fixture`, `main` | **never** |
| `automation_harness` | dev-only: the harness app and its runner, the crates' end-to-end proof | yes |

A gpui dependency in a "never" crate fails `script/check` (`cargo tree`): the runner must build
without gpui. Crates that need gpui are named `gpui_*`; crates that don't are `toolbox_*` or
`gpui_automation_*` for the automation family.

## Rules

- **Dependencies.** Declare every dependency once in the root `[workspace.dependencies]` (keep it
  alphabetical) and use `foo.workspace = true` in crates.
- **Licence.** Every crate is MIT and carries its own copy of the root `LICENSE` (an app's
  third-party notices read it from the crate folder); a new crate copies it too. Nothing derived from GPL code (Zed's `sqlez`, `open_router`,
  editor, project, …) comes here; gpui and gpui-component are Apache-2.0 and fine to build on.
- **Errors.** `anyhow` in the runner and the driver's command handling, `thiserror` enums in
  libraries with typed errors. No `unwrap()`/`expect()` outside tests unless the message states
  the invariant.
- **Compatibility.** The apps' scenario files must keep parsing (PLAN O4). A new runner key or
  action is a new reserved name: check `scenario::HEADER_KEYS`, `RUNNER_KEYS` and `STEP_KEYS`
  against the apps' extensions (dv: `gh`, `lsp`, `gh_log`, `lsp_log`; fc: `mock`, `mock_args`,
  `tools`, `tool_log`, `tool_release`) before adding one.
- **Style.** rustfmt defaults; clippy clean with `-D warnings` in both `gpui_automation` shapes
  (with and without `enabled`). Files stay under ~1,500 lines, split by responsibility.
- **Comments** say what something is and why, in the present tense, citing `PLAN §N` or
  `docs/automation.md` sections. History belongs in commit messages, never in comments. Use
  `/** */` for multi-line comments outside doc comments.
- **`script/check` before every commit.**

## GUI runs

The harness opens windows on the real desktop.

- Run it only through its runner (background mode, the default: windows never take the focus) or
  `cargo test -p automation_harness --test harness_e2e -- --ignored`. Never leave `harness`
  running; if a run fails, kill what you started by its pid, never by name.
- Linux GUI runs go on the runner's own Xvfb display, never the session's (WSLg's windows open on
  the owner's Windows desktop); `--host-display` only when the owner asks for a look.
- Verify by `state` assertions; screenshots are physical pixels.

## Commands

```bash
script/check                 # fmt, clippy x2, gpui boundary, tests, harness e2e
script/check --fast          # the same without the harness e2e
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p gpui_automation --all-targets -- -D warnings      # the stand-ins alone
cargo build -p automation_harness && target/debug/harness-e2e crates/automation_harness/tests/harness*.jsonl
```
