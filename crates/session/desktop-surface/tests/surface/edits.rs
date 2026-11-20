//! A page sends its shell to the workspace only when the shell changed ([`DesktopHandle::revision`]):
//! every change a peer sees counts as one, and a frame that changes nothing counts none.

use super::*;

/// A window change by what it is called, and the verb that makes it.
type Change<'a> = (&'static str, Box<dyn Fn() -> Verb + 'a>);

/// The tree as a peer reads it.
fn shared(handle: &DesktopHandle) -> serde_json::Value {
    serde_json::to_value(handle.shared_tree()).unwrap()
}

/// Every kind of window change a person or agent makes is counted, so a page sends each one; the
/// frames between them, nothing touched, count nothing, so a page sends nothing then.
#[test]
fn each_window_change_counts_and_an_idle_frame_counts_none() {
    let (mut driver, handle, _) = mount();
    let route = |index: usize| handle.snapshot("g", vec![]).entries[index].route.clone();
    let changes: Vec<Change<'_>> = vec![
        (
            "open an app",
            Box::new(|| {
                ChromeEffect::OpenApp {
                    app: "so.tilo.sheets".into(),
                    beside: None,
                    background: None,
                    float: None,
                }
                .into()
            }),
        ),
        (
            "focus a window",
            Box::new(|| LayoutEffect::Focus { tab: route(0).tab }.into()),
        ),
        (
            "set a window's width",
            Box::new(|| {
                ChromeEffect::SetWidth {
                    group: route(0).group,
                    permille: Some(400),
                    preset: None,
                }
                .into()
            }),
        ),
        (
            "maximize a window",
            Box::new(|| {
                ChromeEffect::Maximize {
                    group: route(0).group,
                    on: true,
                }
                .into()
            }),
        ),
        (
            "split a tab into a window of its own",
            Box::new(|| ChromeEffect::NewGroup { tab: route(1).tab }.into()),
        ),
        (
            "close a tab",
            Box::new(|| LayoutEffect::CloseTab { tab: route(0).tab }.into()),
        ),
        (
            "close a window",
            Box::new(|| {
                ChromeEffect::CloseWindow {
                    group: route(0).group,
                }
                .into()
            }),
        ),
    ];
    for (what, change) in changes {
        let (edits, seen) = (handle.revision(), shared(&handle));
        dispatch(&mut driver, &handle, change());
        assert!(handle.revision() > edits, "{what} counts as a change");
        if shared(&handle) == seen {
            // Only this device's view moved (focus, width): a peer sees the same tree, which the
            // page finds out by reading it once.
            continue;
        }
        let edits = handle.revision();
        driver.relayout();
        driver.relayout();
        assert_eq!(
            handle.revision(),
            edits,
            "the frames after {what} count none"
        );
    }

    // A tree another member changed, renamed here, is counted as its own change.
    let mut tree = handle.read(Clone::clone);
    tree.workspaces[0].strips[0].window_groups[0].tabs[0].title = "Renamed".into();
    let edits = handle.revision();
    handle.replace_tree(tree);
    driver.relayout();
    assert!(handle.revision() > edits, "a tree from a peer counts");
    assert!(shared(&handle).to_string().contains("Renamed"));

    let edits = handle.revision();
    for _ in 0..5 {
        driver.relayout();
    }
    assert_eq!(handle.revision(), edits, "an idle shell counts nothing");
}
