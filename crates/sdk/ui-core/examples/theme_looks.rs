//! One view under each surface finish, as PNGs to look at: a real widget
//! view (buttons, switches) and the surface roles, with text on them, over
//! three desktops (window ground, bar, floating panel, control faces at
//! rest, hovered and pressed), so a finish's backdrop pass, the legible
//! range it holds what shows through in, and the theme's response show.
//!
//! `cargo run -p tilo-ui --example theme-looks -- OUT_DIR [THEME.json ...]`
//!
//! Without theme files it shoots the shipped Block, Glass, Pixelated and
//! Liquid Glass presets; each file given is a theme document shot under its
//! own id. Run it on a software adapter (lavapipe) where the GPU is not to
//! be touched.
//!
//! The desks: a bright, photo-like wallpaper of colour; a white page of
//! dark text; a dark page of light text. Text runs under every rim, so a
//! lens's bend shows as bent lines.

use std::path::{Path, PathBuf};

use tilo_ui::backdrop::{BackdropFrame, BackdropRecording, fill_role, layer_scene};
use tilo_ui::chrome::{SelectionState, selection_tier_on};
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::{Preset, Theme, ThemeDocument};
use tilo_ui::tokens::SurfaceRole;
use tilo_ui::widget::{Button, ButtonRole, Column, Row, Toggle};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Rect, RoundedRect};
use vello::peniko::{Color, Fill};

const DESK: (u32, u32) = (720, 420);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let Some(out) = args.next().map(PathBuf::from) else {
        return Err("usage: theme-looks OUT_DIR [THEME.json ...]".into());
    };
    let mut themes: Vec<(String, Theme)> = Vec::new();
    let files: Vec<PathBuf> = args.map(PathBuf::from).collect();
    if files.is_empty() {
        for preset in [
            Preset::Block,
            Preset::Glass,
            Preset::Pixelated,
            Preset::LiquidGlass,
        ] {
            themes.push((preset.id().to_owned(), Theme::preset(preset)));
        }
    }
    for file in files {
        let text = std::fs::read_to_string(&file)
            .map_err(|error| format!("{}: {error}", file.display()))?;
        let document: ThemeDocument = serde_json::from_str(&text)
            .map_err(|error| format!("{}: not a theme document: {error}", file.display()))?;
        // The refusal a person importing the theme reads.
        if let Some(reason) = Theme::refusal(&document) {
            return Err(format!("{}: refused: {reason}", file.display()).into());
        }
        let theme = Theme::from_document(&document)
            .ok_or_else(|| format!("{}: refused", file.display()))?;
        themes.push((document.id.clone(), theme));
    }
    std::fs::create_dir_all(&out)?;
    for (id, theme) in themes {
        let view = out.join(format!("{id}-view.png"));
        let mut widget = view_of();
        tilo_ui::launch::render_widget_png(widget.as_mut(), (420, 220), theme.clone(), &view)?;
        println!("{}", view.display());
        for (name, below) in [
            ("bright", Below::Bright),
            ("white-page", Below::WhitePage),
            ("dark-page", Below::DarkPage),
        ] {
            let desk = out.join(format!("{id}-desk-{name}.png"));
            shoot_desk(&theme, below, &desk);
            println!("{}", desk.display());
        }
    }
    Ok(())
}

/// The view: the four button roles and two switches.
fn view_of() -> Box<dyn tilo_ui::widget::Widget> {
    let buttons = Row::new()
        .with_gap(8.0)
        .push(Box::new(
            Button::new("save", "Save").role(ButtonRole::Primary),
        ))
        .push(Box::new(Button::new("share", "Share")))
        .push(Box::new(
            Button::new("more", "More").role(ButtonRole::Plain),
        ))
        .push(Box::new(
            Button::new("delete", "Delete").role(ButtonRole::Destructive),
        ));
    Box::new(
        Column::new()
            .with_gap(12.0)
            .push(Box::new(buttons))
            .push(Box::new(Toggle::new("wifi", "")))
            .push(Box::new(Toggle::new("bluetooth", ""))),
    )
}

/// What a desk shows below its surfaces.
#[derive(Clone, Copy)]
enum Below {
    /// Bright colour, as a photo is.
    Bright,
    /// A white page of dark text.
    WhitePage,
    /// A dark page of light text.
    DarkPage,
}

/// The surface roles over `below`, with the backdrop pass, and text on the
/// window and the panel in the theme's inks.
fn shoot_desk(theme: &Theme, below: Below, out: &Path) {
    let (width, height) = DESK;
    let mut scene = match below {
        Below::Bright => wallpaper(width, height),
        Below::WhitePage => page(width, height, Color::WHITE, Color::from_rgb8(24, 24, 28)),
        Below::DarkPage => page(
            width,
            height,
            Color::from_rgb8(14, 16, 22),
            Color::from_rgb8(236, 236, 240),
        ),
    };
    let recording = BackdropRecording::begin();
    layer_scene(&scene, 0);
    let colors = &theme.tokens.colors;
    let window_radius = f64::from(theme.tokens.radii.tile);
    // A window, the bar over the desktop, and a floating panel over both.
    let bar = RoundedRect::from_rect(Rect::new(0.0, 0.0, f64::from(width), 32.0), 0.0);
    fill_role(
        &mut scene,
        Affine::IDENTITY,
        &bar,
        SurfaceRole::Bar,
        theme,
        colors.surfaces.base,
    );
    let window = RoundedRect::from_rect(Rect::new(40.0, 56.0, 460.0, 380.0), window_radius);
    fill_role(
        &mut scene,
        Affine::IDENTITY,
        &window,
        SurfaceRole::Window,
        theme,
        colors.surfaces.base,
    );
    // Control faces at rest, hovered and pressed, as the theme's response
    // draws them on the window ground.
    let ground = colors.surfaces.content;
    for (x, state) in [64.0, 160.0, 256.0, 352.0].into_iter().zip([
        SelectionState::new(false, false, false),
        SelectionState::new(false, true, false),
        SelectionState::new(false, true, true),
        SelectionState::new(true, false, false),
    ]) {
        let tier = selection_tier_on(theme, state, ground);
        let face = Rect::new(x, 96.0, x + 80.0, 128.0);
        let centre = face.center();
        let scaled = Affine::scale_about(tier.scale, centre);
        let body = RoundedRect::from_rect(face, f64::from(theme.tokens.radii.control));
        let fill = if tier.fill.components[3] > 0.0 {
            tier.fill
        } else {
            colors.surfaces.raised
        };
        tilo_ui::surface::control_face(&mut scene, scaled, &body, fill, theme);
    }
    let panel = Rect::new(300.0, 160.0, 660.0, 360.0);
    tilo_ui::surface::panel(
        &mut scene,
        Affine::IDENTITY,
        panel,
        f64::from(theme.tokens.radii.panel),
        theme,
    );
    let mut text = TextRenderer::new();
    let mut ink = |words: &str, size: f32, colour: Color, at: (f64, f64)| {
        text.draw_text(&mut scene, words, size, colour, Affine::translate(at), None);
    };
    ink("Wednesday 10:42", 14.0, colors.text_primary, (16.0, 8.0));
    ink("Window title", 16.0, colors.text_primary, (64.0, 66.0));
    ink(
        "Text on the window ground",
        14.0,
        colors.text_primary,
        (64.0, 150.0),
    );
    ink(
        "and its secondary ink",
        14.0,
        colors.text_dim,
        (64.0, 172.0),
    );
    ink("Panel heading", 17.0, colors.text_primary, (324.0, 184.0));
    for (row, line) in [
        "Primary text on the floating panel",
        "Secondary text reads over anything",
    ]
    .iter()
    .enumerate()
    {
        let colour = if row == 0 {
            colors.text_primary
        } else {
            colors.text_dim
        };
        #[allow(clippy::cast_precision_loss, reason = "two rows")]
        let y = 222.0 + row as f64 * 26.0;
        ink(line, 15.0, colour, (324.0, y));
    }
    let frame = BackdropFrame::from_single_scene(recording.finish());
    let mut renderer = OffscreenRenderer::new(width, height);
    renderer.set_backdrop(Some(std::sync::Arc::new(frame)));
    renderer.render_png(&scene, Color::BLACK, out);
}

/// Stripes and discs in strong colours over a bright sky: what a finish
/// shows of what lies below it is plain against them.
fn wallpaper(width: u32, height: u32) -> Scene {
    let mut scene = Scene::new();
    let (w, h) = (f64::from(width), f64::from(height));
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        &vello::peniko::Gradient::new_linear((0.0, 0.0), (0.0, h)).with_stops([
            Color::from_rgb8(150, 200, 250),
            Color::from_rgb8(250, 236, 210),
        ]),
        None,
        &Rect::new(0.0, 0.0, w, h),
    );
    let hues = [
        Color::from_rgb8(230, 90, 70),
        Color::from_rgb8(240, 200, 60),
        Color::from_rgb8(70, 180, 120),
        Color::from_rgb8(70, 130, 230),
        Color::from_rgb8(180, 90, 220),
    ];
    for (step, colour) in (0..18_u32).zip(hues.iter().cycle()) {
        let x = f64::from(step).mul_add(44.0, -40.0);
        scene.fill(
            Fill::NonZero,
            Affine::rotate(0.35),
            *colour,
            None,
            &Rect::new(x, -400.0, x + 18.0, h + 400.0),
        );
    }
    for (step, colour) in (0..5_u32).zip(hues.iter()) {
        let centre = (
            f64::from(step).mul_add(130.0, 120.0),
            f64::from(step % 2).mul_add(-160.0, 300.0),
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            colour.with_alpha(0.9),
            None,
            &Circle::new(centre, 46.0),
        );
    }
    scene
}

/// A page of `paper` across the desk, lines of `ink` text on it end to end,
/// so every surface's rim crosses them.
fn page(width: u32, height: u32, paper: Color, ink: Color) -> Scene {
    let mut scene = Scene::new();
    let mut text = TextRenderer::new();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        paper,
        None,
        &Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
    );
    let line =
        "The lens bends the lines below along its rim, and the text on the glass stays legible.";
    for row in 0..height / 20 {
        text.draw_text(
            &mut scene,
            line,
            14.0,
            ink,
            Affine::translate((6.0, f64::from(row).mul_add(20.0, 2.0))),
            None,
        );
    }
    scene
}
