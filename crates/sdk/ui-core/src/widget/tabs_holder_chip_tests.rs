//! The chip naming who opened a tab: below it, alone, clear of the bar's buttons.

use super::super::tests::{holder, lay_out_at_width};
use super::super::{TabBar, TabDescriptor, TabHolder};
use crate::text_render::TextRenderer;
use vello::kurbo::Rect;

#[test]
fn the_chip_naming_who_opened_a_tab_stands_alone_below_it() {
    let theme = crate::theme::Theme::dark();
    let mut tr = TextRenderer::new();
    let mut b = TabBar::new(
        "g1",
        vec![
            TabDescriptor::new(1, "A"),
            TabDescriptor::new(2, "B"),
            TabDescriptor::new(3, "C"),
        ],
        1,
    );
    lay_out_at_width(&mut b, 1200.0);
    let window = Rect::new(0.0, 0.0, 1200.0, 800.0);
    let pill = b.tab_rects[1];
    assert!(
        b.tip_at(pill.center()).is_some(),
        "a tab nobody else opened names itself"
    );
    b.set_holder(
        2,
        Some(TabHolder {
            keeps: false,
            ..holder()
        }),
    );
    b.hover_open_for_tests(1);
    let (chip, label, _) = b
        .holder_chip(&mut tr, &theme, window)
        .expect("the chip names who opened the tab");
    assert_eq!(label, "Ben opened this window");
    assert!(
        b.tip_at(pill.center()).is_none(),
        "the chip and the tab's own tip never show together"
    );
    assert!(chip.y0 >= pill.y1, "below the tab: {chip:?} under {pill:?}");
    let buttons = [b.new_tab_rect, b.controls.close, b.controls.maximize]
        .into_iter()
        .chain(b.tab_rects.iter().copied())
        .filter(|r| r.area() > 0.0);
    for button in buttons {
        assert!(
            chip.intersect(button).area() <= 0.0,
            "the chip {chip:?} covers the bar's {button:?}"
        );
    }
}
