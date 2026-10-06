//! Offscreen review harness for the keyboard's look.
//!
//! Renders the production [`OskWidget`] paint path into PNGs across themes
//! (light, dark, high contrast), surface sizes (phone and tablet) and the
//! states a user sees: each letter layout, symbols, suggestions, the
//! autocorrect chip, the emoji panel and a pressed key. Nothing here opens a
//! window or touches the session.
//!
//! ```text
//! cargo test -p osk --lib visual_review -- --ignored --nocapture
//! ```
//!
//! Images land in `$TILO_OSK_SHOT_DIR`, or `target/tmp/osk-visual` when it is
//! unset.

use std::collections::HashMap;
use std::path::PathBuf;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::offscreen::OffscreenRenderer;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};

use super::OskWidget;
use crate::layout::KeyAction;

fn shot_dir() -> PathBuf {
    std::env::var_os("TILO_OSK_SHOT_DIR").map_or_else(
        || {
            // `CARGO_TARGET_TMPDIR` only exists for integration tests; a lib
            // test finds the target dir from its own executable instead.
            std::env::current_exe()
                .ok()
                .and_then(|exe| exe.ancestors().nth(3).map(|t| t.join("tmp/osk-visual")))
                .unwrap_or_else(|| std::env::temp_dir().join("osk-visual"))
        },
        PathBuf::from,
    )
}

/// Phone and tablet surfaces, in logical px. The height is the keyboard's own
/// intrinsic height (it asks for toolbar + suggestions + rows).
fn sizes() -> [(&'static str, f32); 2] {
    [("phone", 390.0), ("tablet", 1024.0)]
}

fn themes() -> [(&'static str, Theme); 3] {
    [
        ("light", Theme::light()),
        ("dark", Theme::dark()),
        ("contrast", Theme::high_contrast()),
    ]
}

pub(super) fn lay_out(osk: &mut OskWidget, theme: &Theme, w: f32) -> f32 {
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = osk.build_layout(&mut taffy, theme, &mut node_map);
    let h = taffy
        .style(child)
        .ok()
        .and_then(|s| s.size.height.into_option())
        .unwrap_or(320.0);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(w),
                    height: Dimension::length(h),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    taffy
        .compute_layout(
            root,
            Size {
                width: taffy::AvailableSpace::Definite(w),
                height: taffy::AvailableSpace::Definite(h),
            },
        )
        .unwrap();
    osk.apply_layout(&taffy, child, Point::ORIGIN);
    h
}

pub(super) fn render(osk: &mut OskWidget, theme: &Theme, w: f32, h: f32, name: &str) {
    let dir = shot_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let mut tr = TextRenderer::new();
    // Device pixels per logical pixel, so glyph edges and hairlines read as
    // they do on a phone panel. A render ratio, not a layout size.
    let scale = 2.0;
    let mut scene = Scene::new();
    osk.draw(
        &mut scene,
        Affine::scale(scale),
        &mut tr,
        theme,
        &PresenceCtx::empty(),
    );
    let (pw, ph) = (
        (f64::from(w) * scale).round() as u32,
        (f64::from(h) * scale).round() as u32,
    );
    let mut renderer = OffscreenRenderer::new(pw, ph);
    let path = dir.join(format!("{name}.png"));
    renderer.render_png(&scene, theme.tokens.colors.surfaces.content, &path);
    eprintln!("wrote {}", path.display());
}

fn type_text(osk: &mut OskWidget, text: &str) {
    for c in text.chars() {
        let action = if c == ' ' {
            KeyAction::Space
        } else {
            KeyAction::Char(c.to_string())
        };
        osk.apply_action(action);
    }
}

/// One keyboard state to photograph.
struct State {
    name: &'static str,
    build: fn() -> OskWidget,
    /// Runs after layout, so it can press keys by their geometry.
    after_layout: fn(&mut OskWidget),
}

#[allow(clippy::too_many_lines, reason = "one flat table of scenarios")]
fn states() -> Vec<State> {
    fn plain() -> OskWidget {
        OskWidget::new()
    }
    fn none(_: &mut OskWidget) {}
    vec![
        State {
            name: "qwerty",
            build: plain,
            after_layout: none,
        },
        State {
            name: "azerty",
            build: || OskWidget::new().with_active_layout("azerty"),
            after_layout: none,
        },
        State {
            name: "symbols",
            build: plain,
            after_layout: |osk| {
                osk.apply_action(KeyAction::SymbolsToggle);
            },
        },
        State {
            name: "suggestions",
            build: plain,
            after_layout: |osk| type_text(osk, "hel"),
        },
        State {
            name: "autocorrect",
            build: plain,
            after_layout: |osk| type_text(osk, "teh "),
        },
        State {
            name: "shift",
            build: plain,
            after_layout: |osk| osk.apply_action(KeyAction::Shift),
        },
        State {
            name: "emoji",
            build: plain,
            after_layout: |osk| {
                osk.activate_label("Emoji");
            },
        },
        State {
            name: "pressed-g",
            build: plain,
            after_layout: |osk| press_label(osk, "g"),
        },
        State {
            name: "pressed-q",
            build: plain,
            after_layout: |osk| press_label(osk, "q"),
        },
        State {
            name: "pressed-p",
            build: plain,
            after_layout: |osk| press_label(osk, "p"),
        },
        State {
            name: "longpress-e",
            build: plain,
            after_layout: |osk| long_press(osk, "e", 2),
        },
        State {
            name: "longpress-o",
            build: plain,
            after_layout: |osk| long_press(osk, "o", 8),
        },
        State {
            name: "french-longpress-e",
            build: || OskWidget::new().with_active_layout("azerty"),
            after_layout: |osk| long_press(osk, "e", 1),
        },
        State {
            name: "french-longpress-o",
            build: || OskWidget::new().with_active_layout("azerty"),
            after_layout: |osk| long_press(osk, "o", 2),
        },
        State {
            name: "space-cursor",
            build: plain,
            after_layout: |osk| {
                type_text(osk, "hello");
                let Some((id, rect)) = key_labelled(osk, "space") else {
                    return;
                };
                osk.pressed_key = Some(id);
                let x = rect.center().x;
                osk.begin_cursor_drag(id, None, x);
                osk.drag_cursor(None, x - 80.0);
            },
        },
        State {
            name: "hits-start",
            build: plain,
            after_layout: |osk| osk.hit_overlay = true,
        },
        State {
            name: "hits-th",
            build: plain,
            after_layout: |osk| {
                type_text(osk, "th");
                osk.hit_overlay = true;
            },
        },
        State {
            name: "hits-qu",
            build: plain,
            after_layout: |osk| {
                type_text(osk, "qu");
                osk.hit_overlay = true;
            },
        },
    ]
}

fn key_labelled(
    osk: &OskWidget,
    label: &str,
) -> Option<(crate::layout::KeyId, vello::kurbo::Rect)> {
    let layout = osk.current_layout();
    osk.key_geom
        .iter()
        .find(|(id, _)| layout.key(*id).is_some_and(|k| k.label == label))
        .copied()
}

/// Hold a finger on the key labelled `label`, as a touch-down would.
fn press_label(osk: &mut OskWidget, label: &str) {
    if let Some((id, _)) = key_labelled(osk, label) {
        osk.pressed_key = Some(id);
    }
}

/// Hold `label` past the long-press delay and slide to alternate `pick`.
fn long_press(osk: &mut OskWidget, label: &str, pick: usize) {
    let Some((id, _)) = key_labelled(osk, label) else {
        return;
    };
    osk.pressed_key = Some(id);
    let now = std::time::Instant::now();
    osk.begin_hold(id, None, now);
    osk.poll_hold(now + super::LONG_PRESS);
    if let Some((_, popup)) = osk.alternates.as_mut()
        && let Some(cell) = popup.cells.get(pick)
    {
        let x = cell.center().x;
        popup.track(x);
    }
}

#[test]
#[ignore = "writes review PNGs; run explicitly with --ignored"]
fn render_keyboard_matrix() {
    for (theme_name, theme) in themes() {
        for (size_name, w) in sizes() {
            for state in states() {
                let mut osk = (state.build)();
                lay_out(&mut osk, &theme, w);
                (state.after_layout)(&mut osk);
                // A state may change the row count (symbols) or the bands.
                let h = lay_out(&mut osk, &theme, w);
                render(
                    &mut osk,
                    &theme,
                    w,
                    h,
                    &format!("{theme_name}-{size_name}-{}", state.name),
                );
            }
        }
    }
}
