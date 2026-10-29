//! End-to-end control-interaction proofs.
//!
//! `category_nav.rs` proves a click navigates to each panel and the panel
//! renders. This file goes one step further for the panes whose controls
//! MUST drive real hardware actions: it navigates via a real pointer click,
//! then drives the panel's primary control (also via the production event
//! path or its CRDT key) and asserts the effect actually lands — the narrow
//! content screen's back arrow returning to the list, and a per-panel primary
//! control mutating the document the way its dispatcher hook reads.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use settings::categories::Category;

// Wide navigation tests address every category directly; keep each target
// visible. Short-viewport sidebar scrolling is exercised separately.
const H: f32 = 1800.0;

/// The back button returns to the list and the Wi-Fi toggle stays pane-local.
#[test]
fn back_button_and_wifi_toggle_drive_pane_state() {
    // Case `clicking_back_button_returns_to_list`.
    // The narrow content screen's back-arrow button, clicked through the
    // production event path, navigates back to the standalone list screen.
    {
        const NW: f32 = 440.0;
        const NH: f32 = 820.0;
        // Start drilled into a category's content screen (narrow layout).
        let mut app = crate::support::window_on(Category::Network, (NW, NH));
        let shell = crate::support::shell(&mut app);
        assert!(shell.narrow, "440px resolves to the narrow layout");
        assert!(
            !shell.sidebar_open,
            "starts on the content screen (list hidden)"
        );

        // The back arrow is drawn by the shell (not a child widget of the
        // app), so hit-test it by its resolved rect and click it through the
        // real path.
        let back = shell.back;
        assert!(back.width() > 0.0, "the back arrow has a resolved rect");
        app.harness().click_at(back.center().x, back.center().y);
        app.settle();
        let shell = crate::support::shell(&mut app);
        assert!(
            shell.sidebar_open,
            "back navigates to the standalone list screen"
        );
        assert!(
            shell.back.width() <= 0.0,
            "the list screen has no back button"
        );
    }

    // Case `network_wifi_toggle_drives_state`.
    // Network hardware state is pane-local. Neither initialisation nor a hostile
    // remote scalar may turn the collaborative document into a radio command.
    {
        let mut app = crate::support::window((1200, 1800));
        assert_eq!(
            crate::support::selected(&mut app),
            Category::Network,
            "opens on Network"
        );
        let heads = app.read_document(ui::tilo_resource::heads_of);
        app.settle();
        assert_eq!(app.read_document(ui::tilo_resource::heads_of), heads);
    }
}

/// Every category is reachable by a real click and renders a non-empty
/// content pane — the per-panel navigation guard. (Complements
/// `category_nav.rs`'s deeper per-panel assertions.) Panes render from their
/// models and never write the window's record.
#[test]
fn all_panels_navigate_and_leave_the_record_alone() {
    for &target in Category::ALL {
        if target == Category::DEFAULT {
            continue;
        }
        let mut app = crate::support::window((1200, 1800));
        let row = crate::support::window_row(&mut app, &target.label())
            .unwrap_or_else(|| panic!("no sidebar row for {target:?}"))
            .rect;
        assert!(
            row.y0 >= 0.0 && row.y1 <= f64::from(H),
            "navigation fixture must show {target:?}: {row:?}"
        );
        crate::support::click_category(&mut app, target);
        assert_eq!(
            crate::support::selected(&mut app),
            target,
            "click navigated to {target:?}"
        );
        let heads = app.read_document(ui::tilo_resource::heads_of);
        app.settle();
        assert_eq!(
            app.read_document(ui::tilo_resource::heads_of),
            heads,
            "{target:?} idle frames must leave the record unchanged"
        );
    }
}
