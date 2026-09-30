//! Offscreen proof that every menu-like list paints a *visible* hover /
//! keyboard highlight in both the dark and the light theme.
//!
//! User report (2026-09-16): "in the dark menu when you have to right click
//! something, and a context menu opens or a drop down there is no
//! highlighting." Each case here opens a real widget, moves the highlight onto
//! one row the way a pointer or the keyboard would, renders the scene over the
//! theme's own window ground and samples a text-free pixel on the highlighted
//! row against the same pixel on a plain neighbour. The perceptual step
//! (CIE ΔL*) must clear the floor the veneer tokens promise (≈ 5 for the
//! subtle tier, ≈ 10 for the medium one — `ui/src/tokens/colors.rs`).
//!
//! A case that cannot observe its subject fails: there is no adapter skip.
//! Set `TILO_UI_SCREENSHOT_DIR=<path>` to dump every rendered PNG.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::{AutoCommit, ROOT};
use taffy::{Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::event::{DeviceId, ElementState, KeyEvent, Modifiers, RawKeyEvent, WindowEvent};
use winit::keyboard::{Key, KeyLocation, NamedKey, NativeKeyCode, PhysicalKey};

use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::Dropdown;
use tilo_ui::widget::Widget;
use tilo_ui::widget::autocomplete::{Autocomplete, Suggestion};
use tilo_ui::widget::context_menu::{ContextMenu, MenuItem};
use tilo_ui::widget::link_popover::PageEntry;
use tilo_ui::widget::page_palette::PageJumpPalette as PagePalette;
use tilo_ui::widget::slash_menu::SlashMenu;

const CANVAS_W: u32 = 480;
const CANVAS_H: u32 = 360;

/// The smallest perceptual step a *veneer* highlight may take and still count
/// as "visible" — half the medium veneer's promised ΔL*, comfortably above the
/// ~2.3 JND. Only the pre-arm hover cases use this floor.
const MIN_DELTA_L: f64 = 4.0;

/// The floor for an *armed* row (the one Enter / a click activates). The
/// 10 % veneer that used to paint the context menu's armed row measured
/// ΔL* 11.6 on dark — the value the user reported as "no highlighting" — so a
/// floor that the old paint would pass guards nothing. Every shipped preset's
/// accent clears 20 (the lowest, `gruvboxlight`, measures 28.6).
const MIN_ARMED_DELTA_L: f64 = 20.0;

fn color_of(px: (u8, u8, u8, u8)) -> Color {
    Color::from_rgba8(px.0, px.1, px.2, px.3)
}

/// CIE L* of an opaque sRGB pixel, from the crate's own WCAG luminance so the
/// two "perceptual" numbers in the repo cannot drift apart.
fn lightness(px: (u8, u8, u8, u8)) -> f64 {
    let y = tilo_ui::a11y::relative_luminance(color_of(px));
    let f = if y > 216.0 / 24389.0 {
        y.cbrt()
    } else {
        (841.0 / 108.0) * y + 4.0 / 29.0
    };
    116.0 * f - 16.0
}

fn themes() -> [(&'static str, Theme); 2] {
    [("dark", Theme::dark()), ("light", Theme::light())]
}

fn dispatch(widget: &mut dyn Widget, event: &WindowEvent) {
    let mut doc = AutoCommit::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let node = taffy.new_leaf(Style::default()).unwrap();
    let mut handled = false;
    let mut focus = None;
    widget.handle_event(
        event,
        &mut tilo_ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &Modifiers::default(),
            state: &mut doc,
            path: &ROOT,
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: node,
            taffy: &taffy,
        },
    );
}

fn cursor_moved(p: Point) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(p.x, p.y),
    }
}

fn key_press(key: NamedKey) -> WindowEvent {
    let key = Key::Named(key);
    let event = KeyEvent::synthetic(
        RawKeyEvent {
            physical_key: PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
            state: ElementState::Pressed,
        },
        key.clone(),
        None,
        KeyLocation::Standard,
        false,
        None,
        key,
    );
    WindowEvent::KeyboardInput {
        device_id: DeviceId::dummy(),
        event,
        is_synthetic: false,
    }
}

struct Rendered {
    rgba: Vec<u8>,
}

impl Rendered {
    fn sample(&self, p: Point) -> (u8, u8, u8, u8) {
        let (x, y) = (p.x.round() as u32, p.y.round() as u32);
        assert!(
            x < CANVAS_W && y < CANVAS_H,
            "sample {p:?} is off the canvas"
        );
        OffscreenRenderer::sample(&self.rgba, CANVAS_W, x, y)
    }

    fn dump(&self, name: &str) {
        let Ok(dir) = tilo_ui::env::var("TILO_UI_SCREENSHOT_DIR") else {
            return;
        };
        std::fs::create_dir_all(&dir).unwrap();
        let path = format!("{dir}/{name}.png");
        tilo_ui::png::write_rgba8(&path, CANVAS_W, CANVAS_H, &self.rgba).unwrap();
        eprintln!("wrote {path}");
    }
}

fn render(theme: &Theme, paint: impl FnOnce(&mut Scene, &mut TextRenderer)) -> Rendered {
    let mut renderer = OffscreenRenderer::new(CANVAS_W, CANVAS_H);
    let mut tr = TextRenderer::new();
    let mut scene = Scene::new();
    paint(&mut scene, &mut tr);
    let rgba = renderer.render(&scene, theme.bg_app);
    Rendered { rgba }
}

/// The shared assertion: the highlighted row's text-free pixel must be an
/// opaque, perceptibly different colour from the same pixel one row over.
fn assert_highlight_visible(
    rendered: &Rendered,
    highlighted: Point,
    plain: Point,
    theme_name: &str,
    surface: &str,
) {
    let hi = rendered.sample(highlighted);
    let lo = rendered.sample(plain);
    assert_eq!(
        hi.3, 0xff,
        "{surface}/{theme_name}: highlighted row sample is not opaque"
    );
    assert_eq!(
        lo.3, 0xff,
        "{surface}/{theme_name}: plain row sample is not opaque"
    );
    let delta = (lightness(hi) - lightness(lo)).abs();
    eprintln!("{surface}/{theme_name}: highlighted {hi:?} vs plain {lo:?} → ΔL*={delta:.2}");
    assert!(
        delta >= MIN_DELTA_L,
        "{surface}/{theme_name}: highlighted row {hi:?} vs plain row {lo:?} differ by only \
         ΔL*={delta:.2} (< {MIN_DELTA_L}) — the highlight is invisible"
    );
}

/// The assertion for an *armed* row: the sampled fill must be exactly the
/// theme's primary accent (so a regression back to any veneer fails on the
/// colour, not on a threshold), the step to the neighbour must clear
/// [`MIN_ARMED_DELTA_L`], and the ink the row draws its label with must meet
/// the text-contrast floor against the fill actually painted.
fn assert_armed_row(
    rendered: &Rendered,
    highlighted: Point,
    plain: Point,
    theme: &Theme,
    theme_name: &str,
    surface: &str,
) {
    let hi = rendered.sample(highlighted);
    let lo = rendered.sample(plain);
    assert_eq!(
        hi.3, 0xff,
        "{surface}/{theme_name}: armed row sample is not opaque"
    );
    assert_eq!(
        lo.3, 0xff,
        "{surface}/{theme_name}: plain row sample is not opaque"
    );
    let accent = theme.tokens.colors.accent[tilo_ui::tokens::colors::ACCENT_PRIMARY_IDX].to_rgba8();
    assert_eq!(
        (hi.0, hi.1, hi.2),
        (accent.r, accent.g, accent.b),
        "{surface}/{theme_name}: armed row is painted {hi:?}, not the theme accent"
    );
    let delta = (lightness(hi) - lightness(lo)).abs();
    eprintln!("{surface}/{theme_name}: armed {hi:?} vs plain {lo:?} → ΔL*={delta:.2}");
    assert!(
        delta >= MIN_ARMED_DELTA_L,
        "{surface}/{theme_name}: armed row {hi:?} vs plain row {lo:?} differ by only \
         ΔL*={delta:.2} (< {MIN_ARMED_DELTA_L})"
    );
    let ink = theme
        .tokens
        .colors
        .on_accent(tilo_ui::tokens::colors::ON_FILL_MIN_CONTRAST);
    let ratio = tilo_ui::a11y::contrast_ratio(ink, color_of(hi));
    assert!(
        ratio >= tilo_ui::tokens::colors::ON_FILL_MIN_CONTRAST,
        "{surface}/{theme_name}: armed-row ink {ink:?} on painted fill {hi:?} is {ratio:.2}:1"
    );
}

/// A `Rect` that is safely inside the row and clear of any label text: the
/// row's right edge, where every list in this file leaves the ground bare.
fn text_free_probe(row: Rect) -> Point {
    Point::new(row.x1 - 14.0, row.center().y)
}

/// Row 0 carries the longest label on purpose: measured menus hug their
/// widest row, so the probed rows (1 and 2) must be well short of it for the
/// right-edge probe to stay clear of glyphs.
fn labels() -> [&'static str; 4] {
    ["Duplicate everything here", "Open", "Rename…", "Delete"]
}

#[test]
fn context_menu_highlight_is_visible_by_pointer_keyboard_and_theme() {
    // Case `context_menu_pointer_hover_is_visible_in_both_themes`.
    {
        for (name, theme) in themes() {
            let mut menu = ContextMenu::new();
            for label in labels() {
                menu.push(MenuItem::action(label, || {}));
            }
            // The legacy point-placement path every right-click host uses.
            menu.open_at([40.0, 40.0]);
            let row1 = menu.item_rect(1).expect("row 1 is an action row");
            dispatch(&mut menu, &cursor_moved(row1.center()));
            assert_eq!(menu.hovered(), Some(1), "pointer over row 1 highlights it");
            let row2 = menu.item_rect(2).unwrap();

            let presence = PresenceCtx::default();
            let rendered = render(&theme, |scene, tr| {
                menu.draw(scene, Affine::IDENTITY, tr, &theme, &presence);
            });
            rendered.dump(&format!("menu_highlight_context_menu_hover_{name}"));
            assert_armed_row(
                &rendered,
                text_free_probe(row1),
                text_free_probe(row2),
                &theme,
                name,
                "context-menu/pointer",
            );
        }
    }

    // Case `context_menu_keyboard_highlight_is_visible_when_anchored`.
    {
        for (name, theme) in themes() {
            let mut menu = ContextMenu::new();
            for label in labels() {
                menu.push(MenuItem::action(label, || {}));
            }
            let viewport = Rect::new(0.0, 0.0, f64::from(CANVAS_W), f64::from(CANVAS_H));
            // The measured, viewport-bounded path shared hosts (toolbar
            // overflow, shell) use.
            menu.open_anchored(Rect::new(40.0, 20.0, 120.0, 48.0), viewport);
            let presence = PresenceCtx::default();
            let mut tr_probe = TextRenderer::new();
            menu.layout_within(&mut tr_probe, &theme);
            dispatch(&mut menu, &key_press(NamedKey::ArrowDown));
            dispatch(&mut menu, &key_press(NamedKey::ArrowDown));
            assert_eq!(menu.hovered(), Some(1), "two ArrowDowns land on row 1");
            let row1 = menu.item_rect(1).unwrap();
            let row2 = menu.item_rect(2).unwrap();

            let rendered = render(&theme, |scene, tr| {
                menu.draw(scene, Affine::IDENTITY, tr, &theme, &presence);
            });
            rendered.dump(&format!("menu_highlight_context_menu_keyboard_{name}"));
            assert_armed_row(
                &rendered,
                text_free_probe(row1),
                text_free_probe(row2),
                &theme,
                name,
                "context-menu/keyboard",
            );
        }
    }

    // Case `context_menu_highlight_is_visible_in_every_shipped_theme`.
    // Every preset `Theme::named` resolves, including the accessibility track:
    // the veneer polarity and the accent are per-theme decisions, so a highlight
    // that survives the two defaults can still vanish on a preset.
    // The armed context-menu row must read on every shipped theme, and its
    // label must stay legible on the fill it sits on — the reported surface,
    // swept across every palette the OS can be set to.
    {
        for name in Theme::ALL {
            let theme = Theme::named(name).expect("shipped preset resolves");
            let mut menu = ContextMenu::new();
            for label in labels() {
                menu.push(MenuItem::action(label, || {}));
            }
            menu.open_at([40.0, 40.0]);
            let row1 = menu.item_rect(1).unwrap();
            dispatch(&mut menu, &cursor_moved(row1.center()));
            assert_eq!(menu.hovered(), Some(1));
            let row2 = menu.item_rect(2).unwrap();

            let presence = PresenceCtx::default();
            let rendered = render(&theme, |scene, tr| {
                menu.draw(scene, Affine::IDENTITY, tr, &theme, &presence);
            });
            rendered.dump(&format!("menu_highlight_context_menu_hover_{name}"));
            assert_armed_row(
                &rendered,
                text_free_probe(row1),
                text_free_probe(row2),
                &theme,
                name,
                "context-menu/preset",
            );
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn list_menu_cursor_rows_are_visible_in_both_themes() {
    // Case `dropdown_open_list_highlight_is_visible_in_both_themes`.
    {
        for (name, theme) in themes() {
            let options: Vec<String> = labels().iter().map(|s| (*s).to_string()).collect();
            let mut dd = Dropdown::new(options, "choice").open();
            let control = Rect::new(40.0, 20.0, 300.0, 60.0);
            dd.apply_layout_external(control);
            let mut doc = AutoCommit::new();
            dd.sync(&mut doc, &ROOT);
            // Rows hang directly under the control: LIST_GAP 4, ITEM_HEIGHT 32.
            let row = |i: f64| {
                let top = control.y1 + 4.0 + i * 32.0;
                Rect::new(control.x0, top, control.x1, top + 32.0)
            };
            dispatch(&mut dd, &cursor_moved(row(1.0).center()));
            assert_eq!(
                dd.highlight_index(),
                1,
                "pointer over row 1 moves the highlight"
            );

            let presence = PresenceCtx::default();
            let rendered = render(&theme, |scene, tr| {
                dd.draw(scene, Affine::IDENTITY, tr, &theme, &presence);
                dd.draw_overlay(scene, Affine::IDENTITY, tr, &theme, &presence);
            });
            rendered.dump(&format!("menu_highlight_dropdown_hover_{name}"));
            assert_armed_row(
                &rendered,
                text_free_probe(row(1.0)),
                text_free_probe(row(2.0)),
                &theme,
                name,
                "dropdown",
            );
        }
    }

    // Case `autocomplete_pointer_hover_is_visible_in_both_themes`.
    {
        for (name, theme) in themes() {
            let mut ac = Autocomplete::new();
            let field = Rect::new(40.0, 20.0, 300.0, 60.0);
            ac.apply_layout_external(field);
            ac.begin_edit();
            ac.set_suggestions_open(labels().iter().map(|s| Suggestion::new(*s)).collect());
            assert!(ac.is_open(), "the suggestion list is open");
            // Panel starts DROPDOWN_GAP (6) under the field with a 4 px inset;
            // rows are ROW_H (32) tall and inset 4 px from each side.
            let row = |i: f64| {
                let top = field.y1 + 6.0 + 4.0 + i * 32.0;
                Rect::new(field.x0 + 4.0, top, field.x1 - 4.0, top + 32.0)
            };
            dispatch(&mut ac, &cursor_moved(row(1.0).center()));
            assert!(
                ac.highlighted().is_none(),
                "a pointer hover must not arm the row — that is the keyboard's state"
            );

            let presence = PresenceCtx::default();
            let rendered = render(&theme, |scene, tr| {
                ac.draw(scene, Affine::IDENTITY, tr, &theme, &presence);
                ac.draw_overlay(scene, Affine::IDENTITY, tr, &theme, &presence);
            });
            rendered.dump(&format!("menu_highlight_autocomplete_hover_{name}"));
            assert_highlight_visible(
                &rendered,
                text_free_probe(row(1.0)),
                text_free_probe(row(2.0)),
                name,
                "autocomplete/hover",
            );
        }
    }

    // Case `slash_menu_cursor_row_is_visible_in_both_themes`.
    {
        for (name, theme) in themes() {
            let mut menu = SlashMenu::new();
            menu.set_anchor(Point::new(40.0, 20.0));
            menu.set_visible(true);
            let row1 = menu.row_rect(1).expect("slash menu has at least two rows");
            let row2 = menu
                .row_rect(2)
                .expect("slash menu has at least three rows");
            assert!(
                menu.hover_at(row1.center()),
                "hovering row 1 moves the cursor"
            );

            let rendered = render(&theme, |scene, tr| {
                menu.draw_overlay(scene, Affine::IDENTITY, tr, &theme);
            });
            rendered.dump(&format!("menu_highlight_slash_menu_hover_{name}"));
            assert_armed_row(
                &rendered,
                text_free_probe(row1),
                text_free_probe(row2),
                &theme,
                name,
                "slash-menu",
            );
        }
    }

    // Case `page_palette_cursor_row_is_visible_in_both_themes`.
    {
        for (name, theme) in themes() {
            let mut palette = PagePalette::new();
            palette.set_items(
                labels()
                    .iter()
                    .map(|s| PageEntry {
                        id: uuid::Uuid::new_v4(),
                        title: (*s).to_string(),
                    })
                    .collect(),
            );
            palette.set_anchor(Point::new(0.0, 0.0));
            palette.set_visible(true);
            dispatch_key_to_palette(&mut palette, NamedKey::ArrowDown);
            assert_eq!(
                palette.highlighted().map(|e| e.title.as_str()),
                Some("Open"),
                "ArrowDown moves the cursor to row 1"
            );
            // Card PADDING 12; input INPUT_HEIGHT 44; rows ROW_HEIGHT 36.
            let rect = palette.rect();
            let list_top = rect.y0 + 12.0 + 44.0 + 12.0;
            let row = |i: f64| {
                let top = list_top + i * 36.0;
                Rect::new(rect.x0 + 12.0, top, rect.x1 - 12.0, top + 36.0)
            };

            let rendered = render(&theme, |scene, tr| {
                palette.draw_overlay(scene, Affine::IDENTITY, tr, &theme);
            });
            rendered.dump(&format!("menu_highlight_page_palette_cursor_{name}"));
            assert_armed_row(
                &rendered,
                text_free_probe(row(1.0)),
                text_free_probe(row(2.0)),
                &theme,
                name,
                "page-palette",
            );
        }
    }
}

fn dispatch_key_to_palette(palette: &mut PagePalette, key: NamedKey) {
    assert!(
        palette.handle_key(&key_press(key)),
        "the open palette consumes {key:?}"
    );
}

#[test]
fn closed_dropdown_hover_is_visible_on_an_opaque_ground() {
    // Case `bg_app_ground_is_opaque_so_samples_are_meaningful`.
    {
        // Guards the sampling method itself: every case composites over the
        // theme's window ground, so a transparent ground would make each ΔL*
        // above meaningless rather than merely wrong.
        for name in Theme::ALL {
            let theme = Theme::named(name).expect("shipped preset resolves");
            let px = theme.bg_app.to_rgba8();
            assert_eq!(px.a, 0xff, "{name}: bg_app must be opaque");
        }
    }

    // Case `dropdown_closed_control_hover_is_visible_in_both_themes`.
    // The closed control itself: a pointer resting on a dropdown must be
    // answered (HIG §7), which the widget tracked but never painted.
    {
        // Half the medium tier's floor: the resting hover is the *subtle* veneer
        // (≈ 5 ΔL*), the quietest step the token set promises to be visible.
        const MIN_SUBTLE_DELTA_L: f64 = 3.0;
        for (name, theme) in themes() {
            let options: Vec<String> = labels().iter().map(|s| (*s).to_string()).collect();
            let mut dd = Dropdown::new(options, "choice");
            let control = Rect::new(40.0, 20.0, 300.0, 60.0);
            dd.apply_layout_external(control);
            let mut doc = AutoCommit::new();
            dd.sync(&mut doc, &ROOT);
            let presence = PresenceCtx::default();
            // Clear of both the label (left) and the chevron (right 30 px).
            let probe = Point::new(control.x1 - 44.0, control.center().y);

            let resting = render(&theme, |scene, tr| {
                dd.draw(scene, Affine::IDENTITY, tr, &theme, &presence);
            });
            dispatch(&mut dd, &cursor_moved(control.center()));
            assert!(!dd.is_open(), "a hover must not open the list");
            let hovered = render(&theme, |scene, tr| {
                dd.draw(scene, Affine::IDENTITY, tr, &theme, &presence);
            });
            hovered.dump(&format!("menu_highlight_dropdown_control_hover_{name}"));

            let rest = resting.sample(probe);
            let hov = hovered.sample(probe);
            assert_eq!(rest.3, 0xff, "{name}: resting control sample is not opaque");
            assert_eq!(hov.3, 0xff, "{name}: hovered control sample is not opaque");
            let delta = (lightness(hov) - lightness(rest)).abs();
            eprintln!(
                "dropdown-control/{name}: hovered {hov:?} vs resting {rest:?} → ΔL*={delta:.2}"
            );
            assert!(
                delta >= MIN_SUBTLE_DELTA_L,
                "{name}: hovering the closed dropdown changed the fill by only ΔL*={delta:.2}"
            );
        }
    }
}
