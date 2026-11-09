# toolbox-rs: plan

Shared crates for slide, diff (dv) and fc, replacing the copies each app carried. folio and
terrarium are out of scope (references only). Crates that need gpui are `gpui_*`; crates that
don't are `toolbox_*` (the automation family keeps its `gpui_automation_*` names).

## 0. Status

- **Phase 1 (automation) is built:** `gpui_automation`, `gpui_automation_protocol`,
  `gpui_automation_runner` and the dev-only `automation_harness` (§2). Every gate passes on
  macOS, including the harness scenarios (126 steps) through the new runner. Windows and Linux
  are checked by CI and the owner's PC.
- **Phase 2 is built and adopted** (§5a). Each crate has its own tag and document:
  - `toolbox_process`, `v0.2.0` ([docs/process.md](process.md));
  - `toolbox_stills`, `v0.3.0` ([docs/stills.md](stills.md));
  - `gpui_appearance`, `v0.4.0` ([docs/appearance.md](appearance.md)): light/dark followed live
    with app and automation overrides, Mica/Acrylic on Windows 11, the crate's own vibrancy on
    macOS. It is proved by `harness_appearance.jsonl` and by foreground captures on the owner's
    Mac and Windows 11 PC (Mica, Acrylic, the transparency switch on and off);
  - `toolbox_store`, `v0.5.0` ([docs/store.md](store.md)): the apps' SQLite layer on rusqlite in
    place of their GPL sqlez copies, carrying on from sqlez's `migrations` table. It is proved on
    databases built as sqlez leaves them, and on copies of the owner's real fc library.
- **The apps** are all on `v0.5.0`, verified on macOS and on the owner's Windows 11 PC (dv also
  in WSL):

  | App | Crates it uses |
  |---|---|
  | dv | automation, `toolbox_process`, `gpui_appearance` |
  | fc | automation and all four phase 2 crates |
  | slide | automation, `toolbox_stills`, `gpui_appearance`, `toolbox_store` |

  Neither fc nor slide carries sqlez any more.
- **`v0.5.1`** fixes two automation quirks, verified on the owner's Windows PC:
  - On Windows, captures of windows with a gpui-drawn title bar leave out the top rows that
    DWM's border covers (2 px at 150%).
  - The runner's mode line reports foreground scenarios as running in the foreground.

  No app depends on a capture's pixel size, so the apps can take it at their next change.
- **The stack:** `gpui-pre =0.3.11`, `gpui-pre-platform =0.3.11`, `gpui-component =0.9.0`
  (gpui-kit 0.9). Each is pinned on its own, as the apps do, not through the `gpui-kit` umbrella.
  Rust 1.98.1, edition 2024, resolver 3, as in all three apps.
- **gpui bumps:** almost every compile break in the apps' 0.9 bumps was inside
  `gpui_automation`:
  - `ScrollEvent`;
  - `Keystroke::parse_with_os`;
  - the removed dialog layer;
  - Windows keystrokes typed through the keyboard mapper.

  That crate is now shared, so the next bump is fixed once here rather than in each app.
- **Next:** phase 3, `gpui_theme_kit` (§5b), with `gpui_system` and `toolbox_app_shell` (§6,
  item 4).

## 1. Decisions (settled 2026-11-09)

| # | Decision | Settled |
|---|---|---|
| O1 | Repository | `toolbox-rs`, public, created by the owner. Checked out at `~/code/toolbox-rs`. |
| O2 | How apps depend on it | A git dependency pinned to a tag (`tag = "v0.1.0"`). Never published to crates.io. While developing across repos, a local `[patch."https://github.com/…"]` path override, never committed. |
| O3 | Licence | Every crate is `MIT`; no GPL code goes in the repo. `sqlez` (Zed's GPL-3.0-or-later crate) stays copied inside each app that uses it (slide, fc) until the app moves onto `toolbox_store` (§5a). A git submodule or git dependency on Zed doesn't work: Zed's repo is about 540 MB, today's `sqlez` pulls in Zed's internal `collections`, `path` and `gpui_util` crates and follows Zed's main branch, and it lacks our `Connection::open_read_only`, which slide's database guard and backups use. The toolbox README links to Zed's `crates/sqlez` and lists our local changes. `open_router` stays in the apps too: fc's copy is shaped to fc. |
| O4 | Existing scenarios | The e2e test scripts (`tests/e2e/*.jsonl`: 42 in diff, 27 in slide, 39 in fc) keep running unchanged on the shared runner. Core keys and actions keep their names; each app's own keys move into its extension (§3). |
| O5 | gpui pin | toolbox-rs owns the pin. An app on a toolbox tag must use exactly that tag's gpui and gpui-kit versions (Cargo builds one gpui per app). A gpui-kit bump is a new toolbox minor tag; each app moves when it's ready. |
| O6 | Protocol and API design | Left to the agent (§4). |

## 2. Crates (phase 1: automation)

```
gpui_automation_protocol   no gpui    wire format, matcher, builtin commands' arguments, selectors
gpui_automation            gpui       the in-app driver (`enabled`) or its no-op stand-ins
gpui_automation_runner     no gpui    the e2e runner as a library; each app has a small bin
automation_harness         dev-only   the harness app and its runner: the crates' own proof
```

The seed is slide's copy at `e01f38c`. Its library is diff's plus fc's macOS fixes plus slide's
own features (`window_control`, `dpi_changed`, Windows hit-testing, rounding fixes); its runner is
fc's minus the app mocks. On top of it go diff's Linux support (`capture_x11`, the Linux capture
arms, the runner's Xvfb `display`, `OperatingSystem::Linux` for `secondary-`) and the interface
decisions in §3–§4.

### 2.1 `gpui_automation_protocol`
`protocol` (requests, replies, events, [§4](#4-protocol-decisions)), `matcher`, `commands` (the
builtin commands' arguments, `CaptureSource`) and `parse_window_size`. The runner depends on this
crate alone, so building it never compiles gpui (`script/check` asserts it).

### 2.2 `gpui_automation`
- **Feature `enabled`** turns the driver on; an app's own `automation` feature enables it.
- **Without it**, the crate is the stand-ins each app used to keep in its `*_ui/src/automation.rs`
  (code-identical in dv, fc and slide): `automation_target` is `into_any_element()`,
  `open_window` is `App::open_window`, `is_background` is `false`, and so on. Views call them
  unconditionally.
- `AutomationHost` gains `commands()`, the app's own commands, for the handshake (§4, item 2).
- **Still to come, with slide's move (§5):** the host glue every app writes by hand:
  - argument helpers (`object`, `string`, a path inside the data dir);
  - an optional `clap` `AutomationArgs` (`--automation`, `--automation-pipe`,
    `--automation-background`, `--window-size`, `--data-dir`).
  
  Taken from the hosts as they are, once two of them sit side by side.
- **Later:** `open_window` building on `gpui_kit`'s window opening (Root plus overlays), keeping
  the background rule.

### 2.3 `gpui_automation_runner`
- `main(app)` runs the command line's scenarios; the scenario format lives here, since only the
  runner reads it.
- `Profile` holds what used to be constants:
  - the executable and its build hint;
  - the env prefix and the data-dir variable;
  - the default window size and the platforms;
  - the manifest dir the workspace is found from.
- **For fixtures (fc's move, `v0.1.3`):**
  - `Sidecar`: a helper process (fc's mock OpenRouter server) started as the runner's child,
    in its kill-on-close job on Windows, that announces `{"port": N}` and takes HTTP requests
    on it (fc's `http.rs` and `read_announced_port`);
  - `FixtureCx::env`, the header's `env`, so a fixture's defaults (fc's dummy API keys) give
    way to a header's and a sidecar gets the app's environment;
  - `RunnerApp::check_env`, load-time checks of the header's `env` (fc refuses `FC_LIVE*`);
  - `Fixture::finish` runs after the last exit whatever the outcome, so a failed scenario
    still reaps the processes its app left (fc's orphan check).

## 3. The app boundary

`RunnerApp` (header keys, actions, variables, reserved env, `profile`, `option` for the app's
own command-line flags, `check`, `fixture`) and `Fixture` (`vars`, `env`, `args`, `run`,
`retry_for`, `finish`). docs/automation.md "Runner extensions" has the shapes.

- **Parsing in two passes keeps O4.** The runner takes its own keys from a header or line, and
  the rest must be the app's. `serde(flatten)` can't be combined with `deny_unknown_fields`,
  which is why it takes two passes.
- **Reserved names.** At startup the runner refuses an app whose names reuse its own, so a key
  added to the core later fails loudly instead of being quietly taken by an app.
- **Where things run.** Runner actions run in the runner's process (mock logs, sidecars, files).
  `cmd` always goes over the wire to `AutomationHost::custom` / `custom_task`.
- **What each app's extension will hold:**
  - **dv:** the `gh` and `lsp` headers, the `gh_log` and `lsp_log` actions (`lsp_log`'s
    `wait_ms` is `retry_for`), and the `DV_GH*` / `DV_LSP_*` / `GIT_CEILING_DIRECTORIES`
    environment, plus `--mock-gh` / `--mock-lsp` through `option`.
  - **fc:** the `mock` / `mock_args` / `tools` headers, the `mock`, `tool_log` and
    `tool_release` actions, and the orphan check in `finish`.
  - **slide:** none.

## 4. Protocol decisions

Settled before extraction, because changing them once three apps depend on them means touching
every scenario or every host.

1. **Selectors.** `target` stays an exact automation-target name, forever. `selector` takes
   nsh-testing's grammar:
   - only `target:` resolves today;
   - `id:`, `label:`, `text:`, `role:`, `path:`, `>>` and `nth=` are reserved and fail as not
     implemented;
   - an unknown prefix fails as unknown.

   The reserved prefixes wait for gpui-base's element registry (§6.1). An element tree will come
   from its own `tree` command, never from `state`, which the runner reads after every
   `expect`.
2. **Handshake.** `ready` carries `protocol: 1`, `commands` and `capabilities` (`screenshot`,
   `screen_capture`, `background`). The runner checks a scenario's commands against an app that
   lists them before running any step. A header `requires: [...]` will come with the headless
   executor (§6.2).
3. **Waiting after each step.** `settle` on any request: `frame` (the default, today's
   behaviour) or `none`. `quiet` (no frame drawn for 100 ms, 2 s at most, endless animations
   ignored, as in nsh-testing) is reserved. The default stays `frame`, because changing it would
   change every scenario's timing.
4. **Events.** Lines with `event` and never `id`; `for` names the request that caused one. The
   runner records them in the transcript. `--trace` builds on this later (§6.3), and the same
   envelope could carry requests from the app to the runner, which might replace fc's HTTP mock
   sidecar.

## 5. Migration

1. **Create the repo (done)** from slide's crate, with diff's Linux pieces. Its tests:
   - the crates' own tests;
   - `harness_e2e` against `automation_harness`, on macOS, Windows and Linux (Xvfb).
   
   CI follows the same hardening rules as the apps (pinned actions, read-only tokens). The
   owner tags `v0.1.0`.
2. **slide**, after its own 0.9 work is on main:
   - delete `crates/gpui_automation` and `slide_ui/src/automation.rs`;
   - depend on the tag;
   - add a ~10-line `e2e` bin;
   - all 27 scenarios unchanged.
   
   This proves the boundary.
3. **diff**, after `feat/gpui-kit-0.9` merges:
   - the same steps, with `DvRunner` carrying the gh/lsp/WSL fixture;
   - all 42 scenarios unchanged;
   - `script/check` builds the e2e bin from the app's own crate;
   - check that `script/check --linux-gui` still runs the Linux e2e under Xvfb.
4. **fc:** bump to 0.9 first (expected to be easy), then the same steps with `FcRunner` (mock
   sidecar, tools).

Each app's CLAUDE.md and PLAN §14 then point at the shared crate's docs, not their own copy.

## 5a. Phase 2 crates (no gpui)

### `toolbox_process`: running other programs
**Built.** The API, each app's adoption (file by file) and what the crate leaves out are in
[docs/process.md](process.md).
- **Why:** fc's runner (`fc_tools/src/process.rs`) was written after dv's
  (`dv_core/src/backend/process/local.rs`), and the two have since drifted. dv also has two
  more "run to the end with a timeout" loops (`git/command.rs`, `dv_cli/src/checks/process.rs`)
  and five copies of the Windows no-console snippet.
- **Existing crates checked (2026-11-09):** process-wrap 10.0.1, command-group, duct,
  shared_child, subprocess, async-process, process_control, win32job, and what cargo and
  rust-analyzer do. None meets the requirements. The closest, process-wrap, falls short on the
  points that matter most:
  - with std (no tokio) its Windows job never sets kill-on-close, so children outlive the app
    (`std/job_object.rs:106`);
  - it resumes a suspended child by snapshotting every thread on the machine, about 42 ms per
    spawn as fc measured, where `NtResumeProcess` is instant;
  - it can't kill a Unix group before reaping its leader;
  - it has no timeout, cancellation, graceful stop or line streaming.
  
  Moving onto it would save almost nothing and weaken fc's guarantees. We use `which` for finding
  executables. Look again if process-wrap takes upstream fixes for the first two points.
- **Seed:** fc's copy, which is the more refined:
  - Windows: started suspended, put in a job, then resumed. This closes dv's race, where a
    grandchild could escape the job.
  - Unix: a polite stop (SIGINT, then SIGKILL after a grace period).
  - Cancellation and typed errors.
- **API:**
  - a command builder: program, args, cwd, env set/remove/scrub; no console window on Windows,
    always;
  - the child runs in its own process tree: a Unix group, or a Windows job;
  - `run(timeout, cancel)` and `run_lines`;
  - `spawn_streaming(sink) -> Handle`, with stdin and kill-on-drop (dv's language servers);
  - an optional global "dies with this app" job (dv's `job.rs`);
  - `Stop::Graceful(grace)` or `Stop::Kill`;
  - a typed `ProcessError`;
  - a small `Override` helper: an env var names the executable, otherwise PATH.
- **Settled while building it:**
  - On Windows a graceful stop is a kill: a windowless process has no console for Ctrl-C.
  - A line longer than 1 MiB (`LINE_MAX`) arrives in pieces, rather than growing without
    bound or being cut.
  - A process that escaped its tree and holds a pipe delays a run by one second after the exit
    at most; the run then returns what it read (dv's git used to fail with a timeout).
  - Unix children don't die with a crashed app: `PR_SET_PDEATHSIG` follows the spawning thread,
    not the process.
  - `run` kills the tree whenever it returns, a panicking line handler included.
- **Stays in the apps:**
  - git policy, `GH_PROMPT_DISABLED`/`DV_GH`, `DV_LSP_<ID>`;
  - dv's backend process trait, which is needed for processes inside WSL;
  - fc's tool discovery (Scoop, Homebrew, Settings) and all yt-dlp argument, classification and
    progress parsing.
- **Removes:**
  - about 600–800 lines from diff, which also gains the race fix and a whole-tree kill when git
    times out;
  - about 450 from fc;
  - nothing from slide, which only runs `chflags`.

### `toolbox_stills`: still images (built; docs/stills.md)
- **Seed:** slide's `slide_media` (about 3k lines):
  - OS decoding (ImageIO on macOS, WIC on Windows, HEIF): colour-managed to sRGB, EXIF
    orientation applied, HEIC and AVIF, and 1.5–6× faster than the `image` crate for JPEG;
  - `sniff.rs` (magic bytes), a superset of fc's;
  - `ThumbCache` (a key over path, size and mtime; atomic writes; pruned to a byte budget).
- **What fc gains:** HEIC, colour management and orientation. fc keeps its YouTube thumbnail and
  cover code.
- **Stays in slide:** `scan.rs`, which scans dropped files, and slide's own values (the grid's
  320 px, the 2 GiB cache budget).
- **Added for fc:** decoding bytes in memory (`Source::Bytes`, `SourceBuf`), `sniff_bytes`,
  `Bgra::into_rgba_image` and `resize_to_fit`. Linux builds with the `image` crate as its only
  decoder, so the workspace and CI build there.
- **Windows rule:** WIC joins a thread to COM's multithreaded apartment, and gpui's Windows
  executor runs on the system thread pool, so an app decodes through the crate's `Decoder`,
  whose threads are its own.
- **Licences:** one package new to the tree, `objc2-image-io` (Zlib OR Apache-2.0 OR MIT, as
  the other objc2 crates); no new licence.

### `gpui_appearance`: light/dark and window materials (gpui; built, docs/appearance.md)
The owner raised this from phase 3 because it shapes how every app looks.
- **Seed:** dv's `dv_ui/src/appearance.rs` (210 lines) and `platform.rs` (285). fc only switches
  between Transparent and Opaque; slide has neither.
- **Light/dark:** follow the system's setting live, with an app override (System / Light /
  Dark), and the automation override scenarios use to pin a theme.
- **Materials:** `MaterialSupport::probe` asks the OS what it can show right now:
  - Windows 11: Mica (build 22621+) and Acrylic (22000+), and only while "Transparency effects"
    is on. Windows 10 and Linux get Opaque.
  - The app gets a material it can apply, plus a sensible fallback.
- **macOS vibrancy is real work, not a copy.** gpui's `Blurred` is an `NSVisualEffectView` with
  its tint and saturation layers stripped. In 0.3.6 its `Selection` material showed no blur at
  all on macOS 27 (dv's PLAN D8), so dv ships Opaque on macOS today. 0.3.11 switched it to
  `UnderWindowBackground`, and the foreground captures (2026-11-09) show that it blurs now, but
  untinted and the same for light, dark, active and inactive. The crate sets up its own plain
  `NSVisualEffectView` through objc2 instead: a chosen material, blending mode and state, behind
  gpui's content, with AppKit's tint kept. It is proved on the owner's Mac with foreground screen
  captures, since background screenshots never show blur.
- **Not taken:** gpui's `App::set_window_appearance` sets the application's appearance, after
  which the system's own setting can no longer be read. The crate sets each window's appearance
  instead.
- The theme kit (§5b) builds on this crate for its colour tokens per appearance and material.

### `toolbox_store`: the apps' SQLite database (built; docs/store.md)
Moved into phase 2 from "after phase 2" (2026-11-09).
- **Why:**
  - slide and fc each carry a GPL copy of Zed's `sqlez`, which isn't published as a crate.
  - Both built the same store layer on top of it, and the copies have drifted: the single
    writer, migrations, the foreign-database guard, backup plus integrity check, and a key-value
    table.
  - On `rusqlite` (0.41, MIT, the same bundled C SQLite underneath), that layer is one MIT crate.
- **What it is:** `Database` (one writer behind a mutex, pooled `query_only` readers, every read
  one snapshot), `Database::open` (guard, `quick_check`, daily and pre-migration backups, WAL,
  migrations), `Schema`, `Backups` (including restore), `Kv`, `OsPath`/`OsPathBuf` and a typed
  `Error`. docs/store.md §3 records which app's design each piece took, and why.
- **Existing databases keep working:** the migrations read and write sqlez's own `migrations`
  table, so steps that already ran aren't run again. Tests build databases exactly as sqlez
  leaves them, in plain SQL. A copy of fc's shipped `v2.db` opened with fc's real steps.
- **Stays in the apps:** their tables, queries and column types; their file names and error
  wording; fc's FTS5 check, restore marker and JSON values.
- **Size of each move (counted 2026-11-09):**
  - slide: about 60 query call sites, about 1 day;
  - fc: about 400, about 2–3 days;
  - mostly mechanical (docs/store.md §4–§6). The PLAN's earlier 250/650 over-counted.
- **Order:** slide first, since it's smaller. Each app first pins `libsqlite3-sys =0.39.1`
  under sqlez, because Cargo allows one SQLite per build.

### Not extracted, and why
- **Video players: not shared; fc stays on libmpv** (decided 2026-11-09).
  - **Why mpv can't serve both:** it breaks slide's no-bundled-binaries rule, costs 220–245 MB
    per pre-rolled slot against alhazen's 104 MB, and can't show a native view and a GPUI frame
    together, which slide's ≤ 1 vsync cuts need.
  - **Why alhazen can't serve fc:** fc tried alhazen behind its own player API (a trial branch,
    since removed). On the Mac it played 4K at 60 fps in one player and in four tiles, but:
    - it has no rotation, which fc needs;
    - it has no keyframe seek: a drag on the seek bar costs 0.6–1.3 s against mpv's 40 ms, and
      an exact 8K seek 2.3–3.9 s against 25–110 ms;
    - it costs 1.3–2× mpv's CPU and 2–3× its memory in four tiles;
    - it lacks speed, frame step and A–B loop.
  - **What stays useful for slide alone:** zero-copy video on macOS (`Window::paint_surface`)
    and, if ever worth a gpui patch, a Windows surface in gpui-pre-windows (whose
    `draw_surfaces` does nothing); slide measured 46–78% of a core per 4K60 stream there.
    Slide's alhazen patches should go upstream either way.
  - **Shareable later, if either player is reworked:** the fake-engine test harness (clock,
    media, simulated engine), which both apps took from folio.
- **WSL:** about 1.9k lines, mostly installing and launching dv's own Linux build. The generic
  layer is small: listing distros, running `/bin/sh` scripts through `wsl.exe`, `\\wsl.localhost`
  path mapping and the sandbox seam. Extract it when a second app needs WSL.
- **Mock executables** (`mock_gh`, `mock_tool`, `mock_lsp`): about 150–200 lines overlap (argv
  patterns, a locked JSONL call log, `times`, exit codes). Extract `toolbox_mock_exe` if a third
  CLI mock appears.
- **open_router:** shaped to each app (see O3).

## 5b. Phase 3: `gpui_theme_kit` (scaffolding only)
Every app ends up building the same scaffolding around its own values. Terrarium adds sprite
accents, hard shadows and pixel icons; those stay in the app, like every app's palette.
The kit carries:
- the mapping from app tokens to gpui-component's `Theme`. Every gpui-kit bump breaks it (0.9
  made the theme structs `#[non_exhaustive]`), so fixing one shared copy absorbs that churn;
- a type-scale trait: dv's `text_spec`, terrarium's `TypeExt`;
- bundled fonts: loading them, plus a startup check that they resolved;
- colour helpers: OKLCH (terrarium) and a contrast-ratio check for tests (dv's contrast floor).

Appearance and window materials have moved up into phase 2 (`gpui_appearance`, §5a).

It might build on gpui-base 0.9's semantic tokens (`ColorTokens`, `TypographyTokens`, …) rather
than our own structs; check that when it's built.

## 6. Later

1. **Semantic targets.** These come from gpui-base's element registry (`test_support`), which can
   only be compiled with `gpui-base/test-support`. That feature turns on `gpui/test-support`,
   which changes the executor and is banned in runner-launched builds.
   - Ask gpui-kit for a feature that compiles only the registry, or patch it.
   - Then add the `role` / `label` / `id` / `path` selectors and `tree` behind a `semantic`
     feature.
   - `automation_target` stays: it works on any element, has stable names, covers sub-ranges and
     costs nothing when automation is off.
2. **Headless executor.** The same scenarios in-process (gpui `TestAppContext` + `TestWindowExt`)
   for scenarios that don't need real-window capabilities:
   - the driver's frame waits become a `FrameClock` trait;
   - an `Executor` trait in the runner;
   - skipping uses `requires` (§4, item 2).
3. **`--trace` contents** and a `tree` diff for each step.
4. **Phase 3 crates, alongside `gpui_theme_kit`:**
   - `gpui_system`: the `system` seam (`open_url` / `reveal` / `pick_folder` / clipboard,
     recorded under automation), with core requests and room for each app's own;
   - `toolbox_app_shell` (no gpui): `data_dir` plus the instance lock, `crash`, `logging`,
     `dock_icon`, the Windows resource `build.rs` helper, and Velopack `installer` / `updates`
     behind a feature;
   - most refined copies: slide and fc.
5. **Not shared:**
   - theme values and keymap tables;
   - the players (§5a);
   - fc's thumbnail and cover code;
   - `mock_openrouter`.
   
   Scripts and CI become a template rather than a crate.

## 7. Risks

- **gpui-kit's pace:** six releases in six weeks, each with breaking changes. Keep the shared
  crates' gpui surface small (driver, capture, guard) so a bump stays cheap. Bump the shared
  repo first and let the apps follow.
- **Cross-repo development is slower:** an automation change used to be one commit in the app.
  The local `[patch]` override covers day-to-day work, and tags make releases deliberate.
- **The `deny_unknown_fields` safety must survive:** the two-pass parse plus the reserved-name
  check keep typos loud.
