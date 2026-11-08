# toolbox_process: running other programs

The one way the apps start another program (PLAN §5a). It has no gpui (`script/check` step 3
asserts it), so any crate can use it, CLI and headless builds included.

```toml
toolbox_process = { git = "https://github.com/kylekz/toolbox-rs", tag = "v0.2.0" }
```

## What every process gets

- **It runs as itself.** A program and its arguments, never a shell. A bare name is looked up
  on `PATH` by the OS when it starts; `Override` finds it beforehand (below).
- **No console window on Windows**, whichever way it runs (`CREATE_NO_WINDOW`). The apps run
  detached, so a console child would flash a window.
- **A tree of its own**, which takes everything it starts with it:
  - **Unix:** the process leads its own process group (`process_group(0)`).
  - **Windows:** it is created suspended (`CREATE_SUSPENDED`), put in a Job Object of its own
    with `KILL_ON_JOB_CLOSE`, and only then resumed (`NtResumeProcess`). A child it starts at
    once (PyInstaller's yt-dlp starts Python within ~100 ms) can't escape the job, as it could
    when the job is joined after a plain spawn. The job's handle belongs to the app, so the tree
    also dies with the app, however the app ends.
- **Its leftovers end with it.** When the process exits, whatever it left running in its tree
  is killed. On Unix its exit is seen before it is reaped (`waitid` with `WNOWAIT`), so the
  group's id can't have been reused by then.
- **A stop reaches the whole tree.** `Stop::Graceful(grace)` asks first, then kills what is
  left after `grace`; `Stop::Kill` kills at once.
  - Unix: asking is SIGINT to the group (Ctrl-C, which CLI tools clean up after: yt-dlp exits in
    ~0.05 s, keeping its `.part`); killing is SIGKILL to the group.
  - Windows: a windowless process has no console to send Ctrl-C to, so both end the job at once
    (exit code 1).
- **Back-pressure, not unbounded buffering.** One reader thread per pipe sends into a bounded
  queue (256 chunks or lines); a consumer that falls behind makes the process wait on its pipes.
- **Escaped processes can't hang a run.** A process that leaves its tree on purpose (`setsid`,
  a job breakaway) is out of reach. If it still holds a pipe, the run returns (or the exit is
  delivered) one second (`DRAIN`) after the process exits; its reader thread ends when it lets
  go of the pipe.

## API

| Item | What it is |
|---|---|
| `Command` | Builder: `new(program)`, `arg`, `args`, `env`, `env_remove`, `env_clear`, `current_dir`, `input(bytes)`, `stop(Stop)`; getters for each; `to_std()`. |
| `Stop` | `Graceful(Duration)` (the default, with `DEFAULT_GRACE` = 3 s) or `Kill`. What a timeout or a cancel does in `run`. |
| `run(&command, timeout, &cancel) -> Result<Output, ProcessError>` | Runs to the end and collects stdout and stderr byte for byte. A non-zero exit is not an error: read `Output::status`. |
| `run_lines(&command, timeout, &cancel, on_line) -> Result<ExitStatus, ProcessError>` | The same, handing each line (`Stream`, bytes without `\n` or `\r\n`) to `on_line` on the calling thread as it arrives. |
| `NO_TIMEOUT` | `Duration::MAX`: no deadline; the token can still cancel. |
| `CancelToken` | `new`, `cancel`, `is_cancelled`; clones share it. A token cancelled before `run` starts nothing. |
| `spawn_streaming(&command, sink) -> Result<Handle, ProcessError>` | Starts it with stdin piped. `sink` hears `Event::Stdout(chunk)`, `Event::Stderr(line)`, then `Event::Exit(Option<ExitStatus>)` once, on a thread of the process's own. |
| `Handle` | `pid`, `write(bytes)`, `close_stdin`, `stop(Stop)`. **Dropping it kills the tree**, and once `drop` returns the sink has been dropped and is never called again (unless dropped from inside the sink). |
| `ProcessError` | `Start`, `TimedOut`, `Cancelled`, `Wait`, `Ended` (a write to a closed stdin), `Write`. Each names the program. |
| `tie_to_this_process(&std::process::Child) -> io::Result<()>` | For a child started outside this crate (`Command::to_std`): joins the app's own kill-on-close job on Windows (created on first use, never closed); does nothing elsewhere. Processes from `run` and `spawn_streaming` don't need it. |
| `Override::new(var, name)` | `locate()` / `locate_with(env, cwd)`: the program `$var` names when it is set and not empty, otherwise `name` on `PATH` (through the `which` crate, `PATHEXT` included on Windows). Returns `Located { program, origin: Origin::Var / Origin::Path }`, or `LocateError::BadVar` / `NotFound`. |
| `LINE_MAX` | 1 MiB. A longer line (in `run_lines`, or streaming stderr) arrives in pieces of this size, so a program that never ends a line can't grow memory without bound. |

`Command::input(bytes)`: `run` and `run_lines` write the bytes on a thread of their own and then
close stdin (without input, stdin is empty, never inherited); `spawn_streaming` writes them
before it returns and keeps stdin open.

`run` kills the tree whenever it returns, a panic in `on_line` included, which is a no-op once
the process has exited.

### Threads

`run`: three per call (two readers and the waiter), plus one writer with `input`; the calling
thread consumes. `spawn_streaming`: four (two readers, the waiter, the delivery thread). A
graceful stop adds a short-lived thread that kills after the grace period unless the process
exits first. Threads are named `tbx-proc-*`.

## Deliberately left out

- **Shell-like policy.** Refusing batch files (`.bat`/`.cmd`, which only `cmd.exe` runs),
  requiring absolute paths, checking a working directory against a worktree: each app's own
  rules (dv's `check`).
- **Git policy, `GH_PROMPT_DISABLED`, `DV_GH`, `DV_LSP_<ID>`**: dv's. `Override` covers the
  generic half ("a variable names it, otherwise `PATH`"); `DV_LSP_<ID>` needs an absolute path
  and `_ARGS`, which stay in dv.
- **dv's backend process trait** (`Backend::spawn_stdio`, `ProcessSpec`, `ProcessHandle`):
  needed for processes inside WSL, which run on dv's server. Its local implementation is built
  on this crate.
- **fc's tool discovery** (Scoop, Homebrew, Settings) and all of yt-dlp's arguments,
  classification and progress parsing.
- **Dying with the app on Unix.** Nothing kills a child when the app dies. Linux's
  `PR_SET_PDEATHSIG` fires when the *thread* that spawned the child exits, which a worker
  thread does all the time, so it would kill healthy children.
- **async.** The apps run processes on worker threads; a sink or a channel bridges to an
  executor.
- **stdout and stderr merged, or inherited.** Both are always piped and read. A child that must
  share the app's console (dv's CLI running dv inside a distro) uses `std::process::Command`.
- **Bounding a line's length for display.** `LINE_MAX` only protects memory; dv still cuts a
  stderr line at 8 KiB (`STDERR_LINE_MAX`).

## Adopting it in dv

Add `toolbox_process` to the root `[workspace.dependencies]` beside `gpui_automation`, and to
`dv_core`, `dv_backend`, `dv_cli` and `dv` (for `agent_cli.rs`). About 600–800 lines go.

**What dv gains:**
- The Windows race closes: a language server (or git hook) can't start a child before it is in
  its job. The paragraph about that window in `local.rs`'s module docs goes away.
- A git timeout kills git and everything it started (hooks, `ssh`, credential helpers), not
  only git; what git leaves behind when it exits is killed too.
- One no-console rule instead of five copies, and one "run to the end" loop instead of four.

### `crates/dv_core/src/backend/process/local.rs` (644 lines → about 200)
- `spawn` builds a `toolbox_process::Command` from the checked spec (`program`, `args`,
  `current_dir(cwd)`, `env`/`env_remove` per `spec.env` entry) and calls `spawn_streaming`.
- `check` and `is_batch_file` stay, unchanged, with their test.
- The sink maps events onto dv's `ProcessSink`:
  - `Event::Stdout(bytes)` → `ProcessEvent::Stdout(bytes)`;
  - `Event::Stderr(line)` → `ProcessEvent::Stderr(bounded_line(String::from_utf8_lossy(&line).into_owned()))`;
  - `Event::Exit(status)` → `ProcessEvent::Exit(status.and_then(|status| status.code()))`. A kill
    still reads `None` on Unix and `Some(1)` on Windows.
- `LocalProcess` shrinks to `{ program: String, handle: toolbox_process::Handle }`:
  - `write` → `handle.write(bytes)`, with `ProcessError::Ended` → `ended(&program)` and
    `ProcessError::Write { source, .. }` → `BackendError::Io { op: "writing to", .. }`;
  - `kill` → `handle.stop(Stop::Kill)`;
  - its `Drop` goes: dropping the `Handle` does the same, including the drop from inside the
    sink.
- Goes: `Msg`, `QUEUE`, `DRAIN`, `CHUNK`, `start_thread`, `lock`, `Running`, `State`, `Tree`,
  `own_tree`, `read_stdout`, `read_stderr`, `read_line`, `deliver`, and the `read_line` half of
  `stderr_reads_as_bounded_lines` (keep a `bounded_line` test where `bounded_line` lives).
- **Differs:** a stderr line over 1 MiB now arrives as several lines (each cut at 8 KiB by
  `bounded_line`) where dv read and dropped the rest. `Backend::spawn_stdio`'s contract is
  otherwise unchanged: the conformance tests (`dv_backend/tests/conformance/process.rs`) should
  pass as they are.

### `crates/dv_core/src/job.rs` (226 lines → about 15)
- `tie_to_this_process(child)` becomes a wrapper that logs, keeping its four callers
  (`dv_cli/src/reexec.rs`, `dv_backend/src/launch.rs`, `dv_backend/src/wsl.rs` twice) as they
  are:
  ```rust
  pub fn tie_to_this_process(child: &std::process::Child) {
      if let Err(error) = toolbox_process::tie_to_this_process(child) {
          log::warn!("couldn't tie process {} to dv's job object ({error}); it relies on its stdin closing", child.id());
      }
  }
  ```
- `Job` (one process's own job) and the `windows` module go: the crate's trees replace them.
  Its Windows tests are covered by toolbox's (`src/windows_tests.rs`).
- `dv_core/Cargo.toml`: `libc` and `windows-sys` go if nothing else in `dv_core` uses them (today
  only `local.rs` and `job.rs` do).

### `crates/dv_core/src/git/command.rs` (295 lines → about 200)
- `command_with_index` returns a `toolbox_process::Command`: the overlay, then `env_remove` for
  each `DROPPED_ENV`, `env` for each `POLICY_ENV`, `GIT_INDEX_FILE`, `--no-pager` and the
  `-c` pairs. The `CREATE_NO_WINDOW` block goes.
- `run_with_index` becomes:
  ```rust
  let command = command_with_index(cwd, overlay, index_file)
      .args(args)
      .stop(Stop::Kill);
  let command = match &opts.stdin { Some(bytes) => command.input(bytes.clone()), None => command };
  let timeout = opts.timeout_ms.map_or(NO_TIMEOUT, Duration::from_millis);
  match toolbox_process::run(&command, timeout, &CancelToken::new()) { … }
  ```
  mapping `ProcessError::Start { source, .. }` → `BackendError::Spawn`, `TimedOut` →
  `BackendError::Timeout`, `Wait { source, .. }` → `BackendError::io("waiting for git in", …)`;
  the `ok_codes` check and the debug log stay.
- Goes: `DRAIN_GRACE`, `read_in_background`, `wait_until`, the stdin writer thread.
- `command()` and `apply_overlay` stay for the tests that build a `std::process::Command`
  (`backend/tests.rs`, `review/tests/concurrency.rs`); `command()` can return
  `command_with_index(…).to_std()`.
- **Differs:** when git has exited but a process it started escaped its group and holds the
  pipes, the call now returns git's output one second after the exit, where dv waited until
  the deadline and then failed with `Timeout`. A process still in git's group is killed when
  git exits, which closes the pipes at once.

### `crates/dv_cli/src/checks/process.rs` (89 lines → about 30), `checks/git.rs`, `checks/gh.rs`
- `run(command: toolbox_process::Command, timeout) -> Result<Ran, ProcessError>` calls
  `toolbox_process::run(&command.stop(Stop::Kill), timeout, &CancelToken::new())` and turns
  `Output` into `Ran` (`code`, lossy `stdout` and `stderr`).
- `apply_env` folds the `(name, Option<value>)` pairs into `env`/`env_remove`; `no_window`, the
  reader threads, the polling loop and the `tie_to_this_process` call go (the tree replaces the
  tie).
- `git.rs` (`read`) and `gh.rs` (`check`) build `toolbox_process::Command::new("git")` /
  `Command::new(&seams.gh)` and match `ProcessError::Start { source, .. } if source.kind() ==
  NotFound` where they matched `error.kind() == NotFound`.
- `gh.rs` may use `Override::new(GH_VAR, "gh").locate_with(|name| seams.var(name).map(Into::into), cwd)`
  to say where `gh` was found, or that neither `DV_GH` nor `PATH` has it, before running it. The
  `GH_VAR` constant and `Seams::gh` stay.

### `crates/dv_backend/src/launch.rs`, `crates/dv_backend/src/wsl.rs`
- `launch.rs` `spawn`: build `toolbox_process::Command::new(program).args(args)` plus the env
  pairs, then `.to_std()`, set the three pipes and spawn as today. Its pipes go to the
  `Transport`, so it keeps `std::process::Child` and the `tie_to_this_process` call.
- `launch.rs`'s `no_window` (both `cfg` versions) goes; `wsl.rs`'s `wsl_exe()` becomes
  `toolbox_process::Command::new(WSL_EXE).to_std()`, and `wsl/tests.rs`'s source check for
  `no_window(&mut command);` checks for `.to_std()` instead.
- `wsl.rs`'s own `run(command, input, timeout)` (another run-to-the-end loop) can move onto
  `toolbox_process::run` once `Shell::command` returns a `toolbox_process::Command`; that is
  optional, and `console_command` stays a `std::process::Command` (it shares the console on
  purpose).

### `crates/dv/src/automation/agent_cli.rs`
- `Running::start`: `toolbox_process::Command::new(exe)…to_std()` in place of
  `std::process::Command::new(exe)` and the `CREATE_NO_WINDOW` block (its stdin, stdout and
  stderr stay as they are).

### Unchanged
- `dv_lsp`: it starts servers through `Backend::spawn_stdio`, so it gains the race fix without
  a change. `DV_LSP_<ID>` (`settings::Override`) stays.
- `dv/src/launch.rs` (the detached app: `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`, `setsid`):
  a process meant to outlive the CLI, the opposite of a tree.

## Adopting it in fc

Add `toolbox_process` to the root `[workspace.dependencies]` beside `gpui_automation`, and to
`fc_tools` and `fc_ui`. About 450 lines go. The crate's API is fc's, so the move is mostly
renames.

**What fc gains:** the same guarantees it has now, plus: a panic in a `run_lines` handler kills
the tree; a token cancelled beforehand starts nothing; `input` for stdin; a bound on a runaway
line; streaming, if fc ever needs it. Its `process.rs` tests run in toolbox-rs's CI on all three
OSes.

### `crates/fc_tools/src/process.rs` (594 lines): deleted
- `fc_tools/src/lib.rs`: drop `pub mod process;`.
- `fc_tools/Cargo.toml`: drop `libc` and `windows-sys` from `[dependencies]` (only `process.rs`
  used them); keep `libc` as a Unix dev-dependency if `tests/process.rs` stays.
- Renames at every use: `fc_tools::process::` → `toolbox_process::`, `ToolCommand` →
  `Command`, `process::KILL_GRACE` → `toolbox_process::DEFAULT_GRACE` (both 3 s). `run`,
  `run_lines`, `CancelToken`, `Output` (with `stderr_text`), `Stream` and `ProcessError`'s
  four variants keep their names, shapes and messages. The files:
  - `fc_tools/src/install.rs`, `version.rs`, `ffmpeg.rs`, `ffmpeg/tests.rs`, `partial.rs`,
    `test_support.rs`, `ytdlp/args.rs`, `ytdlp/progress.rs`;
  - `fc_tools/tests/backfill.rs`, `offline_format.rs`, `process.rs`;
  - `fc_ui/src/services/backfill.rs`, `downloads/paste.rs`, `downloads/mod.rs`,
    `downloads/worker.rs`, `tools.rs`, `thumbs/mod.rs`, `library/probe.rs`.
- `ProcessError` gains `Ended` and `Write` (from streaming only). fc's matches all end in a catch-all
  arm today, so none needs a change.
- `fc_tools/tests/process.rs` (`mock_tool`-based): toolbox-rs's `tests/process.rs` covers the
  same ground (pipes, UTF-8, environment, missing program, whole-tree timeout, leftovers,
  SIGINT, SIGKILL after the grace). Keep fc's as an end-to-end check against `mock_tool`, or
  delete it; with it kept, its `KILL_GRACE` becomes `DEFAULT_GRACE`.

### Unchanged
- yt-dlp's environment policy (`PATH`, `PYTHONUTF8`, `TEMP`) in `ytdlp/args.rs`, tool discovery
  in `discover.rs`, and everything that parses yt-dlp's output.
- `fc_e2e`: the runner's sidecar and the orphan check have their own processes.

## slide

Nothing: it only runs `chflags`.

## Tests

`crates/toolbox_process/tests/process.rs` runs the crate against its own test program
(`tests/support/child.rs`, a `[[bin]]` that apps never build), on macOS, Linux and Windows in
CI: pipes byte for byte and line by line, more output than a pipe holds, input, the environment
and working directory, a missing program, cancels before and during, timeouts, whole-tree kills
(a grandchild holding the pipes), leftovers on exit, a panicking line handler, streaming (writes,
`close_stdin`, a write after the exit, a graceful stop, a kill, the drop), `tie_to_this_process`
and `Override`. Unix adds SIGINT handling, SIGKILL after an unanswered SIGINT, and an escaped
process. `src/windows_tests.rs` checks from inside that a process is in its own job, a tied
child is in the app's job, and a job's end exits with code 1.
