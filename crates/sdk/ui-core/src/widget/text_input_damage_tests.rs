use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::Color;

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{TextInput, Widget};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 260;

/// The pixels (x, y) where `before` and `after` differ that no rect of
/// `damage` touches.
fn unrepainted(before: &[u8], after: &[u8], damage: &[Rect]) -> Vec<(u32, u32)> {
    let width = usize::try_from(WIDTH).unwrap();
    before
        .chunks_exact(4)
        .zip(after.chunks_exact(4))
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(index, _)| {
            let x = u32::try_from(index % width).unwrap();
            let y = u32::try_from(index / width).unwrap();
            (x, y)
        })
        .filter(|&(x, y)| {
            let pixel = Rect::new(
                f64::from(x),
                f64::from(y),
                f64::from(x + 1),
                f64::from(y + 1),
            );
            !damage.iter().any(|rect| rect.intersect(pixel).area() > 0.0)
        })
        .collect()
}

#[test]
fn an_edited_line_repaints_its_ink_that_rises_past_its_row() {
    let _settled = crate::motion::testing::settled();
    let theme = Theme::dark();
    let presence = crate::presence::PresenceCtx::empty();
    let mut renderer = TextRenderer::new();
    renderer.set_text_scale(theme.text_scale);
    // The marked line's stacked accents rise through the rows above it;
    // the first line is far enough above to stay clear of them.
    let marks = "\u{301}".repeat(10);
    let mut field = TextInput::local()
        .multiline(true)
        .with_initial_value(&format!("top\n\n\n\n\n\nA{marks}a{marks}\nlast"));
    field.apply_layout_external(Rect::new(0.0, 0.0, f64::from(WIDTH), f64::from(HEIGHT)));
    let mut draw = |field: &mut TextInput| {
        let mut scene = Scene::new();
        Widget::draw(
            field,
            &mut scene,
            Affine::IDENTITY,
            &mut renderer,
            &theme,
            &presence,
        );
        scene
    };
    let shown = draw(&mut field);
    assert!(field.multiline_shaped);
    Widget::mark_drawn(&mut field);
    field.set_text("top\n\n\n\n\n\nAa\nlast");
    let edited = draw(&mut field);
    let mut damage = Vec::new();
    Widget::collect_damage(&field, &mut damage);

    let layout = field.single_layout.as_ref().expect("a shaped layout");
    let rows: Vec<(f64, f64)> = layout
        .buffer
        .layout_runs()
        .map(|run| {
            let top = field.single_origin.y + f64::from(run.line_top);
            (top, top + f64::from(run.line_height))
        })
        .collect();
    let (first_top, first_bottom) = rows[0];
    let (edited_top, _) = rows[6];
    assert!(
        damage
            .iter()
            .all(|rect| rect.y1 <= first_top || rect.y0 >= first_bottom),
        "the edit repaints its line, not the field: {damage:?}"
    );

    let mut gpu = crate::offscreen::OffscreenRenderer::try_new_for_pixel_test(WIDTH, HEIGHT, true)
        .expect("pixel test renderer");
    let before = gpu.render(&shown, Color::BLACK);
    let after = gpu.render(&edited, Color::BLACK);
    let width = usize::try_from(WIDTH).unwrap();
    let above = before
        .chunks_exact(4)
        .zip(after.chunks_exact(4))
        .enumerate()
        .filter(|(index, (a, b))| {
            let y = f64::from(u32::try_from(index / width).unwrap());
            a != b && y + 1.0 < edited_top
        })
        .count();
    assert!(
        above > 0,
        "the accents the edit took away painted above the edited line's row"
    );
    let missed = unrepainted(&before, &after, &damage);
    assert!(
        missed.is_empty(),
        "every pixel the edit changed is repainted; not {} of them, such as {:?} \
         (damage {damage:?}, edited row from y {edited_top})",
        missed.len(),
        &missed[..missed.len().min(8)]
    );
}
