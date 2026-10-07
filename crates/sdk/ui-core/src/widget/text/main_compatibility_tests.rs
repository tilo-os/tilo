//! Actual Text color-cache regression for main's drawn_color input.
use super::*;
fn draw_current(text: &mut Text, renderer: &mut TextRenderer, theme: &Theme) -> Scene {
    text.apply_layout_external(vello::kurbo::Rect::new(10.0, 20.0, 810.0, 120.0));
    let mut scene = Scene::new();
    crate::widget::Widget::draw(
        text,
        &mut scene,
        Affine::IDENTITY,
        renderer,
        theme,
        &crate::presence::PresenceCtx::empty(),
    );
    crate::widget::Widget::mark_drawn(text);
    assert!(
        !scene.encoding().resources.glyph_runs.is_empty(),
        "original actual Text glyphs required"
    );
    scene
}
fn assert_same_scene(actual: &Scene, expected: &Scene) {
    assert!(
        exact_scene_encoding_eq(actual, expected),
        "hot actual Text must repaint original current color exactly as a cold widget"
    );
}
#[test]
fn main_actual_text_default_color_changes_repaint_without_changing_prepared_geometry() {
    let mut renderer = crate::font_catalog::fixture_renderer();
    let mut hot = Text::new("Current color Καλημέρα");
    let first_theme = Theme::dark();
    let first = draw_current(&mut hot, &mut renderer, &first_theme);
    let retained = hot
        .prepared_input
        .as_ref()
        .expect("bounded ordinary text uses retained input")
        .clone();
    let mut changed = Theme::dark();
    changed.tokens.colors.text_primary = vello::peniko::Color::WHITE;
    if changed.tokens.colors.text_primary == first_theme.tokens.colors.text_primary {
        changed.tokens.colors.text_primary = vello::peniko::Color::BLACK;
    }
    let current = draw_current(&mut hot, &mut renderer, &changed);
    let cold = draw_current(
        &mut Text::new("Current color Καλημέρα"),
        &mut renderer,
        &changed,
    );
    assert_same_scene(&current, &cold);
    assert!(
        !exact_scene_encoding_eq(&first, &current),
        "real brush change required"
    );
    assert_eq!(retained.text(), hot.prepared_input.as_ref().unwrap().text());
    assert_eq!(hot.drawn_color, Some(changed.tokens.colors.text_primary));
    assert_same_scene(&draw_current(&mut hot, &mut renderer, &first_theme), &first);
}
#[test]
fn main_actual_text_dimmed_and_explicit_colors_preserve_original_transition_order() {
    let mut renderer = crate::font_catalog::fixture_renderer();
    let theme = Theme::dark();
    let label = "Current explicit color";
    let mut hot = Text::new(label);
    draw_current(&mut hot, &mut renderer, &theme);
    hot = hot.with_color(vello::peniko::Color::WHITE);
    let explicit = draw_current(&mut hot, &mut renderer, &theme);
    assert_same_scene(
        &explicit,
        &draw_current(
            &mut Text::new(label).with_color(vello::peniko::Color::WHITE),
            &mut renderer,
            &theme,
        ),
    );
    hot = hot.dimmed();
    let dimmed = draw_current(&mut hot, &mut renderer, &theme);
    assert_same_scene(
        &dimmed,
        &draw_current(&mut Text::new(label).dimmed(), &mut renderer, &theme),
    );
    assert_eq!(hot.drawn_color, Some(theme.text_dim));
    let mut changed = Theme::dark();
    changed.text_dim = vello::peniko::Color::BLACK;
    if changed.text_dim == theme.text_dim {
        changed.text_dim = vello::peniko::Color::WHITE;
    }
    assert_same_scene(
        &draw_current(&mut hot, &mut renderer, &changed),
        &draw_current(&mut Text::new(label).dimmed(), &mut renderer, &changed),
    );
    hot = hot.with_color(vello::peniko::Color::WHITE);
    assert_same_scene(&draw_current(&mut hot, &mut renderer, &changed), &explicit);
    assert_eq!(hot.drawn_color, Some(vello::peniko::Color::WHITE));
}

// Main already exposes the original full structural comparator. Retain every
// stream and late resource comparison; no count/hash-only replacement.
fn exact_scene_encoding_eq(left: &vello::Scene, right: &vello::Scene) -> bool {
    left.encoding_eq(right)
}
