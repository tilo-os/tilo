//! What asks for the shell's next pass from outside a pass, and when a change is shared.

use super::*;

/// A tree another member changed and a command the chrome or an agent dispatched each ask for the
/// pass that shows them, with no pass running: a page that draws only when asked shows them at
/// once.
#[test]
fn a_change_from_outside_a_pass_asks_for_its_pass() {
    let (_driver, handle, _) = mount();
    let woken = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    handle.set_wake({
        let woken = std::sync::Arc::clone(&woken);
        std::sync::Arc::new(move || {
            woken.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        })
    });
    let wakes = || woken.load(std::sync::atomic::Ordering::Relaxed);

    let before = wakes();
    handle.replace_tree(handle.read(Clone::clone));
    assert!(
        wakes() > before,
        "a tree changed elsewhere asks for its pass"
    );

    let before = wakes();
    let routes = handle.snapshot("g", vec![]).entries;
    handle
        .dispatch(verb(
            "focus",
            serde_json::json!({"tab":routes[1].route.tab}),
        ))
        .unwrap();
    assert!(wakes() > before, "a dispatched command asks for its pass");
}

/// A window opened here reaches the workspace's shared desktop from the pass that opens it, before
/// that pass builds the window's app: the tree with the window in it is shared first, so another
/// person's page shows it without waiting for this page's slow frame.
#[test]
fn an_opened_window_is_shared_before_its_app_is_built() {
    let (mut driver, handle, built) = mount();
    let shared: Rc<std::cell::RefCell<Vec<(usize, usize)>>> = Rc::default();
    handle.set_publish({
        let shared = Rc::clone(&shared);
        let built = Rc::clone(&built);
        Rc::new(move |handle| {
            let windows = handle.snapshot("g", vec![]).entries.len();
            shared.borrow_mut().push((windows, built.get()));
        })
    });
    let before = built.get();
    dispatch(
        &mut driver,
        &handle,
        verb("open-app", serde_json::json!({"app":"so.tilo.sheets"})),
    );
    assert_eq!(built.get(), before + 1, "the pass built the window's app");
    assert_eq!(
        shared.borrow().first().copied(),
        Some((3, before)),
        "the window was shared before its app was built: {:?}",
        shared.borrow()
    );
}
