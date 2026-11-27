//! A guided page holds one frame across the steps it shows: the body keeps one width whether
//! the step in front overflows the room or fits in it, so a control does not shift by the bar's
//! gutter from one page to the next.

use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::widget::{Column, GuidedPage, Spacer, Text};
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
}

/// A page in a card that holds it to its height scrolls its own step: the card's body keeps one
/// width as the steps change, as a page alone on a window does.
#[test]
fn a_card_holds_the_body_at_one_width_across_its_steps() {
    let short = body_width(GuidedPage::new(Spacer::new()).embedded().fills_height());
    let tall = body_width(GuidedPage::new(tall_page()).embedded().fills_height());
    assert!(
        (short - tall).abs() < 0.5,
        "the card's body is {short} wide on a short step and {tall} on a long one"
    );
}
