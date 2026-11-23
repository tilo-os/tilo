//! A hosted window's press on a row that presents a resource: its type's default view beside the
//! list, or, when the app gives the type no view, the platform's page of the resource's facts.
//! A session's window holds the same instance the plain window does, over the patch connection.

use tilo_app_window::UiNode;

use super::{OPENBY, Session};

fn staged() -> Session {
    let mut session = Session::new(OPENBY, "ada");
    session.instance_mut().stage("two books");
    session.settle();
    session
}

/// Every fact of the page: its rows' labels and values, in order.
fn facts(node: &UiNode, out: &mut Vec<(String, String)>) {
    if node.kind == "ui.Property" {
        let text = |name: &str| match node.props.get(name) {
            Some(tilo_app_window::Value::Text(text)) => Some(text.clone()),
            _ => None,
        };
        if let (Some(label), Some(value)) = (text("label"), text("value")) {
            out.push((label, value));
        }
    }
    for child in &node.children {
        facts(child, out);
    }
}

#[test]
fn a_press_on_a_row_of_a_type_with_no_view_shows_its_facts_page() {
    let mut session = staged();
    let (x, y) = super::note_row(&mut session, "Favourite");
    session.window.click_at(x, y);
    session.settle();
    let page = session
        .instance()
        .instance()
        .detail_facts()
        .expect("the press opens the resource's page")
        .clone();
    let mut shown = Vec::new();
    facts(&page, &mut shown);
    assert!(
        shown.contains(&("Note".to_owned(), "Dog-eared".to_owned())),
        "the page says what the item holds, field by field: {shown:?}"
    );
    assert!(
        session.shown().iter().any(|text| text == "Label"),
        "and what it is: {:?}",
        session.shown()
    );
}
