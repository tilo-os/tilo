//! A guided page holds one frame across the steps it shows: the body keeps one width whether
//! the step in front overflows the room or fits in it, so a control does not shift by the bar's
//! gutter from one page to the next.

use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::widget::{Column, GuidedPage, Row, Scrollable, Spacer, Stepper, Text};
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::Widget;

fn body_width(page: GuidedPage) -> f32 {
    let mut harness = Driver::new_with_theme(
        tilo_ui_app::State::new(),
        Box::new(page),
        (1180, 820),
        Theme::light(),
    );
    harness.relayout();
    let _ = harness.recompose();
    harness
        .with_widget_mut(|page: &mut GuidedPage| {
            let scroll = page.children()[0];
            scroll.children()[0].get_rect().width() as f32
        })
        .expect("the page stands as the root")
}

fn tall_page() -> Column {
    let mut tall = Column::new().with_intrinsic_height();
    for _ in 0..80 {
        tall = tall.push(Box::new(Text::new(
            "A line of a page taller than the room.",
        )));
    }
    tall
}

#[test]
fn the_body_keeps_one_width_whichever_step_shows() {
    let short = body_width(GuidedPage::new(Spacer::new()));
    let tall = body_width(GuidedPage::new(tall_page()));
    assert!(
        (short - tall).abs() < 0.5,
        "a short page's body is {short} wide and a long page's {tall}: the bar's gutter must not
         change the page's width between steps"
    );
    let metrics = Theme::light().tokens.metrics;
    let kept = f64::from(metrics.readable_width) - f64::from(metrics.scrollbar_gutter);
    assert!(
        (f64::from(short) - kept).abs() < 0.5,
        "the body keeps the bar's gutter: {short} wide, {kept} expected"
    );

    // A page in a card that holds it to its height scrolls its own step, and the card's body
    // keeps one width as the steps change, as a page alone on a window does.
    let short = body_width(GuidedPage::new(Spacer::new()).embedded().fills_height());
    let tall = body_width(GuidedPage::new(tall_page()).embedded().fills_height());
    assert!(
        (short - tall).abs() < 0.5,
        "the card's body is {short} wide on a short step and {tall} on a long one"
    );
}

/// A page inside a page that already scrolls, as a settings pane holds its wizard, stands at the
/// height of its parts and lets the outer page scroll: it takes the height it is offered only
/// when the surface holds it to one.
#[test]
fn an_embedded_page_in_a_scrolling_page_keeps_its_content_height() {
    let page = GuidedPage::new(tall_page())
        .embedded()
        .stepper(Stepper::new(vec!["One".to_owned(), "Two".to_owned()]))
        .actions(Row::new().push(Box::new(Text::new("Back"))));
    let content = Column::new().with_intrinsic_height().push(Box::new(page));
    let root = Scrollable::new_vertical()
        .fill_parent_height()
        .with_child(Box::new(content));
    let mut harness = Driver::new_with_theme(
        tilo_ui_app::State::new(),
        Box::new(root),
        (900, 700),
        Theme::light(),
    );
    harness.relayout();
    let _ = harness.recompose();
    let (height, parts) = harness
        .with_widget_mut(|page: &mut GuidedPage| {
            (page.get_rect().height() as f32, page.children().len())
        })
        .expect("the guided page stands in the page that scrolls");
    assert_eq!(parts, 3, "the track, the page and the ways forward");
    assert!(
        height > 600.0,
        "the embedded page stands at its parts' height, never at none of the surface's: {height}"
    );
}
