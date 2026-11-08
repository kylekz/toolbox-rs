//! The materials demo: a window that `gpui_appearance` tracks, and the commands and `state` that
//! prove the crate end to end (`tests/harness_appearance.jsonl`; the foreground look,
//! `tests/foreground/materials_foreground.jsonl`; docs/appearance.md "Proving it").
//!
//! The window opens in front of a backdrop window of saturated stripes and large text, so a
//! translucent material has something sharp behind it to blur. Its left strip paints nothing, so
//! the material shows as the OS draws it; the panel on the right is tinted as an app's surfaces
//! would be (more over a material, solid over Opaque), so text stays readable on both.

use anyhow::{Context as _, Result, anyhow, bail};
use gpui::{
    App, AppContext as _, Bounds, Context, Entity, Hsla, IntoElement, ParentElement as _, Pixels,
    Render, Styled as _, TitlebarOptions, Window, WindowBackgroundAppearance, WindowBounds,
    WindowHandle, WindowOptions, div, point, px, rgb, rgba, size,
};
use gpui_appearance::{
    Appearance, Applied, Backing, Blending, Material, MaterialSupport, Mode, Native, Preferences,
    Vibrancy, VibrancyMaterial, VibrancyState, WindowTracker,
};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};
use serde_json::{Value, json};

/// The demo window's name for `window` arguments.
pub const WINDOW: &str = "materials";
/// The backdrop behind it.
const BACKDROP: &str = "backdrop";
/// The backdrop's stripes: saturated, so blur shows as colours bleeding into each other.
const STRIPES: [u32; 8] = [
    0xff3b30, 0xffcc00, 0x34c759, 0x007aff, 0xaf52de, 0xffffff, 0x000000, 0xff9500,
];

/// The window behind the demo: stripes under large text, all hard edges.
struct Backdrop;

impl Render for Backdrop {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .size_full()
            .font_family(cx.theme().font_family.clone())
            .child(
                h_flex()
                    .size_full()
                    .children(STRIPES.map(|color| div().flex_1().h_full().bg(rgb(color)))),
            )
            .child(
                v_flex()
                    .absolute()
                    .inset_0()
                    .items_center()
                    .justify_center()
                    .text_size(px(72.))
                    .text_color(rgb(0x000000))
                    .child("BACKDROP")
                    .child(div().text_color(rgb(0xffffff)).child("sharp edges")),
            )
    }
}

pub struct MaterialsView {
    tracker: WindowTracker,
    /// gpui's own background, set past the crate by `gpui_background`; the view then paints as
    /// over a material so that background can show.
    gpui_background: Option<WindowBackgroundAppearance>,
}

impl MaterialsView {
    pub fn applied(&self) -> Applied {
        self.tracker.applied()
    }
}

impl Render for MaterialsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let applied = self.tracker.applied();
        let translucent = self.gpui_background.is_some() || applied.material.is_translucent();
        let material: String = match self.gpui_background {
            Some(background) => format!("gpui {background:?}"),
            None => applied.material.label().into(),
        };
        let dark = applied.appearance.is_dark();
        let ink: Hsla = if dark {
            rgba(0xf2f2f7ff)
        } else {
            rgba(0x1c1c1eff)
        }
        .into();
        let ink_2: Hsla = if dark {
            rgba(0xf2f2f7a0)
        } else {
            rgba(0x1c1c1ea0)
        }
        .into();
        // A translucent tint over a material, a solid surface over Opaque.
        let panel: Hsla = match (translucent, dark) {
            (true, true) => rgba(0x1c1c1e8c),
            (true, false) => rgba(0xffffff8c),
            (false, true) => rgba(0x1c1c1eff),
            (false, false) => rgba(0xf5f5f7ff),
        }
        .into();
        let strip: Hsla = if translucent {
            gpui::transparent_black()
        } else if dark {
            rgba(0x2c2c2eff).into()
        } else {
            rgba(0xe5e5eaff).into()
        };
        let vibrancy = applied
            .vibrancy
            .map(|vibrancy| {
                format!(
                    "{} · {} · {}",
                    vibrancy.material.key(),
                    vibrancy.blending.key(),
                    vibrancy.state.key()
                )
            })
            .unwrap_or_else(|| "none".into());
        let line = |label: &'static str, value: String| {
            h_flex()
                .gap_2()
                .child(div().w(px(110.)).text_color(ink_2).child(label))
                .child(div().text_color(ink).child(value))
        };
        h_flex()
            .size_full()
            .font_family(cx.theme().font_family.clone())
            .text_size(px(14.))
            .child(
                v_flex()
                    .flex_none()
                    .w(px(180.))
                    .h_full()
                    .bg(strip)
                    .pt(px(44.))
                    .px_4()
                    .gap_2()
                    .text_color(ink)
                    .child("Material")
                    .child(div().text_color(ink_2).child(material)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .bg(panel)
                    .pt(px(44.))
                    .px_5()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(20.))
                            .text_color(ink)
                            .child("gpui_appearance"),
                    )
                    .child(line("appearance", applied.appearance.key().into()))
                    .child(line(
                        "forced",
                        applied
                            .forced
                            .map_or("follows system", Appearance::key)
                            .into(),
                    ))
                    .child(line("material", applied.material.key().into()))
                    .child(line("vibrancy", vibrancy)),
            )
    }
}

/// Opens the backdrop and, in front of it, the demo over the top-left of `over` (the main
/// window's bounds), tracked by `gpui_appearance`, and registers it as [`WINDOW`].
pub fn open(
    over: Bounds<Pixels>,
    cx: &mut App,
) -> Result<(WindowHandle<MaterialsView>, Entity<MaterialsView>)> {
    let backdrop = gpui_automation::open_window(
        cx,
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                over.origin + point(px(24.), px(24.)),
                size(px(720.), px(480.)),
            ))),
            titlebar: None,
            ..Default::default()
        },
        |_, cx| cx.new(|_| Backdrop),
    )?;
    gpui_automation::register_window(BACKDROP, backdrop.into(), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            over.origin + point(px(64.), px(64.)),
            size(px(640.), px(400.)),
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some("Materials".into()),
            appears_transparent: true,
            traffic_light_position: None,
        }),
        window_background: gpui_appearance::window_background(Backing::Solid, cx),
        ..Default::default()
    };
    let mut view = None;
    let handle = gpui_automation::open_window(cx, options, |window, cx| {
        let root = cx.new(|cx| MaterialsView {
            tracker: gpui_appearance::track_window(Backing::Solid, window, cx),
            gpui_background: None,
        });
        view = Some(root.clone());
        root
    })?;
    gpui_automation::register_window(WINDOW, handle.into(), cx);
    Ok((handle, view.context("the materials view was not created")?))
}

/// `state` for the demo window: what the crate applied and what AppKit reports back.
pub fn window_state(view: &MaterialsView, window: &Window) -> Value {
    json!({
        "view": "materials",
        "applied": applied_json(view.applied()),
        "native": gpui_appearance::native(window).map(native_json),
    })
}

/// `state.appearance` of the main window: the app-wide state.
pub fn app_state(cx: &App) -> Value {
    let state = gpui_appearance::state(cx);
    let resolved = state.resolved();
    let preferences = state.preferences();
    json!({
        "system": state.system().key(),
        "mode": preferences.mode.key(),
        "material": preferences.material.key(),
        "pinned": state.pinned().map(Appearance::key),
        "vibrancy": vibrancy_json(state.vibrancy()),
        "available": state.support().available().into_iter().map(Material::key).collect::<Vec<_>>(),
        "resolved": {
            "appearance": resolved.appearance.key(),
            "material": resolved.material.key(),
        },
    })
}

/// The module's commands other than `open_materials`, which the harness owns; `None` for any
/// other command.
pub fn command(cmd: &str, args: &Value, cx: &mut App) -> Option<Result<Value>> {
    let result = match cmd {
        "set_appearance" => set_appearance(args, cx),
        "pin_appearance" => {
            optional_appearance(args).map(|appearance| gpui_appearance::pin(appearance, cx))
        }
        "set_system_appearance" => optional_appearance(args).and_then(|appearance| {
            gpui_appearance::set_system(
                appearance.context("set_system_appearance needs \"light\" or \"dark\"")?,
                cx,
            );
            Ok(())
        }),
        "set_material_support" => set_material_support(args, cx),
        _ => return None,
    };
    Some(result.map(|()| json!({"appearance": app_state(cx)})))
}

/// `set_appearance {mode?, material?, vibrancy?: {material?, blending?, state?}}`: what is given
/// replaces the current value.
fn set_appearance(args: &Value, cx: &mut App) -> Result<()> {
    let state = *gpui_appearance::state(cx);
    let mut preferences: Preferences = state.preferences();
    if let Some(mode) = string(args, "mode")? {
        preferences.mode = Mode::from_key(mode)
            .ok_or_else(|| anyhow!("mode must be system, light or dark, not {mode:?}"))?;
    }
    if let Some(material) = string(args, "material")? {
        preferences.material = Material::from_key(material).ok_or_else(|| {
            anyhow!("material must be mica, acrylic, vibrancy or opaque, not {material:?}")
        })?;
    }
    let mut vibrancy: Vibrancy = state.vibrancy();
    if let Some(style) = args.get("vibrancy") {
        if let Some(material) = string(style, "material")? {
            vibrancy.material = VibrancyMaterial::from_key(material)
                .ok_or_else(|| anyhow!("unknown vibrancy material {material:?}"))?;
        }
        if let Some(blending) = string(style, "blending")? {
            vibrancy.blending = Blending::from_key(blending)
                .ok_or_else(|| anyhow!("unknown vibrancy blending {blending:?}"))?;
        }
        if let Some(each) = string(style, "state")? {
            vibrancy.state = VibrancyState::from_key(each)
                .ok_or_else(|| anyhow!("unknown vibrancy state {each:?}"))?;
        }
    }
    gpui_appearance::set_preferences(preferences, cx);
    gpui_appearance::set_vibrancy(vibrancy, cx);
    Ok(())
}

/// `gpui_background {"background": "blurred" | "transparent"}`, sent to the demo's `window`, sets
/// gpui's own background past the crate, to compare gpui's `Blurred` with the crate's vibrancy.
/// Ask the crate for Opaque first, so its own view is out of the way. It holds until the window
/// closes.
pub fn gpui_background(
    view: &Entity<MaterialsView>,
    args: &Value,
    window: &Window,
    cx: &mut App,
) -> Result<Value> {
    let background = match string(args, "background")? {
        Some("blurred") => WindowBackgroundAppearance::Blurred,
        Some("transparent") => WindowBackgroundAppearance::Transparent,
        other => bail!("background must be blurred or transparent, not {other:?}"),
    };
    window.set_background_appearance(background);
    view.update(cx, |view, cx| {
        view.gpui_background = Some(background);
        cx.notify();
    });
    Ok(json!({"gpui_background": format!("{background:?}")}))
}

/// `set_material_support {"probe": true}` asks the OS again; `{mica, acrylic, vibrancy}` stands
/// in for it (each `false` when left out).
fn set_material_support(args: &Value, cx: &mut App) -> Result<()> {
    if args.get("probe").and_then(Value::as_bool) == Some(true) {
        gpui_appearance::refresh_support(cx);
        return Ok(());
    }
    let flag = |name: &str| -> Result<bool> {
        match args.get(name) {
            None => Ok(false),
            Some(value) => value
                .as_bool()
                .with_context(|| format!("set_material_support's {name:?} must be a boolean")),
        }
    };
    gpui_appearance::set_support(
        MaterialSupport {
            mica: flag("mica")?,
            acrylic: flag("acrylic")?,
            vibrancy: flag("vibrancy")?,
        },
        cx,
    );
    Ok(())
}

/// `{"appearance": "light" | "dark" | null}`.
fn optional_appearance(args: &Value) -> Result<Option<Appearance>> {
    match args.get("appearance") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(key)) => Appearance::from_key(key)
            .map(Some)
            .ok_or_else(|| anyhow!("appearance must be light, dark or null, not {key:?}")),
        Some(other) => bail!("appearance must be light, dark or null, not {other}"),
    }
}

fn string<'a>(args: &'a Value, name: &str) -> Result<Option<&'a str>> {
    match args.get(name) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .with_context(|| format!("{name:?} must be a string")),
    }
}

fn applied_json(applied: Applied) -> Value {
    json!({
        "appearance": applied.appearance.key(),
        "forced": applied.forced.map(Appearance::key),
        "material": applied.material.key(),
        "vibrancy": applied.vibrancy.map(vibrancy_json),
    })
}

fn native_json(native: Native) -> Value {
    json!({
        "forced": native.forced.map(Appearance::key),
        "opaque": native.opaque,
        "vibrancy_view": native.vibrancy_view,
        "vibrancy": native.vibrancy.map(vibrancy_json),
    })
}

fn vibrancy_json(vibrancy: Vibrancy) -> Value {
    json!({
        "material": vibrancy.material.key(),
        "blending": vibrancy.blending.key(),
        "state": vibrancy.state.key(),
    })
}
