//! The rich text line as a view declares it: read from its props and built as the component.

use tilo_rich_edit::{DeclaredLine, RichTextLineProps};
use tilo_ui::component::testing::{Node, text};
use tilo_ui::component::{Component as _, Described as _};
use tilo_ui::testing::component_harness::harness;
use tilo_ui::widget::Widget as _;
use tilo_ui_graph::component::Value;

#[test]
fn a_rich_text_line_edits_its_bound_text() {
    let queue = tilo_ui::Intents::default();
    let mut node = Node::new("RichTextLine", "line", &queue)
        .prop("placeholder", text("Write here"))
        .prop("role", Value::Choice("body".into()))
        .on("enter");
    let props = RichTextLineProps::from_source(&mut node).expect("reads");
    let mut host = harness(DeclaredLine::from_props("line", props));
    assert_eq!(
        host.root_mut::<DeclaredLine>().line().name(),
        "RichTextLine"
    );
}
