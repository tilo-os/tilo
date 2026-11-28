//! A secret a view reveals hides again when its window loses the person's focus: a `ui.Property`
//! value or a `ui.Text` the app says is a secret masks itself, and assistive technology reads
//! only that it is hidden, until the app sends a value it has not shown before.

use crate::hosted::{SECRET, Session, accessible, shown_at};

/// Every word the window publishes to assistive technology: each node's name and value.
fn spoken(session: &mut Session) -> String {
    let mut out = String::new();
    for node in accessible(session) {
        if let Some(name) = node.name {
            out.push_str(&name);
            out.push('\n');
        }
        if let Some(value) = node.value {
            out.push_str(&value);
            out.push('\n');
        }
    }
    out
}

#[test]
fn a_revealed_secret_hides_when_its_window_loses_focus() {
    let _settled = ui::motion::testing::settled();
    let mut session = Session::new(SECRET, "ada");
    session.settle();

    // The person asks for the secret: the property's value and the text beside it are shown.
    let (x, y) = shown_at(&mut session, "Reveal");
    session.window.click_at(x, y);
    session.settle();
    let shown = spoken(&mut session);
    assert!(shown.contains("482913"), "the property shows it: {shown:?}");
    assert!(
        shown.contains("SOLO-SECRET-CODE"),
        "the text shows it: {shown:?}"
    );

    // The window loses the focus: neither reaches a frame or a reader any more.
    session.window.focus(false);
    session.settle();
    let hidden = spoken(&mut session);
    assert!(
        !hidden.contains("482913"),
        "the property's secret is hidden: {hidden:?}"
    );
    assert!(
        !hidden.contains("SOLO-SECRET-CODE"),
        "the text's secret is hidden: {hidden:?}"
    );

    // Taking the focus back does not show it again: only a value the app sends anew does.
    session.window.focus(true);
    session.settle();
    let refocused = spoken(&mut session);
    assert!(
        !refocused.contains("482913") && !refocused.contains("SOLO-SECRET-CODE"),
        "a refocus keeps it hidden: {refocused:?}"
    );

    // Hide masks the values, and Reveal sends them again: a value the window has not shown
    // before shows while it has the focus.
    let (x, y) = shown_at(&mut session, "Hide");
    session.window.click_at(x, y);
    session.settle();
    let (x, y) = shown_at(&mut session, "Reveal");
    session.window.click_at(x, y);
    session.settle();
    let fresh = spoken(&mut session);
    assert!(
        fresh.contains("482913") && fresh.contains("SOLO-SECRET-CODE"),
        "a new value shows again: {fresh:?}"
    );
}
