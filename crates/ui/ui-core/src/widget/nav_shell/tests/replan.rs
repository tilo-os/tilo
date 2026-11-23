//! A layout switch the shell takes after its parts were laid out.

use super::*;

/// Larger text floats the sidebar as a drawer; back at the stock size it docks again.
#[test]
fn docking_again_after_larger_text_lays_the_content_out_where_the_shell_says() {
    let mut theme = Theme::light();
    theme.reduce_motion = true;
    let mut h = harness(
        mail_shell().content_kind(ContentKind::Document),
        1280,
        800,
        theme.clone(),
    );
    h.root_mut::<NavShell>().show(NavPane::Content);
    for text in [1.0, 1.5, 1.0] {
        h.relayout_themed((1280, 800), theme.clone().with_text_scale(text))
            .expect("layout");
        h.relayout_at((1280, 800)).expect("layout");
        let column = geometry(&h).body(Column::Content);
        let content = child_rect(&h, "content").expect("the content");
        assert!(
            (content.x0 - column.x0).abs() < 0.5 && (content.x1 - column.x1).abs() < 0.5,
            "at text {text} the content stands in its column {column:?}, not at {content:?}"
        );
        assert!(
            content.x1 <= 1280.5,
            "inside the window at text {text}: {content:?}"
        );
    }
}
