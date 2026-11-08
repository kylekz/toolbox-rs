# Light/dark and window materials (`gpui_appearance`)

`gpui_appearance` decides whether a window is light or dark and what shows behind its content, and
applies both to every window of the app. It replaces dv's `dv_ui/src/appearance.rs` and
`platform.rs`, and gives fc and slide what they lack today (PLAN §5a).

It does four things:

- **Follows the system's light/dark setting live**, unless the app says otherwise.
- **Takes the app's choice:** a [`Mode`](#light-and-dark) (System / Light / Dark) and a
  [`Material`](#materials), usually from the app's settings file.
- **Lets automation pin light or dark** without touching the app's settings, so a scenario
  looks the same on every machine.
- **Asks the OS which materials it can show right now** (`MaterialSupport::probe`), and falls
  back to Opaque when it can't show the one asked for.

The app's theme reads the result (light or dark, and the material) to pick its colours. The crate
applies the same result to each window: the frame's light/dark, Mica or Acrylic on Windows 11,
and real vibrancy on macOS through the crate's own `NSVisualEffectView`.

```toml
[dependencies]
gpui_appearance = { git = "https://github.com/kylekz/toolbox-rs", tag = "v0.4.0", features = ["serde"] }
```

The `serde` feature derives `Serialize`/`Deserialize` for `Appearance`, `Mode`, `Material` and the
vibrancy types, spelled exactly as their `key()`s (`"system"`, `"mica"`, `"under-window-background"`),
so a settings file can hold them directly. dv's existing settings files keep parsing.

## The API in one page

```rust
use gpui_appearance::{AppearanceState, Backing, MaterialSupport, Mode, Material, Preferences, Setup, Vibrancy};

// 1. Startup, after gpui_component::init and before the first window.
gpui_appearance::init(
    Setup {
        support: MaterialSupport::probe(),
        preferences: Preferences { mode: settings.mode, material: settings.material },
        vibrancy: Vibrancy::default(), // the app's design choice, see "Vibrancy"
    },
    cx,
);

// 2. The theme follows: install it now and on every change.
theme::install(gpui_appearance::resolved(cx), cx);
cx.observe_global::<AppearanceState>(|cx| theme::install(gpui_appearance::resolved(cx), cx))
    .detach();

// 3. The app's settings changed.
gpui_appearance::set_preferences(Preferences { mode, material }, cx);

// 4. Each window: open with the resolved material...
let options = WindowOptions {
    window_background: gpui_appearance::window_background(Backing::Solid, cx),
    ..Default::default()
};
// ...and, in its root view's constructor, track it for its lifetime.
struct Shell { appearance: gpui_appearance::WindowTracker, /* ... */ }
let appearance = gpui_appearance::track_window(Backing::Solid, window, cx);
```

| Item | What it is |
|---|---|
| `Appearance` | `Light` or `Dark`. `key()`/`from_key()`, `of_window(WindowAppearance)`. |
| `Mode` | The app's light/dark setting: `System` (the default) / `Light` / `Dark`. `resolve(system)`. |
| `Material` | `Mica`, `Acrylic`, `Vibrancy`, `Opaque`. `key()`, `label()`, `is_translucent()`, `platform_default()`, `window_background(backing)`. |
| `MaterialSupport` | What the OS can show now: `probe()`, `supports()`, `available()`, `resolve(wanted)`. `OPAQUE_ONLY` and `ALL` for tests. |
| `Vibrancy` | macOS's view style: `VibrancyMaterial` (14 AppKit materials), `Blending`, `VibrancyState`. |
| `Setup`, `init` | Startup. |
| `Preferences` | What the app asks for: `mode` and `material`. |
| `AppearanceState`, `state(cx)` | The global: `system()`, `support()`, `preferences()`, `pinned()`, `vibrancy()`, `forced()`, `resolved()`. Observe it with `cx.observe_global::<AppearanceState>`. |
| `Resolved`, `resolved(cx)` | What windows show now: `appearance` and `material`. |
| `set_preferences`, `set_vibrancy` | The app's changes. |
| `pin(Option<Appearance>)` | The automation override. |
| `set_system`, `set_support`, `refresh_support` | Stand-ins for the OS (tests, scenarios), and re-probing. |
| `Backing` | What Opaque means for one window: `Solid` (gpui's `Opaque`) or `SeeThrough` (gpui's `Transparent`, for fc's video hole on Windows). |
| `window_background(backing, cx)` | `WindowOptions::window_background` for a window about to open. |
| `track_window(backing, window, cx)` → `WindowTracker` | Keeps one window in step; `applied()` says what was applied. |
| `native(window)` → `Option<Native>` | What AppKit reports for the window (macOS; `None` elsewhere): its own appearance, `isOpaque`, and the crate's effect view with its style. |

The global changes, and notifies its observers, only when something in it actually changes.

## How the pieces resolve

```
appearance = pin                      if automation pinned one
           | Light / Dark             if the mode is Light / Dark
           | the system's             if the mode is System
material   = preferences.material     if the platform supports it now
           | Opaque                   otherwise
```

An unsupported material always resolves to Opaque, never to another translucent material. Each
app tunes its surface colours per material, and Opaque is the one every app designs to look
finished on its own (dv's DESIGN §3). The material asked for is kept, so it comes back when the
platform can show it again (Transparency effects switched back on).

The app narrows its preferences itself before handing them over, using whatever the crate doesn't
know about. A dv palette with no light variant (Dracula) asks for `Mode::Dark`, and an opaque-only
palette (Claude, Dracula) asks for `Material::Opaque`.

## Light and dark

- **The system's setting** is read at `init` from `App::window_appearance()` (the application's
  appearance). Each tracked window reports changes through gpui's `observe_window_appearance`, and
  the tracker reads the *application's* appearance there too: a window whose own appearance is
  forced reports that forced value, not the system's.
- **Applying it:**
  - **macOS:** the window's own `NSAppearance` is set to aqua or darkAqua, or to none in
    System mode, where the window follows the system live. This colours the title bar and traffic
    lights, and tints the vibrancy material. gpui's `App::set_window_appearance` isn't used: it
    sets `NSApplication.appearance`, after which `App::window_appearance` reports the forced value
    and the system's own setting can't be followed any more.
  - **Windows:** `DWMWA_USE_IMMERSIVE_DARK_MODE`, which colours the frame and tints Mica/Acrylic.
    gpui resets it to the system's value right after a system change, so the tracker puts the
    app's value back on the next frame.
  - **Linux:** nothing at the window level; the app's theme does it all.

## Materials

| Platform | Can show | Condition |
|---|---|---|
| Windows 11, build 22621+ | Mica, Acrylic | "Transparency effects" on (`HKCU\…\Themes\Personalize\EnableTransparency`) |
| Windows 11, builds 22000–22620 | Acrylic | the same switch (gpui sets Mica through `DWMWA_SYSTEMBACKDROP_TYPE`, which these builds lack) |
| Windows 10 | Opaque only | |
| macOS | Vibrancy | Accessibility › Display › "Reduce transparency" off |
| Linux | Opaque only | |

`Material::platform_default()` is Mica on Windows, Vibrancy on macOS and Opaque on Linux.
Acrylic is never the default, because it can lag while the window is dragged on some Windows
builds (dv's PLAN D8).

Applying a material clears the previous one first. gpui's `set_background_appearance` only adds
its own effect, so going from Acrylic to Mica would otherwise leave both on. The tracker re-probes
the support whenever a window is activated, because the switches can flip while the app is in the
background.

**What the app must paint:** where a translucent material shows, the app paints nothing, or a
partly transparent tint. That includes gpui-component's `Root`, which paints
`theme.colors.background` behind everything; dv maps it to transparent over a material.

**Acrylic comes untinted.** gpui sets Acrylic through the accent policy with a fully transparent
gradient colour, so an unpainted area shows the blur at full saturation (the demo's left strip
on Windows). Mica brings its own tint, and so does macOS's vibrancy. Over Acrylic, an app's
surface tokens carry the tint themselves, as dv's `acrylic` values do.

### Vibrancy (macOS)

gpui's own `Blurred` is an `NSVisualEffectView` subclass with AppKit's tint (`CAChameleonLayer`)
and saturation filter stripped out. Up to gpui-pre 0.3.6 it showed no blur at all on macOS 27
(dv's PLAN D8). gpui-pre 0.3.11 switched its material to `UnderWindowBackground`, and the
foreground captures on the owner's Mac (macOS 27, 2026-11-09) show that it now blurs. It is still
a bare blur, though: untinted, fully saturated, and the same whether the window is light or dark,
active or not.

The crate uses a plain `NSVisualEffectView` of its own instead and leaves AppKit's layers alone,
so the window looks like any other macOS window:
- the material is tinted for light or dark;
- it is AppKit's material of the app's choice;
- it goes flat when the window is inactive.

It also doesn't depend on gpui's layer surgery, which looks layers up by class name and broke
once already.

- **Where the view goes:** it is the bottom subview of the window's content view, the view gpui
  adds its `GPUIView` to (`addSubview:positioned:NSWindowBelow relativeTo:nil`). It is sized to
  the content view and resizes with it, exactly as gpui inserts its own blurred view. gpui's view
  stays above it and covers the window, so it still gets every event.
- **The window is gpui's `Transparent`:** not opaque, a near-clear background colour (gpui avoids
  `clearColor` because of a shadow bug), and a transparent Metal layer. So whatever gpui leaves
  unpainted shows the effect view.
- **The view is found again by its `identifier`** (`toolbox-rs.gpui_appearance.vibrancy`), so the
  crate keeps no per-window state. Changing the style restyles the same view; any other material
  removes it.
- **Every AppKit call needs the main thread and a real window.** On gpui's test platform they do
  nothing, so unit tests can track windows.

The style is the app's design decision, set at `init` and changeable with `set_vibrancy`:

| Field | Values | Default |
|---|---|---|
| `material` | `titlebar`, `selection`, `menu`, `popover`, `sidebar`, `header-view`, `sheet`, `window-background`, `hud-window`, `full-screen-ui`, `tool-tip`, `content-background`, `under-window-background`, `under-page-background` | `under-window-background` |
| `blending` | `behind-window` (the desktop and other windows), `within-window` (the window's own content under the view: here only the tint, since gpui draws above it) | `behind-window` |
| `state` | `follows-window` (live while active, flat while inactive, as macOS does), `active`, `inactive` | `follows-window` |

What the captures showed on macOS 27: `under-window-background` and `sidebar` look the same, a
strong blur with a dark or light tint. `hud-window` blurs less and keeps more colour.

## Theme tokens: appearance × material

The crate holds no colours. Each app keeps its own palette and picks its tokens from
`resolved(cx)`:

```rust
fn install(resolved: gpui_appearance::Resolved, cx: &mut App) {
    let colors = Colors::new(resolved.appearance, resolved.material);
    // ...map onto gpui-component's Theme as today; Root's background is transparent over a
    // translucent material and the solid `chrome` colour over Opaque.
}
```

The pattern is dv's (DESIGN §1, §3):
- every *surface* token has one value per material, more tinted over a material and solid over
  Opaque;
- *solid* tokens (text, accents) depend on the appearance alone;
- popovers with no scrim under them stay solid whatever the material.

With `Material::Vibrancy` added, an app that has per-material surface values needs a vibrancy
column. Acrylic's values are a sound start, since both blur whatever is behind the window. Tune
them against the foreground captures.

## Automation

The crate has no automation dependency. Each app exposes it through its own `AutomationHost`:

- **`state`:** report `state(cx)` and `resolved(cx)`, and for a window `tracker.applied()`
  and, on macOS, `native(window)`. The harness's keys are a good template (`materials.rs`,
  `app_state` and `window_state`).
  - **O4:** dv keeps its existing keys (`settings.resolved_mode`, `settings.available_materials`,
    `shell.window_material`, …), so its 42 scenarios keep passing.
- **The pin:** a command calling `pin(Some(Appearance::Dark))` lets a scenario pin a theme
  without writing the app's settings file; `pin(None)` lifts it.
- **Stand-ins:**
  - `set_system` stands in for the OS switching light/dark, which a scenario can't do.
  - `set_support` stands in for what the machine can show (Transparency effects off, Reduce
    transparency on); `refresh_support` asks the OS again.

**What a background run shows:** a background window is never active. So with the default
`follows-window` state, vibrancy shows flat, and a `window` screenshot never shows blur anyway.
Assert on `applied` and `native`, and look at blur only in a foreground run with `source:
screen` captures.

## Proving it

- **Unit tests** (`cargo test -p gpui_appearance`, with and without `--features serde`):
  - the Windows build and transparency rules, macOS's Reduce transparency, and the fallback;
  - resolution, the pin, and notifications only on real change;
  - a tracked window following every change;
  - the AppKit raw values;
  - the serde spellings.
- **`crates/automation_harness/tests/harness_appearance.jsonl`** (in `script/check`). The
  harness's `open_materials` opens a demo window over a backdrop of saturated stripes; the
  commands `set_appearance`, `pin_appearance`, `set_system_appearance` and
  `set_material_support` drive it. The scenario asserts:
  - the probed support;
  - the fallback;
  - System / Light / Dark, and the pin beating both;
  - on macOS, what AppKit reads back: the window's own appearance, `isOpaque`, the effect view
    present or gone, and its style after every change.

  On Windows and Linux it asserts what the crate applied, with Mica and Acrylic stood in for.
- **`crates/automation_harness/tests/foreground/materials_foreground.jsonl`:** the look, in the
  foreground with screen captures. It takes the focus for about 6 seconds, so it runs only by
  hand:

  ```bash
  cargo build -p automation_harness
  target/debug/harness-e2e --out target/e2e-materials crates/automation_harness/tests/foreground/materials_foreground.jsonl
  ```

  On macOS it captures Opaque, three vibrancy materials dark and light, and gpui's own `Blurred`
  for comparison (`gpui_background`). Checked on the owner's Mac on 2026-11-09.
- **Windows:** CI checks the Windows code with clippy and the unit tests (`windows-2025`,
  `script/check --fast`), including the probe against the runner's own build. The scenarios also
  ran on the owner's PC (Windows 11 build 26300, 2026-11-09), all passing.
  - **Transparency effects on:** available `mica, acrylic, opaque`. Mica showed its tint over
    the wallpaper, and not the windows behind (as Mica does). Acrylic blurred the backdrop's
    stripes under the demo's tint, dark and light.
  - **Transparency effects off:** available `opaque`, and every capture was Opaque.
  - **The frame** matched each capture's light/dark.

  **Open, not the crate's:** every Windows screen capture, Opaque ones included, shows a 1–2 px
  strip of what is behind the window along its top edge. Either `gpui_automation`'s `screen`
  rectangle starts above the client area, or gpui's top resize border isn't covered by the
  window's fill. To look into with the next Windows automation work.

## Adopting it

The app migrations happen later, one session per app, at the release tag. For each app: which
files and call sites change, how its theme tokens pick up appearance × material, and what it
gains. Paths are the apps' as of 2026-11-09 (dv `947a86a`, fc `50a230b`, slide `0560027`).

### dv

dv's copy is the seed, so dv mostly deletes code.

**Files and call sites:**
- **`crates/dv_ui/src/platform.rs`: delete it.** `MaterialSupport`, `WINDOWS_11_BUILD`,
  `MICA_BUILD`, `apply_window_material`, `apply_window_appearance` and the `windows` module are
  all in the crate now. Drop the `windows-sys` and `raw-window-handle` dependencies from
  `dv_ui/Cargo.toml` if nothing else uses them.
- **`crates/dv_ui/src/theme/mod.rs`:**
  - replace its `Appearance` and `Material` with `pub use gpui_appearance::{Appearance,
    Material};`, and its `Mode` (in `services/settings.rs`) with `gpui_appearance::Mode`. Enable
    the `serde` feature; the spellings are the same, so `settings.json` keeps parsing;
  - `Material` gains `Vibrancy`, so `Material::pick` and `Surface` need a `vibrancy` value per
    surface token (`tokens.rs`); start from `acrylic`;
  - `Material::window_background()` becomes `material.window_background(Backing::Solid)`;
  - `Material::platform_default()` is the crate's, which is **Vibrancy on macOS**. Moving dv's
    macOS default from Opaque to Vibrancy is a DESIGN decision (D8, DESIGN §3). Until it's made,
    keep `SettingsFile::default()` on `Material::Opaque` on macOS;
  - `Appearance::of_window` and `key` are the crate's; `is_light()` becomes `!is_dark()`.
- **`crates/dv_ui/src/appearance.rs`: shrink it to dv's own narrowing.**
  - `init` calls `gpui_appearance::init` with `support: MaterialSupport::probe()` and the
    settings' preferences, narrowed by the palette:
    - `mode`: `Mode::Dark` when `!palette.has_light()`, else the setting;
    - `material`: `palette.material(settings.material())`.
  - The settings subscription calls `set_preferences` with the same narrowing.
  - An `observe_global::<AppearanceState>` installs the theme.
  - `resolved(cx)` keeps its `Resolved { palette, appearance, material }`, built from the
    crate's `resolved(cx)` and the palette.
  - `support`, `system`, `set_support` and `refresh_support` become the crate's;
    `system_changed` goes away (the tracker does it).
- **`crates/dv_ui/src/shell/mod.rs`:**
  - `Shell::new` keeps a `gpui_appearance::track_window(Backing::Solid, window, cx)`;
  - delete the `observe_window_appearance` and `apply_window_theme` blocks and the
    `observe_global_in::<DvTheme>` material part (keep its `cx.notify()`), and the activation
    hook's `refresh_support`;
  - `applied_material()` becomes `self.appearance.applied().material`;
  - `window_options` uses `gpui_appearance::window_background(Backing::Solid, cx)`.
- **`crates/dv/src/app.rs:146`, `crates/dv/src/automation/tests.rs:39` and
  `crates/dv_ui/src/test_support.rs:51`:** pass `MaterialSupport::probe()` / `ALL` into the new
  `init`.
- **`crates/dv/src/automation/state.rs`:** read `gpui_appearance::state(cx)`; keep every key.
  Add `"material": "vibrancy"` to `set_setting`'s accepted values.
- **Scenarios:** `shell_materials_macos.jsonl` asserts `available_materials == ["opaque"]` on
  macOS and becomes `["vibrancy", "opaque"]`. That is the one intended change (O4 allows it,
  since it is a behaviour change, not a format one). `setup/appearance.rs`'s material picker
  lists `support.available()`, so Vibrancy appears on macOS by itself.

**What dv gains:** real vibrancy on macOS, once DESIGN has tuned its tokens; Reduce transparency
respected; and the fix stays in the toolbox.

### fc

fc is light only. On Windows it keeps its window see-through for mpv's child window.

**Files and call sites:**
- **`crates/fc_ui/src/lib.rs` (`init`):** call `gpui_appearance::init` after `theme::init`, with
  `Preferences { mode: Mode::Light, material: Material::Opaque }`, until fc's DESIGN says
  otherwise.
- **`crates/fc_ui/src/shell/frame.rs` (`window_options`):** replace the `if cfg!(windows)
  { Transparent } else { Opaque }` with `gpui_appearance::window_background(backing(), cx)`,
  where `backing()` is `Backing::SeeThrough` on Windows and `Backing::Solid` elsewhere. Keep the
  doc comment on why.
- **`crates/fc_ui/src/shell/mod.rs` (`Shell::new`):** keep a `track_window(backing(), window,
  cx)`. `Root`'s transparent background on Windows (`open_main_window`) stays.
- **`crates/fc/src/automation/mod.rs`:** add a `pin_appearance` command and an `appearance` part
  in `state`, if scenarios want them.
- **Theme (`crates/fc_ui/src/theme/mod.rs`):** nothing while fc stays light on Opaque. When fc
  adopts a material (a translucent sidebar over Mica or vibrancy), `FcColors::cover` and
  `surface` get one value per material, chosen from `resolved(cx).material` in an
  `observe_global::<AppearanceState>`, and `Root`'s background maps to transparent over a
  material.

**What fc gains:**
- the native frame matches fc's light UI even when the system is dark: the window border on
  Windows (`DWMWA_USE_IMMERSIVE_DARK_MODE`; fc draws its own caption buttons), the border and
  edges on macOS (the window's `NSAppearance`). Today it follows the system;
- its Transparent/Opaque switch moves into one tested place;
- Mica or vibrancy become one preference away.

### slide

slide is dark only ("Darkroom") and has neither feature today.

**Files and call sites:**
- **`crates/slide_ui/src/lib.rs` (`init`, after `theme::init`):** call `gpui_appearance::init`
  with `Preferences { mode: Mode::Dark, material: Material::Opaque }`.
- **Windows:** `crates/slide_ui/src/shell.rs` (`open_main_window`),
  `crates/slide_ui/src/settings.rs` (`open_window`) and `crates/slide_ui/src/playback/mod.rs`:
  - set `window_background: gpui_appearance::window_background(Backing::Solid, cx)` on the main
    window and the Settings window;
  - keep a `track_window` in each window's root view (`Shell`, `SettingsWindow`,
    `PlaybackWindow`);
  - the playback window stays Opaque whatever the preferences, because video sits there, but it
    should still track for the dark frame.
- **`crates/slide/src/automation.rs`:** the same optional `pin_appearance` and `appearance` state
  as fc.
- **Theme (`crates/slide_ui/src/theme.rs`):** nothing while slide is dark on Opaque. A vibrant
  sidebar later means `bg_sidebar` gets a vibrancy value with Root's background transparent.

**What slide gains:** a native frame (border and edges) that matches its dark UI even when the
system is light; today it follows the system. Vibrancy becomes a later design option.
