//! Every tab names the whole of its title in a tooltip.

use super::super::{TabBar, TabDescriptor};
use super::tests::lay_out_at_width_with;
use crate::widget::Widget;

#[test]
fn a_tab_names_its_whole_title_in_a_tooltip() {
    let _settled = crate::motion::testing::settled();
    let theme = crate::theme::Theme::dark().with_reduce_motion(true);
    let long = "A note whose title is far longer than its pill";
    let mut b = TabBar::new("g1", vec![TabDescriptor::new(1, long)], 1);
    lay_out_at_width_with(&mut b, 900.0, &theme);
    let (rect, title) = b
        .tooltip_target_at(b.tab_rects()[0].center())
        .expect("the pill names its tab");
    assert_eq!(title, long);
    assert_eq!(rect, b.tab_rects()[0]);
}
