# Automation and end-to-end scenarios

Three crates drive a gpui app from outside, for tests and agents:

- **`gpui_automation`** (gpui): the in-app driver. An app started with `--automation` serves a
  JSONL protocol on stdin/stdout (or a Windows named pipe): input, windows, screenshots, named
  targets, waits on the real frame loop, and the app's own `state` and commands. Without its
  `enabled` feature the crate is only no-op stand-ins, so views call it unconditionally and a
  shipped build carries none of the driver.
- **`gpui_automation_protocol`** (no gpui): the wire format, the JSON-pointer expectations and
  the builtin commands' arguments, shared by the driver and the runner.
- **`gpui_automation_runner`** (no gpui): the scenario runner as a library. Each app has a small
  `e2e` bin that hands it a `RunnerApp`: the app's executable, its environment, and its own
  scenario extensions (Runner extensions, below).

A scenario is a `*.jsonl` file: automation commands run against a freshly started app, with
assertions on the app's `state` JSON.

```bash
cargo build -p <app>
cargo run -p <app> --bin e2e -- tests/e2e
cargo run -p <app> --bin e2e -- --filter empty tests/e2e/*.jsonl
```

Build the app with a plain `cargo build -p <app>` right before a run. A build that compiles the
app's bin alongside test targets with unified features (`cargo build --workspace --all-targets`,
or `cargo test`) can relink it with gpui's `test-support`, which includes leak detection: that
binary panics at exit when an entity outlives the App, so a scenario can exit 101 after `quit`
against it and pass against a plain one.

The crates' own proof is `crates/automation_harness`: a small gpui-component app and its runner.
`harness.jsonl` covers every command, restarts, selectors, the handshake and the runner's
hermetic environment; `harness_background.jsonl` (background runs only) a window asked for
maximized bounds opening windowed and inactive; and a second test drives the harness over the
named pipe (Windows):

```bash
cargo test -p automation_harness --test harness_e2e -- --ignored
# or by hand:
cargo build -p automation_harness
target/debug/harness-e2e crates/automation_harness/tests/harness*.jsonl
```

Windows open on the real desktop, but by default in the background (next section), so runs can
go on while someone uses the machine. Apps that render `gpui_automation::pointer_guard()` drop
the real mouse's events while automation runs, so a cursor crossing the window can't bend a
scripted drag. Keyboard input is not filtered, and the real cursor still moves gpui's hover
position until the next scripted pointer event, so it is still best to leave the windows alone.

On Linux the runner starts an Xvfb display of its own for the run and starts every app on it
under X11 alone, so no window reaches the desktop (WSLg's, under WSL); `--host-display` opens on
the session's display instead, for an explicit exploratory look.

### Background runs

The runner starts the app with `--automation-background` unless it is given
`--foreground`. An app that supports the flag  calls
`gpui_automation::set_background(true)` and opens **every** window through
`gpui_automation::open_window` , which
in background mode:

- opens the window without activating it (`WindowOptions::focus` off), so it
  never takes the keyboard focus or the foreground;
- on Windows, moves it to the bottom of the z-order without activating it
  (`SetWindowPos(HWND_BOTTOM, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE)`)
  before its first frame, and makes it a tool window that can't be activated
  (`WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`) without a system menu (with
  `WS_SYSMENU`, DWM draws a tool window's small caption "x" at its top-right,
  which shows through the title bar). It has no taskbar button,
  isn't in Alt+Tab, and Windows never hands it the foreground when the person
  minimises or closes their own window. The window is also subclassed to
  answer `WM_MOUSEACTIVATE` with `MA_NOACTIVATE`, because gpui itself answers
  `MA_ACTIVATE`, so a real click doesn't activate it either. It stays on
  screen, behind everything else: DirectComposition keeps rendering an
  occluded window and `PrintWindow(PW_RENDERFULLCONTENT)` still captures it,
  which a minimised or off-screen window wouldn't allow;
- opens a window asked for `WindowBounds::Maximized` windowed at its restore
  bounds (maximizing activates the window on Windows);
- renders at full rate although it is inactive (gpui throttles inactive
  windows to ~30 fps otherwise, which would slow every step), and in Low
  Power Mode or under thermal pressure (gpui's 30 and 60 fps caps).

The app must not activate or raise anything itself in this mode:
`App::activate` and `Window::activate_window` (gpui's Windows
`activate_window` even injects a synthetic Alt press), `Window::resize` (its
`SetWindowPos` lacks `SWP_NOACTIVATE`/`SWP_NOZORDER`, so it raises and
activates the window; open windows at the size they need, and scenarios use
the `resize` command), and `Window::zoom_window` (`SW_MAXIMIZE` activates).
Fullscreen is fine. Scripted input never needs the OS focus: the driver
dispatches events into the gpui window directly and gpui keeps keyboard focus
per window, so clicks, drags, keys, typing into inputs, file drops,
screenshots, resizing, fullscreen (Windows; on macOS a background window
doesn't enter native fullscreen) and second windows all work (`harness.jsonl`
proves each in background mode), and screenshots match a foreground run pixel
for pixel apart from text carets (below).

**What differs**: gpui considers a background window inactive
(`Window::is_window_active()` is `false`, and so is `state.window.active`;
`state.window.background` and the `ready` event's `background` are `true`).
Everything gpui ties to activation therefore stays off:

- window focus events: `on_focus`/`on_blur`/`on_focus_in`/`on_focus_out`
  listeners and gpui-component's `InputEvent::Focus`/`Blur` don't fire, while
  `FocusHandle::is_focused` and focus-scoped key dispatch work as usual. Don't
  make a scenario depend on blur (commit an edit with Enter, not by clicking
  away), or give it a foreground header;
- gpui-component inputs don't paint their caret or selection, so a
  screenshot of a focused input lacks the caret;
- `observe_window_activation` callbacks never run.

Faking the activation isn't possible: a posted `WM_ACTIVATE` reaches
`DefWindowProc`, which focuses and so really activates the window, and a
process the runner started may take the foreground (the watch below caught
exactly that). A scenario that needs an active window sets
`"foreground": true` in its header; the runner then starts that one app
normally.

On macOS only the unfocused, unthrottled part applies, and the window does
the opposite of Windows': macOS stops drawing a window that is fully covered
(gpui's frames stall, and a step fails with "no frame within 10s"), so the
window floats in front of every normal window for the whole run, unfocused.
It is raised to the floating level (`NSFloatingWindowLevel`) and ordered
front with `orderFrontRegardless`, neither of which activates the app; a plain
`orderFront:` from an inactive app can leave it behind the active app's
window. The app also becomes an accessory (no Dock icon, not in the app
switcher). macOS stops drawing on a sleeping display too, so the runner also
holds the display awake for the whole run (`caffeinate -d -u -w <runner>`,
which ends with it and changes no setting). Windows of other apps opened
during a run go behind the app's windows; a full-screen app in another Space
doesn't cover them.

The runner holds apps to it: on Windows it records the foreground window
before starting each scenario and hooks foreground changes
(`EVENT_SYSTEM_FOREGROUND`) for the whole run. If a window of the scenario's
app process ever becomes the foreground window, the step fails with
"the app took the foreground during a background run". Foreground changes
between other processes (the person at the machine switching windows) are
only noted in `transcript.log` as `# foreground changed to ...`.

`--foreground` runs every scenario the old way (focused windows, in front),
for watching a run.

### Screenshots

A `screenshot` takes its pixels from one of two sources (`"source"`):

- **`window`** (the default): the window's own composed frame,
  `PrintWindow(PW_RENDERFULLCONTENT)` on Windows. It works behind other
  windows, so background runs use it.
- **`screen`**: the window's client rect copied from the desktop as shown
  (Windows). It needs the window in front, so it is refused in background
  mode, and fails rather than copying another window's pixels when the window
  isn't the foreground window, is covered at any of 9 sample points, or runs
  off the screen.

What a background run shows is the window **inactive**. Judge layout, type
and colour from background screenshots; what only an active window shows (a
text caret, anything tied to window focus) needs a scenario with a
`"foreground": true` header. The trade-off of a foreground scenario is the
person's machine: it takes the focus for a few seconds, keystrokes typed
meanwhile reach the app, and a click elsewhere fails its screen captures.

On **macOS** both sources are Core Graphics images (`CGWindowListCreateImage`,
at the display's resolution: 2x on Retina, so a 1440x900 window captures as
2880x1800; converted to sRGB; the shadow left out). `window` is the window's
own image, covered or not. `screen` is the window's frame as the screen
composes the window and what is under it (windows above it left out); since a
background window is in front on macOS, it works in background runs there.
They differ at the rounded corners (black in the window's image, the desktop
in the screen's). A window that has just opened is still in AppKit's open
animation, drawn scaled down and then with a few pixels of outline, so a
`window` capture waits (up to 2 s) until the image is the window's frame at
its scale; a full-size image that keeps an outline is taken after 750 ms.
`screen` captures need the Screen Recording permission for whatever runs the
app (the terminal's app, or the SSH session); the runner warns at start when
it is missing. A window's own image works without it.

Compare screenshots with the design at the same scale: screenshots are physical pixels, so a
1440×900 window captures as 2880×1800 on a 2× display.


## Runner

```
e2e [options] <scenario.jsonl | directory | glob>...
  --app <exe>              default <target>/debug/<app>
  --filter <text>          only scenarios whose file name contains <text>
  --out <dir>              default <target>/e2e; one run at a time per dir
  --timeout-ms <n>         base reply timeout per command (default 30000)
  --ready-timeout-ms <n>   how long the app may take to start (default 60000)
  --keep-data              keep each scenario's data dir after it passes
  --jobs <n>               up to <n> scenarios at once (default 1; background only)
  --foreground             focused windows in front instead of background runs
  --host-display           Linux: the session's display instead of a fresh Xvfb
  -v, --verbose            print every request and reply
  ...                      the app's own options (RunnerApp::option)
```

`<target>` is `$CARGO_TARGET_DIR` or the workspace's `target/`; on Windows
the defaults end in `.exe`. Wildcards (`tests/e2e/*.jsonl`) are expanded by
the runner itself, so they work from PowerShell and cmd too; a directory
means every `*.jsonl` in it. The runner never builds anything.

`--jobs` runs scenarios side by side: each has its own app and data dir, and
the foreground check looks at its own app's pids. Each prints `----` as it
starts and its result line as it ends, so they interleave by line;
`script/check` passes `--jobs 4`. Foreground runs take the focus, so they run
one at a time.

A run holds an OS lock on `<out>/.e2e.lock` for its whole duration. The lock
is released when the process ends, however it ends. A second runner given the
same `--out` fails at once with `another e2e run is using <out> ...` instead
of clearing the first run's scenario dirs, so give concurrent runs different
`--out` dirs inside `target/`.

Per scenario the runner:

1. Clears `<out>/<scenario>/` and creates its data dir,
   `<out>/<scenario>/data`. Test data never goes to the system temp dir; the
   next run of the scenario wipes it.
2. Starts the app as
   `<app> --automation --automation-background --data-dir <data> --window-size WxH [app_args...]`
   in the data dir, with a hermetic environment:
   - `the app's data-dir variable (`<APP>_DATA_DIR`)` is always the same data dir, so even a launch without
     `--data-dir` (a `restart` with `"data_dir": false`) never reaches the
     owner's real one;
   - every inherited variable named `<APP>_*` (in any case) is removed, so
     nothing from the owner's shell (`<APP>_PLAYER_REAL`, a data dir) reaches
     the app; only the runner and the header's `env` set them;
   - `E2E_OUT_DIR` and `E2E_DATA_DIR` name the scenario's dirs;
   - a header's `env` may not set `the app's data-dir variable (`<APP>_DATA_DIR`)`, `E2E_OUT_DIR` or
     `E2E_DATA_DIR`.
3. Waits for `{"event": "ready"}`, then sends each step, checking its
   expectations. A background run fails a step whose app took the
   foreground.
4. Sends `quit` after the last step (unless a step did) and requires a clean
   exit within 15 s.

A failing step stops the scenario: the runner saves `failure.png`, prints the
line, the error and the tail of the app's stderr, keeps the data dir and
moves on to the next scenario. The exit code is 0 when everything passed, 1
when a scenario failed, 2 for usage errors and 130 when interrupted. Ctrl-C
stops the current scenario and kills its processes; on Windows every child
also sits in a kill-on-close job object, so even a runner that is killed
outright takes them with it.

Outputs in `<out>/<scenario>/`:

| File | Contents |
|---|---|
| `NN-<name>.png` | screenshots in scenario order (`01-start.png`, ...), physical pixels |
| `failure.png` | the window at the moment a step failed |
| `transcript.log` | every line sent (`>`) and received (`<`), exits and restarts (`#`), timestamped |
| `app.stderr.log` | the app's log; after a restart it goes to `app.2.stderr.log`, `app.3.stderr.log`, ... |
| `data/` | the app's data dir (`--data-dir`, `the app's data-dir variable (`<APP>_DATA_DIR`)`, its working directory): removed once the scenario passes unless `--keep-data`, kept after a failure |

### Runner extensions

An app's `e2e` bin is a few lines around its `RunnerApp`:

```rust
struct DvRunner { mock_gh: Option<PathBuf> }

impl gpui_automation_runner::RunnerApp for DvRunner {
    type Header = DvHeader;        // {"gh": "fixtures/gh/review.json"}, deny_unknown_fields
    type Action = DvAction;        // {"gh_log": {}}, externally tagged
    const HEADER_KEYS: &'static [&'static str] = &["gh"];
    const ACTIONS: &'static [&'static str] = &["gh_log"];
    const VARS: &'static [&'static str] = &["dv_mock_gh_log"];
    const RESERVED_ENV: &'static [&'static str] = &["DV_GH", "DV_MOCK_GH", "DV_MOCK_GH_LOG"];

    fn profile(&self) -> Profile {
        Profile {
            manifest_dir: env!("CARGO_MANIFEST_DIR"),
            name: "dv",
            build_hint: "cargo build -p dv",
            env_prefix: "DV_",
            data_dir_var: "DV_DATA_DIR",
            default_window_size: "1440x900",
            platforms: &["windows", "linux", "macos"],
        }
    }

    fn fixture(&self, header: &DvHeader, cx: &FixtureCx) -> Result<Box<dyn Fixture<DvAction>>> {
        /* points DV_GH at mock_gh, logs to <out>/gh.log, answers gh_log from it */
    }
}

fn main() -> ExitCode { gpui_automation_runner::main(DvRunner { mock_gh: None }) }
```

- **`Profile`**: the default app (`<target>/debug/<name>`), the hint when it's missing, the
  prefix of every variable the app reads (inherited ones are removed; a header's `opt_in` must
  use it), the data-dir variable, the default window size and the platforms a scenario may
  name.
- **Header keys**: the runner takes its own keys out of a header first; the rest must be the
  app's (`HEADER_KEYS`) and deserialize into `Header`, which rejects unknown fields, so a typo
  in either is an error. `RunnerApp::check` adds load-time checks (a file the header names must
  exist).
- **Actions**: a line whose key is one of `ACTIONS` is a runner step, run by the fixture in the
  runner's process (reading a mock's log, poking a sidecar), never sent to the app. It takes
  `expect_result`, `save` and `platforms` like a command; `Fixture::retry_for` runs it again
  until `expect_result` holds, for a log another process writes. It may follow a `quit` when it
  has no `expect`.
- **The fixture** is made per scenario before the first launch: `vars` (`${...}`), `env` and
  `args` for every launch, `run` for actions, and `finish` after the last exit (no child
  process outlived the app, say).
- **Reserved names**: an app's header keys, actions and variables may not be the runner's own;
  the runner refuses to start otherwise, so a key added here later fails loudly instead of being
  quietly taken by an app. A header's `env` may not set the fixture's `RESERVED_ENV`.

An app with no extensions uses `NoHeader` and `NoAction` and keeps the default `fixture`
(`crates/automation_harness/src/bin/harness-e2e.rs`).

## Scenario format

One JSON object per line. Blank lines and lines starting with `//` or `#`
are ignored. The first line may be a header:

```json
{"scenario": {"window_size": "1440x900", "app_args": ["--player", "fake"], "env": {"RUST_LOG": "info"}, "platforms": ["macos"], "foreground": false, "opt_in": "<APP>_PLAYER_REAL", "description": "..."}}
```

All header fields are optional (`window_size` defaults to `1440x900`, the
size the boards are compared at). `foreground: true` starts this scenario's
app focused and in front even in a background run (see "Background runs").
`platforms` limits the scenario to some operating systems
(`std::env::consts::OS` names: `windows`, `macos`); others skip it.
`opt_in` names a `<APP>_` variable the runner's own environment must set to
`1` (`"opt_in": "<APP>_PLAYER_REAL"`); otherwise the runner
reports the scenario as skipped. The app never gets the variable.

Every other line is a step: an automation command (Protocol, below) with
optional runner-only keys, or a runner-only step:

| Key | Meaning |
|---|---|
| `expect` | expectation(s) checked against a fresh `state` after the action (with the command's `window`) |
| `expect_result` | expectation(s) checked against the action's own result: a command's `result`, a restart's `ready` event |
| `expect_error` | the command must fail with an error containing this text |
| `save` | `{"name": "/json/pointer"}`: variables from the result, used later as `${name}` |
| `platforms` | the operating systems this line runs on; elsewhere the runner passes over it |
| `runner_timeout_ms` | the reply timeout for this line, instead of the default |

A line with only `expect` checks `state` and does nothing else. The
runner-only step:

- **`restart`** sends `quit`, requires a clean exit within 15 s, then starts
  the app again with the same data dir, window size and environment and
  waits for `ready`. `app_args`, when given, replace the header's for this
  and later launches. `"data_dir": false` starts it (and later launches)
  without `--data-dir`, so it finds its data dir as a normal start does,
  through `the app's data-dir variable (`<APP>_DATA_DIR`)`; `"data_dir": true` puts the flag back. Screenshot
  numbering and the transcript carry on. After an explicit `{"cmd": "quit"}`
  only a restart may follow.

Don't set `id`: the runner numbers requests by line. A screenshot takes a
`name` and the runner turns it into `<out>/<scenario>/NN-<name>.png`; an
explicit `path` is used instead, and a relative one resolves against the
runner's working directory, not the app's. The default reply timeout is 30 s
plus whatever the command itself waits (`ms`, `timeout_ms`,
`hold_ms + step_ms * steps`).

Strings in commands, expectations and `app_args` may use `${data_dir}` (this
run's data dir, also the app's working directory), `${out_dir}` (its output
dir) and `${workspace}` (the repo root), e.g.
`"path": "${workspace}/tests/fixtures/media"`. Variables set by `save` work in
commands, expectations and a `restart`'s `app_args`.

### Expectations

`{"pointer": <RFC 6901 pointer>, <check>: <operand>, ...}`; all checks on a
line must hold, and `expect` may be a list.

| Check | Holds when |
|---|---|
| `equals` | deep-equal; numbers compare by value with a 1e-6 relative tolerance |
| `not_equals` | missing, or not deep-equal |
| `contains` | substring of a string; an array element that matches (objects by subset); a subset of an object |
| `exists` | `true`: the pointer resolves (a `null` value counts); `false`: it doesn't |
| `gte`, `lte` | the value is a number `>=` / `<=` the operand |
| `len` | array length, object key count, or string length in characters |

`""` points at the whole document. Unknown keys are errors, so a typo like
`"equal"` fails the scenario instead of asserting nothing.


## Checked in the foreground

macOS doesn't take a background window into native fullscreen, and gpui only gives an active
window focus and blur, a text input's caret and selection. Such behaviour is checked with
`"foreground": true` in the header, or by hand with a foreground scratch scenario (Exploratory
runs, below). `window`'s result reports `can_fullscreen` (AppKit takes only a resizable window
into fullscreen, and gpui makes a window without a title bar unresizable), so a background
scenario can at least check that a window could go fullscreen.

## Exploratory runs

Looking at the app by hand, or trying a change that no scenario covers yet,
keeps to the same rules as a scenario: a data dir under `target/`, windows in
the background, and a run that ends with `quit`. A plain launch of the app without `--data-dir` is the owner's own
run, never an exploratory one: it uses the real data dir.

**A scratch scenario** is the simplest way. Keep it out of `tests/e2e`, where
every `*.jsonl` is a test, for example in `target/tmp/<task>.jsonl`:

```json
{"scenario": {"window_size": "1440x900"}}
{"cmd": "wait_ready", "timeout_ms": 20000}
{"cmd": "settle"}
{"cmd": "screenshot", "name": "main"}
{"cmd": "state"}
```

```bash
cargo build -p <app>
cargo run -p <app> --bin e2e -- --out target/tmp/e2e-<task> --keep-data target/tmp/<task>.jsonl
```

Everything in "Runner" applies: the hermetic environment, background windows,
the closing `quit`, and killing whatever is left. The replies, `state`
included, are in `target/tmp/e2e-<task>/<task>/transcript.log` (`-v` prints
them as well), next to the screenshots and, with `--keep-data`, the data dir.
Giving the run its own `--out` keeps it clear of a full run's `target/e2e`.
Delete the scratch output when done.

**Attached over a named pipe** (Windows), to send commands one at a time to
a running app. This needs PowerShell 7.4 or later for
`Start-Process -Environment`. Run it as one script, because the process id
lives in its variables:

```powershell
cargo build -p <app>
$dir = (New-Item -ItemType Directory -Force 'target/tmp/explore').FullName
$app = $null
try {
    # the app on a scratch data dir.
    $app = Start-Process target/debug/<app>.exe -NoNewWindow -PassThru -RedirectStandardError "$dir/app.log" `
        -ArgumentList "--automation-pipe <app>-dev --automation-background --data-dir $dir/data" `
        -Environment @{ <APP>_DATA_DIR = "$dir/data" }
    $pipe = [System.IO.Pipes.NamedPipeClientStream]::new('.', '<app>-dev', 'InOut'); $pipe.Connect(60000)
    $reader = [System.IO.StreamReader]::new($pipe); $writer = [System.IO.StreamWriter]::new($pipe)
    $writer.AutoFlush = $true
    $reader.ReadLine()                                           # {"event":"ready",...}
    $writer.WriteLine('{"id": 1, "cmd": "state"}'); $reader.ReadLine()
    # ...more commands (Protocol, below)...
    $writer.WriteLine('{"id": 2, "cmd": "quit"}'); $reader.ReadLine()
    $pipe.Dispose(); $app.WaitForExit(15000) | Out-Null
} finally {
    if ($app) { Stop-Process -Id $app.Id -ErrorAction Ignore }
}
```

Paths in commands resolve against the app's working directory, which is the
repo root here. The app refuses to start if another process already serves
the pipe name, so give concurrent sessions different names.

## Protocol

The app reads one JSON request per line on stdin and writes exactly one
reply line per request on stdout:

```json
{"id": 7, "cmd": "click", "window": "main", "target": "empty/choose-files"}
{"id": 7, "ok": true, "result": {"clicked": [140.0, 212.0], "count": 1}}
{"id": 8, "ok": false, "error": "no target \"empty/choose-file\" is rendered in window \"main\" (#0); rendered targets include ..."}
```

`id` is echoed (`null` if absent). `window` (a registered name or an index,
see `windows`) addresses one command to a window other than the selected
one. Unknown arguments are errors. Besides replies the app prints one event,
`{"event": "ready", "windows": [...], "pid": N}`, once its window has drawn.
Nothing else goes to stdout; logs go to stderr. Closing stdin quits the app.

**The handshake.** The `ready` event carries `"protocol": 1` (it goes up only for a change that
breaks drivers), `"commands"` (the app's own, from `AutomationHost::commands`) and
`"capabilities"` (`screenshot`, `screen_capture`, `background`). When an app lists its commands,
the runner checks every line of a scenario against them and the builtins before running any
step, so a typo fails at once with its line (lines with `expect_error` are left alone). An app
that lists none isn't checked.

**Events.** Lines with `"event"` and never `"id"` are unsolicited: the runner records them in the
transcript and never takes them for replies. An event a request caused names it as
`"for": <id>`.

**`settle`.** Any request may say what its reply waits for after the input it sends:
`"frame"` (the default, one drawn frame, below) or `"none"` (reply once the input is
dispatched). `"quiet"` is reserved for "no frame drawn for 100 ms, at most 2 s" and fails as not
implemented yet.

**Selectors.** Pointer commands take `"selector": "target:<name>"` as well as `"target"`. Only
`target:` resolves today; `id:`, `label:`, `text:`, `role:`, `path:`, scoping with `>>` and
`nth=` are reserved for gpui-base's element registry and fail as not implemented yet, and an
unknown prefix fails as unknown, so a selector written now never changes meaning later.

With `Options::pipe` (an app's `--automation-pipe <name>` flag, which the harness has) the same protocol runs over the Windows named pipe
`\\.\pipe\<name>` instead, so a driver can attach to an app that is already
running. Stdin and stdout are then left alone. One client at a time (another
gets `ERROR_PIPE_BUSY`); each is greeted with the `ready` event once the
window has drawn; disconnecting doesn't quit the app, and the next client can
connect straight away. Replies to a client that has left are dropped, and a
long wait whose client has left gives up early, as it does when stdin closes.
The app refuses to start if another process already serves that name. Any
language can drive it; from PowerShell:

```powershell
$pipe = [System.IO.Pipes.NamedPipeClientStream]::new('.', '<app>', 'InOut')
$pipe.Connect(5000)
$reader = [System.IO.StreamReader]::new($pipe); $writer = [System.IO.StreamWriter]::new($pipe)
$writer.AutoFlush = $true
$reader.ReadLine()                                  # {"event":"ready",...}
$writer.WriteLine('{"id": 1, "cmd": "state"}'); $reader.ReadLine()
$pipe.Dispose()                                     # the app keeps running
```

Every command that injects input replies only after the window has drawn
the result, so the next command never hit-tests a stale frame and `state`
already reflects the change; no `wait` is needed between steps. Commands
that wait on the frame loop fail after 10 s without frames (a minimised
window). Waits are capped at 120 s.

Positions are window coordinates in logical pixels (`x`, `y`), or a
`target`: a name views attach with
`.automation_target("sidebar/item/Kyoto 2025")`. A target command
captures one fresh frame of the window and uses the centre of the target's
visible (unclipped) bounds, so it keeps working when layout moves; a target
that is rendered but scrolled out of view is an error, not a click on the
wrong thing. Coordinates outside the window are rejected, since a drag
leaving the window could turn into a real OS drag-and-drop.

| Command | Arguments | Result |
|---|---|---|
| `state` | | the host's state object plus `window` and `windows` (below) |
| `windows` | | `[{index, name, id, selected}]`: registered windows in registration order, then unregistered ones |
| `window` | `index` or `name` | selects the default window for later commands; returns its `window` info |
| `actions` | | every registered action name |
| `action` | `name` (full, or a unique suffix such as `"ToggleSplit"`), `data`? | `{action}` with the full name |
| `key` | `keystroke` (`"ctrl-shift-k"`; `secondary` is the platform modifier, as in gpui's keymap: `Cmd` on macOS, `Ctrl` elsewhere, so a scenario that presses the app's `Ctrl`/`⌘` keys writes `"secondary-o"` and runs on every platform) | `{handled}`; the key goes down and comes back up, so Enter or Space on a focused button presses it. It arrives as the OS's own event would: on Windows a shifted digit or punctuation key becomes its shifted character through gpui's keyboard mapper (`"shift-["` is `{` on a US layout), as key bindings are mapped |
| `keys` | `keys`: `"ctrl-k down enter"` or a list | `{handled: [..]}`; each key goes down and up, and a frame renders between keystrokes |
| `type` | `text` | `{typed}`; per-character keystrokes into the focused input, `\n` is `enter` |
| `click` | `x`,`y` or `target`; `button` (`left`/`right`/`middle`); `modifiers` (`"ctrl-shift"`; `"secondary"` as in `key`); `count` (2 = double click) | `{clicked, count}`; hovers and renders first, like a real pointer. A `target` is pressed once two frames in a row drew it in the same place (up to 1 s), so a button still sliding in (a dialog's entrance) is pressed where it is, not where it was |
| `mouse_move` | `x`,`y` or `target`; `modifiers` | `{moved}`; carries a held button, so it also drags |
| `mouse_down`, `mouse_up` | position optional (defaults to the pointer); `button`, `modifiers` | `{at, down}` |
| `drag` | `from`, `to` (each `[x, y]`, `{"x","y"}`, `{"target"}` with an optional `dx`/`dy` offset from its centre, or a target name); `steps` (10), `step_ms`, `hold_ms`, `release` (true), `button`, `modifiers` | `{from, to, steps, released}` |
| `scroll` | position or `target`; `dx`, `dy`; `unit` (`px` default, or `lines`) | `{at, dx, dy}`; negative `dy` scrolls down, as a wheel notch towards you does |
| `drop_files` | `paths` (must exist); position or `target`; `hold_ms`; `submit` (true); `cancel` | `{paths, at, outcome}` (`dropped`, `hovering`, `cancelled`) |
| `resize` | `w`, `h` (logical), `timeout_ms`? | `{w, h}` once the viewport has that size. A size below the window's minimum (`WindowOptions::window_min_size`, else the system's) is clamped to it, on every platform, and `{w, h}` is the clamped size. On Windows the size is exact even when it is taller or wider than the display (a portrait 1080x1920); a fullscreen or maximised window is an error |
| `window_control` | `x`, `y` or `target` | Windows only: `{at, area}`, what the window answers to `WM_NCHITTEST` with the pointer there (it moves first: gpui answers from the hitboxes under it). `area` is `caption` (a `WindowControlArea::Drag` strip), `min`, `max`, `close`, `client`, or `ht <code>` for an edge. Elsewhere an error, since the OS hit-tests its own title bar |
| `dpi_changed` | `dpi` (48–960; 96 is 100 %) | Windows only: `{scale}` once a frame has drawn. Sends the window the `WM_DPICHANGED` a move to another display or a new scale in Settings sends, with the rectangle Windows would suggest (the window's own, scaled from the DPI it last took), so gpui takes the new scale. The display itself doesn't change. Elsewhere an error |
| `fullscreen` | `on`, `timeout_ms`? | the `window` info once the switch has happened |
| `screenshot` | `path`; `source`: `"window"` (default) or `"screen"` (Windows, foreground only; see "Screenshots") | `{path, source, w, h, scale, logical: {w, h}}`; `w`/`h` are physical pixels |
| `bounds` | `target` | `{target, x, y, w, h, visible, count}`; `visible` is `null` when clipped away |
| `query` | `prefix` (`""` = all) | every rendered target with that prefix, in render order |
| `wait` | `ms` | `{waited_ms}` |
| `wait_frames` | `n` (1) | `{frames, elapsed_ms}` |
| `wait_ready` | `timeout_ms` (15000) | `{ready, elapsed_ms}` once the host reports settled |
| `settle` | `timeout_ms` (10000) | `{frames, elapsed_ms}` once the host is settled and a frame has rendered |
| `wait_for` | an expectation (`pointer` + checks), `timeout_ms` (10000), `interval_ms` (25) | `{elapsed_ms, actual}`; the error carries the last mismatch |
| `assert` | an expectation | `{actual}`, or an error describing the mismatch |
| `quit` | | `{quitting: true}`, then the app exits |

Anything else is offered to the app's `AutomationHost::custom`, which is how
apps add their own commands, then to `AutomationHost::custom_task`, for
commands that finish later (a store write, a scan): its reply waits for the
task, within the 120 s cap; unknown commands are errors. A command can't take
an argument called `id` (that is the request id).

`state.window` is
`{index, name, id, w, h, scale, fullscreen, maximized, can_fullscreen, active, background, bounds: {x, y, w, h}, display: {id, bounds}}`
(logical pixels; `bounds` is the window's position on the desktop; `can_fullscreen`, macOS
only and `null` elsewhere, is whether AppKit would take the window into native fullscreen: it
does only for a resizable window, and gpui makes one without a title bar unresizable) and
`state.windows` repeats the `windows` list.

### Multi-step drags and drops

`drag` moves in `steps` increments and waits for a rendered frame after each
one, so drag-over styling, drag previews and `on_drag_move` handlers see
every position as they would with a real mouse. `hold_ms` hovers at `to`
before releasing (hover-to-expand timers run meanwhile). With
`"release": false` the button stays down: take a screenshot or assert
mid-drag, then continue with `drag` without `from`, or finish with
`mouse_up`.

`drop_files` sends the platform's own `FileDrop` events (entered, then
over, then dropped), so the app's real `on_drop::<ExternalPaths>` code runs.
`"submit": false` leaves the files hovering; a later `drop_files` without
`paths` continues that drag, and `"cancel": true` drags them back out. gpui
answers a submitted drop with `App::activate`, so in a background run the
driver sends the drop's mouse-up alone (the same drop for the app's
`on_drop` listeners) and the app stays in the background.

## Wiring an app (`gpui_automation`)

Depend on the crate unconditionally and turn the driver on from the app's own feature:

```toml
[dependencies]
gpui_automation = { git = "https://github.com/kylekz/toolbox-rs", tag = "v0.1.0" }

[features]
automation = ["gpui_automation/enabled"]
```

Without `enabled`, `automation_target`, `open_window`, `pointer_guard`, `is_background` and the
other names a view calls are no-op stand-ins, so the views need no `cfg`; the host below and
`start` exist only with it.

```rust
impl gpui_automation::AutomationHost for AppState {
    fn state(&self, window: &Window, cx: &App) -> serde_json::Value { /* the assert surface */ }
    fn settled(&self, cx: &App) -> bool { /* no pending tasks, streams or animations */ }
    // optional: the custom commands, sent in `ready` so the runner checks scenarios up front
    fn commands(&self) -> &'static [&'static str] { &["seed_repo"] }
    fn custom(&mut self, cmd: &str, args: Value, window: &mut Window, cx: &mut Context<Self>)
        -> Option<anyhow::Result<Value>> { None }
    // optional: commands whose reply waits for a task
    fn custom_task(&mut self, cmd: &str, args: Value, window: &mut Window, cx: &mut Context<Self>)
        -> Option<Task<anyhow::Result<Value>>> { None }
}

// before the first window, when --automation-background was passed:
gpui_automation::set_background(true);
// open every window through this (a plain cx.open_window unless in the background):
let window = gpui_automation::open_window(cx, options, |window, cx| /* root view */)?;
// after opening the main window, when --automation was passed
// (`Options { pipe: Some(name), ..Default::default() }` for --automation-pipe <name>):
gpui_automation::start(window.into(), app_state, cx, gpui_automation::Options::default())?;
// when opening another window, also through open_window:
gpui_automation::register_window("second", second.into(), cx);
// never activate in the background:
if !gpui_automation::is_background() { cx.activate(true); }
// in each window's root view, as its first child:
div().child(gpui_automation::pointer_guard()) /* ... */
```

Views name elements with `use gpui_automation::AutomationTargetExt as _;`
and `.automation_target("sidebar/item/Kyoto 2025")` (or
`.automation_target_with(|| format!(..).into())` for computed names). When
automation is off this is a plain `into_any_element()`; when it is on, bounds
are recorded only during the one frame a target command captures. Name
targets hierarchically so `query` can list a group: `shell/sidebar`,
`sidebar/item/<name>`, `editor/tile/<n>`, `hud/next`.

Known limits: `Window::is_window_hovered` follows the real OS cursor, so
hover logic gated on it needs a `gpui_automation::is_active()` bypass;
the named-pipe transport is Windows-only; screenshots work on Windows and on
macOS (Core Graphics, "Screenshots"); and on macOS `resize` goes through
gpui, where AppKit clamps a size larger than the display (`harness.jsonl`
resizes past the display on Windows only).
