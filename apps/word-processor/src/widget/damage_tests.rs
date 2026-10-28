//! The editor paints its regions as parts and reports as damage the regions
//! whose drawing changed, so a host repaints a typed character's paragraph
//! (and the word count) rather than the window.

use super::*;
use ui::PresenceCtx;

/// A typed character damages its paragraph's band on the page and the
/// layout bar's word count, nothing else; a resize damages the window.
#[test]
fn a_typed_character_damages_its_paragraph_and_a_resize_the_window() {
    let mut state = crate::initialized_state();
    let mut wp = WordProcessorWidget::detached();
    wp.sync(&state.doc, &state.root);
    let mut edits = ui::widget::Edits::lend(&mut state.doc);
    wp.model_mut().insert_str(&mut edits, "Title");
    wp.model_mut().enter(&mut edits);
    wp.model_mut().insert_str(&mut edits, "Body text here");
    let body = wp.model_mut().caret().clone();
    wp.model_mut().enter(&mut edits);
    wp.model_mut().insert_str(&mut edits, "A last paragraph");
    // The caret goes back into the body paragraph, which the typing edits.
    wp.model_mut().set_caret(body);
    wp.sync(&state.doc, &state.root);

    let (w, h) = (1200.0, 800.0);
    // One text renderer across frames, as a host keeps: its fonts are the
    // same fonts from frame to frame.
    let mut tr = TextRenderer::new();
    let theme = Theme::dark();
    let presence = PresenceCtx::default();
    let mut frame = |wp: &mut WordProcessorWidget, w: f64, h: f64| {
        wp.rect = Rect::new(0.0, 0.0, w, h);
        let mut scene = Scene::new();
        wp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        let mut damage = Vec::new();
        wp.collect_damage(&mut damage);
        wp.mark_drawn();
        damage
    };
    for _ in 0..3 {
        frame(&mut wp, w, h);
    }
    let still = frame(&mut wp, w, h);
    assert!(
        still.is_empty(),
        "an unchanged frame damages nothing: {still:?}"
    );

    let block = wp.inner.caret.block.clone();
    wp.model_mut()
        .insert_str(&mut ui::widget::Edits::lend(&mut state.doc), "!");
    wp.sync(&state.doc, &state.root);
    wp.lifecycle.dirty = true;
    let damage = frame(&mut wp, w, h);
    let laid = wp
        .laid
        .iter()
        .find(|laid| laid.id == block)
        .expect("the edited paragraph is laid out");
    let to_widget = wp.zoom.transform(Affine::IDENTITY);
    let paragraph = to_widget.transform_rect_bbox(Rect::new(
        0.0,
        laid.y_top - 16.0,
        w,
        laid.y_top + laid.height + 16.0,
    ));
    // The layout bar counts the words, so it changes with them.
    let counts = Rect::new(0.0, h - wp.layout_bar_height, w, h).inflate(8.0, 8.0);
    assert!(
        damage
            .iter()
            .any(|rect| paragraph.union(*rect) == paragraph),
        "the typed character is repainted: {damage:?}"
    );
    for rect in &damage {
        assert!(
            paragraph.union(*rect) == paragraph || counts.union(*rect) == counts,
            "{rect:?} is outside the edited paragraph's band {paragraph:?} and the layout bar {counts:?}"
        );
    }

    let wider = Rect::new(0.0, 0.0, w + 100.0, h);
    let damage = frame(&mut wp, w + 100.0, h);
    assert!(
        damage.iter().any(|rect| rect.union(wider) == *rect),
        "a resize repaints the window: {damage:?}"
    );
}
