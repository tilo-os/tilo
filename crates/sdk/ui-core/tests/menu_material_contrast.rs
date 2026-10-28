//! A menu keeps its text contrast whatever is behind it. Each case renders
//! a real context menu, with enabled and disabled rows, in the blurred
//! floating material over a white, a black and a busy backdrop, in the dark
//! and the light theme, through the same backdrop pass a window uses. It
//! measures each row's label against the material under it, and every
//! pixel of the material against the inks a panel draws.
//!
//! A case that cannot observe its subject fails: there is no adapter skip.
//! Set `TILO_UI_SCREENSHOT_DIR=<path>` to dump every rendered PNG.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::arithmetic_side_effects,
    clippy::many_single_char_names,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::Arc;

use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect, Shape};
use vello::peniko::{Color, Fill};

use tilo_ui::backdrop::{BackdropFrame, BackdropRecording, layer_scene};
use tilo_ui::color::contrast_ratio;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::tokens::colors::{ON_FILL_MIN_CONTRAST, SMALL_TEXT_MIN_CONTRAST};
use tilo_ui::widget::Widget;
use tilo_ui::widget::context_menu::{ContextMenu, MenuItem};

const CANVAS_W: u32 = 480;
const CANVAS_H: u32 = 600;

/// Body text's floor, which every label must clear on the material.
const BODY_FLOOR: f64 = ON_FILL_MIN_CONTRAST;

/// What a rendered label's strongest pixel must clear over the material.
/// A glyph's anti-aliased stem never covers a whole pixel at 1x, so the
/// measured pixel falls short of its ink; a disabled label still reads as
/// text, at the non-text floor at least.
const RENDERED_ENABLED_FLOOR: f64 = BODY_FLOOR;
const RENDERED_DISABLED_FLOOR: f64 = 3.0;

fn themes() -> [(&'static str, Theme); 2] {
    [("dark", Theme::dark()), ("light", Theme::light())]
}

fn fill(scene: &mut Scene, colour: Color, shape: &impl vello::kurbo::Shape) {
    scene.fill(Fill::NonZero, Affine::IDENTITY, colour, None, shape);
}

fn canvas() -> Rect {
    Rect::new(0.0, 0.0, f64::from(CANVAS_W), f64::from(CANVAS_H))
}

/// What lies behind the menu.
fn backdrop(name: &str, scene: &mut Scene) {
    match name {
        "white" => fill(scene, Color::from_rgb8(0xff, 0xff, 0xff), &canvas()),
        "black" => fill(scene, Color::from_rgb8(0, 0, 0), &canvas()),
        // A slide with a title, a selection and a bright accent disc under
        // part of the menu: stripes of black and white, a saturated blue
        // disc and a pale panel, the way the words' menu opened over one.
        _ => {
            fill(scene, Color::from_rgb8(0xee, 0xf0, 0xf4), &canvas());
            for column in 0..CANVAS_W / 12 {
                if column % 2 == 0 {
                    let x = f64::from(column * 12);
                    fill(
                        scene,
                        Color::from_rgb8(0x10, 0x12, 0x18),
                        &Rect::new(x, 0.0, x + 12.0, 120.0),
                    );
                }
            }
            fill(
                scene,
                Color::from_rgb8(0x4a, 0x77, 0xd8),
                &Circle::new(Point::new(300.0, 190.0), 70.0),
            );
            fill(
                scene,
                Color::from_rgb8(0xa8, 0xbc, 0xe6),
                &Rect::new(40.0, 150.0, 220.0, 200.0),
            );
        }
    }
}

const ROWS: [(&str, bool); 5] = [
    ("Cut", true),
    ("Paste", true),
    ("Select all", true),
    ("Bulleted list", false),
    ("Numbered list", false),
];

fn menu() -> ContextMenu {
    let mut menu = ContextMenu::new();
    for (label, enabled) in ROWS {
        menu.push(MenuItem::action(label, || {}).with_enabled(enabled));
    }
    menu.open_at([150.0, 60.0]);
    menu
}

/// `paint` over `backdrop`, through the backdrop pass.
fn render(
    theme: &Theme,
    backdrop_name: &str,
    paint: impl FnOnce(&mut Scene, &mut TextRenderer),
) -> Vec<u8> {
    let mut text = TextRenderer::new();
    let mut scene = Scene::new();
    let recording = BackdropRecording::begin();
    layer_scene(&scene, 0);
    backdrop(backdrop_name, &mut scene);
    paint(&mut scene, &mut text);
    let frame = BackdropFrame::from_single_scene(recording.finish());
    assert_eq!(
        frame.regions.len(),
        1,
        "the menu is drawn in the blurred material"
    );
    let mut renderer = OffscreenRenderer::new(CANVAS_W, CANVAS_H);
    renderer.set_backdrop(Some(Arc::new(frame)));
    renderer.render(&scene, theme.tokens.colors.surfaces.content)
}

fn colour_at(rgba: &[u8], x: u32, y: u32) -> Color {
    let (r, g, b, _) = OffscreenRenderer::sample(rgba, CANVAS_W, x, y);
    Color::from_rgb8(r, g, b)
}

fn dump(name: &str, rgba: &[u8]) {
    let Ok(dir) = tilo_ui::env::var("TILO_UI_SCREENSHOT_DIR") else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    let path = format!("{dir}/{name}.png");
    tilo_ui::png::write_rgba8(&path, CANVAS_W, CANVAS_H, rgba).unwrap();
    eprintln!("wrote {path}");
}

/// Pixels of `rect` (inset by `inset`), as integer coordinates.
fn pixels(rect: Rect, inset: f64) -> impl Iterator<Item = (u32, u32)> {
    let rect = rect.inset(-inset);
    let (x0, y0) = (rect.x0.ceil() as u32, rect.y0.ceil() as u32);
    let (x1, y1) = (rect.x1.floor() as u32, rect.y1.floor() as u32);
    (y0..y1).flat_map(move |y| (x0..x1).map(move |x| (x, y)))
}

/// Pixels whose whole square lies inside the panel's rounded body, `inset`
/// in from its edge (clear of the edge hairline).
fn body_pixels(panel: Rect, radius: f64, inset: f64) -> impl Iterator<Item = (u32, u32)> {
    let body = RoundedRect::from_rect(panel.inset(-inset), (radius - inset).max(0.0));
    pixels(panel, inset).filter(move |&(x, y)| {
        let (x, y) = (f64::from(x), f64::from(y));
        [(x, y), (x + 1.0, y), (x, y + 1.0), (x + 1.0, y + 1.0)]
            .into_iter()
            .all(|corner| body.contains(Point::new(corner.0, corner.1)))
    })
}

// Menu text keeps its contrast over any backdrop; a disabled row still
// reads as dimmer text rather than fading into what is behind it.
#[test]
fn a_menus_text_keeps_its_contrast_over_any_backdrop() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = tilo_ui::motion::testing::settled();
    let presence = PresenceCtx::default();
    for (theme_name, theme) in themes() {
        let colors = &theme.tokens.colors;
        let ground = tilo_ui::chrome::panel_ground(&theme);
        // The inks a panel draws: primary labels, and a disabled row's
        // dim ink, each chosen against the opaque ground.
        let disabled_ink = colors
            .on_surface_at(ground, SMALL_TEXT_MIN_CONTRAST)
            .ink_dim;
        let mut shifts = Vec::new();
        for backdrop_name in ["white", "black", "busy"] {
            let case = format!("{theme_name} over {backdrop_name}");
            let mut shown = menu();
            let with_text = render(&theme, backdrop_name, |scene, text| {
                shown.draw_overlay(scene, Affine::IDENTITY, text, &theme, &presence);
            });
            dump(
                &format!("menu_material_{theme_name}_{backdrop_name}"),
                &with_text,
            );
            let panel = Widget::modal_capture(&shown).expect("the menu is open");
            let material = render(&theme, backdrop_name, |scene, _| {
                tilo_ui::widget::floating_surface::paint_rounded(
                    scene,
                    Affine::IDENTITY,
                    panel,
                    tilo_ui::widget::floating_surface::radius(&theme),
                    &theme,
                );
            });

            let (mut lightest, mut darkest) = (0.0_f64, 1.0_f64);
            let radius = tilo_ui::widget::floating_surface::radius(&theme);
            for (x, y) in body_pixels(panel, radius, 2.0) {
                let under = colour_at(&material, x, y);
                let luminance = tilo_ui::color::relative_luminance(under);
                lightest = lightest.max(luminance);
                darkest = darkest.min(luminance);
                for (ink_name, ink) in
                    [("primary", colors.text_primary), ("disabled", disabled_ink)]
                {
                    let ratio = contrast_ratio(ink, under);
                    assert!(
                        ratio >= BODY_FLOOR,
                        "{case}: {ink_name} ink reads {ratio:.2}:1 on the material at ({x}, {y}), \
                         {under:?}"
                    );
                }
            }
            let ground_luminance = tilo_ui::color::relative_luminance(ground);
            shifts.push((lightest - ground_luminance).max(ground_luminance - darkest));

            let mut enabled_ratios = Vec::new();
            for (index, (label, enabled)) in ROWS.iter().enumerate() {
                let row = shown.item_rect(index).expect("an action row");
                let strongest = pixels(row, 1.0)
                    .map(|(x, y)| {
                        contrast_ratio(colour_at(&with_text, x, y), colour_at(&material, x, y))
                    })
                    .fold(1.0_f64, f64::max);
                let floor = if *enabled {
                    RENDERED_ENABLED_FLOOR
                } else {
                    RENDERED_DISABLED_FLOOR
                };
                assert!(
                    strongest >= floor,
                    "{case}: \"{label}\" reads {strongest:.2}:1 on the material, below {floor}"
                );
                if *enabled {
                    enabled_ratios.push(strongest);
                } else {
                    let weakest_enabled = enabled_ratios.iter().copied().fold(f64::MAX, f64::min);
                    assert!(
                        strongest < weakest_enabled,
                        "{case}: disabled \"{label}\" ({strongest:.2}:1) must read dimmer than \
                         the enabled rows ({weakest_enabled:.2}:1)"
                    );
                }
            }
        }
        // The pass ran: some backdrop shows through the material. (Over a
        // backdrop past the band's end the material holds the end, flat.)
        assert!(
            shifts.iter().any(|shift| *shift > 0.002),
            "{theme_name}: the material never left its opaque ground ({shifts:?}); the \
             backdrop pass did not run"
        );
    }
}

// The words' menu of a text box drawn in dark ink
// on a light slide, under the dark theme: its labels are the menu's inks,
// not the words'.
#[test]
fn the_words_menu_of_dark_words_on_a_light_slide_reads_in_the_dark_theme() {
    use tilo_ui::widget::{BlockContent, BlockEditor, Embedded};
    let _settled = tilo_ui::motion::testing::settled();
    let theme = Theme::dark();
    let presence = PresenceCtx::default();
    presence.viewport.set(Some(canvas()));
    let mut text = TextRenderer::new();
    let mut host = Embedded::new(BlockEditor::new().with_wrap_lines(true));
    host.widget_mut()
        .present_blocks(&[BlockContent::paragraph("a", "Goals for this phase")]);
    host.set_text_color(Some(Color::from_rgb8(0x12, 0x14, 0x18)));
    host.layout(Rect::new(40.0, 30.0, 400.0, 30.0), &theme, &mut text);
    host.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut text,
        &theme,
        &presence,
    );
    assert!(host.widget_mut().open_text_menu(Point::new(60.0, 40.0)));
    let panel = host.modal_capture().expect("the menu is open");
    let menu = host.widget().text_menu().expect("the words' menu is open");
    let rows: Vec<(String, Rect, bool)> = menu
        .items()
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let offset = panel.origin() - Widget::modal_capture(menu)?.origin();
            Some((
                item.label()?.to_owned(),
                menu.item_rect(index)? + offset,
                item.is_enabled(),
            ))
        })
        .collect();
    assert!(rows.len() >= 5, "the words' menu has its rows: {rows:?}");
    let with_text = render(&theme, "white", |scene, text| {
        host.draw_overlay(scene, Affine::IDENTITY, text, &theme, &presence);
    });
    dump("menu_material_words_on_light_slide_dark", &with_text);
    let material = render(&theme, "white", |scene, _| {
        tilo_ui::widget::floating_surface::paint_rounded(
            scene,
            Affine::IDENTITY,
            panel,
            tilo_ui::widget::floating_surface::radius(&theme),
            &theme,
        );
    });
    for (label, row, enabled) in rows {
        // The label, not the shortcut at the row's end.
        let label_part = Rect::new(row.x0, row.y0, row.x0 + row.width() * 0.55, row.y1);
        let strongest = pixels(label_part, 1.0)
            .map(|(x, y)| contrast_ratio(colour_at(&with_text, x, y), colour_at(&material, x, y)))
            .fold(1.0_f64, f64::max);
        let floor = if enabled {
            RENDERED_ENABLED_FLOOR
        } else {
            RENDERED_DISABLED_FLOOR
        };
        assert!(
            strongest >= floor,
            "\"{label}\" reads {strongest:.2}:1 on the dark menu, below {floor}"
        );
    }
}
