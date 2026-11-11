use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{Color, Fill};
use vello_encoding::MarkKind;

use super::*;

fn begins(scene: &Scene) -> Vec<(u64, [f32; 2])> {
    scene
        .encoding()
        .resources
        .marks
        .iter()
        .filter_map(|mark| match mark.kind {
            MarkKind::Begin { key, origin } => Some((key, origin.translation)),
            MarkKind::End => None,
        })
        .collect()
}

fn square(scene: &mut Scene, transform: Affine) {
    scene.fill(
        Fill::NonZero,
        transform,
        Color::from_rgb8(1, 2, 3),
        None,
        &Rect::new(0.0, 0.0, 4.0, 4.0),
    );
}

#[test]
fn keyed_items_survive_an_append_and_move_with_its_transform() {
    key_widgets(Some(keyer()));
    begin_frame();
    let mut cached = Scene::new();
    keyed(&mut cached, 7, Affine::translate((10.0, 20.0)), |scene| {
        square(scene, Affine::IDENTITY);
    });
    let mut frame = Scene::new();
    square(&mut frame, Affine::IDENTITY);
    frame.append(&cached, Some(Affine::translate((100.0, 0.0))));
    key_widgets(None);

    let marks = &frame.encoding().resources.marks;
    assert_eq!(marks.len(), 2);
    // After the frame's own square: one draw tag in.
    assert_eq!(marks[0].draw_tags, 1);
    assert_eq!(marks[1].draw_tags, 2);
    assert_eq!(begins(&frame), vec![(7, [110.0, 20.0])]);
}

#[test]
fn nothing_is_marked_while_no_host_asks_for_keys() {
    key_widgets(None);
    let mut scene = Scene::new();
    keyed(&mut scene, 7, Affine::IDENTITY, |scene| {
        square(scene, Affine::IDENTITY);
    });
    assert!(scene.encoding().resources.marks.is_empty());
}

#[test]
fn an_item_key_names_its_place_under_the_open_key() {
    key_widgets(Some(keyer()));
    begin_frame();
    let mut scene = Scene::new();
    let (inside_a, inside_b) = (
        keyed(&mut scene, 1, Affine::IDENTITY, |_| item_key("row", &3)),
        keyed(&mut scene, 2, Affine::IDENTITY, |_| item_key("row", &3)),
    );
    let again = keyed(&mut scene, 1, Affine::IDENTITY, |_| item_key("row", &3));
    key_widgets(None);
    assert_ne!(inside_a, inside_b);
    assert_eq!(inside_a, again);
}

#[test]
fn reset_clears_the_marks() {
    key_widgets(Some(keyer()));
    let mut scene = Scene::new();
    keyed(&mut scene, 1, Affine::IDENTITY, |scene| {
        square(scene, Affine::IDENTITY)
    });
    key_widgets(None);
    scene.reset();
    assert!(scene.encoding().resources.marks.is_empty());
}

/// Every widget a container draws itself, not only those drawn through the
/// flow containers, carries a key of its own: a card grid draws its cards and
/// each card its body.
#[test]
fn a_container_that_draws_its_children_itself_keys_each_of_them() {
    use crate::presence::PresenceCtx;
    use crate::text_render::TextRenderer;
    use crate::theme::Theme;
    use crate::widget::{Button, Card, CardGrid, DrawChild, Element, Widget};

    let cards: Vec<Element> = (0..3)
        .map(|i| {
            Box::new(Card::new(Box::new(Button::new(
                &format!("card-{i}"),
                &format!("Card {i}"),
            )))) as Element
        })
        .collect();
    let mut grid: Element = Box::new(CardGrid::new(cards));
    let theme = Theme::default();
    let mut text = TextRenderer::new();
    crate::widget::layout::lay_out_detached(
        grid.as_mut(),
        Rect::new(0.0, 0.0, 800.0, 600.0),
        &theme,
        &mut text,
    )
    .unwrap();

    key_widgets(Some(keyer()));
    begin_frame();
    let mut scene = Scene::new();
    grid.draw_child(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        &theme,
        &PresenceCtx::default(),
    );

    fn keys(widget: &dyn Widget, out: &mut Vec<(&'static str, Option<String>)>) {
        out.push((widget.concrete_type_name(), painted_key(widget)));
        widget.for_each_child(&mut |child: &dyn Widget| keys(child, out));
    }
    let mut drawn = Vec::new();
    keys(grid.as_ref(), &mut drawn);
    key_widgets(None);

    let unkeyed: Vec<_> = drawn.iter().filter(|(_, key)| key.is_none()).collect();
    assert!(unkeyed.is_empty(), "drawn without a key: {unkeyed:?}");
    let distinct: HashSet<_> = drawn.iter().map(|(_, key)| key.clone()).collect();
    assert_eq!(distinct.len(), drawn.len(), "{drawn:?}");
    assert!(
        drawn.len() >= 7,
        "the grid, three cards and their buttons: {drawn:?}"
    );
}
