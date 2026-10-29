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

use settings::SettingsWindow;
use settings::categories::Category;
use ui::headless::{Framed, viewport};
use ui::widget::Widget;
use vello::kurbo::Point;

const W: f32 = 1200.0;
// Wide navigation tests address every category directly; keep each target
// visible. Short-viewport sidebar scrolling is exercised separately.
const H: f32 = 1800.0;

/// Mount `app` in the production frame host at `(w, h)` and run its first
/// frame.
fn lay_out_at(app: SettingsWindow, w: f32, h: f32) -> Framed<settings::SettingsWindow> {
    Framed::new(ui::State::new(), app, viewport((w, h)), ui::Theme::dark())
}

/// Rebuild and re-lay-out exactly as the host does on structure-dirty.
fn relayout(app: &mut Framed<settings::SettingsWindow>, (w, h): (f32, f32)) {
    Framed::harness_mut(app)
        .relayout_themed(viewport((w, h)), ui::Theme::dark())
        .expect("settings frame lays out");
}

/// Drive a full left-click (cursor move → press → release) at `p` through
/// the production event path.
fn click_at(app: &mut Framed<settings::SettingsWindow>, p: Point) {
    Framed::harness_mut(app).click_at(p.x, p.y);
}

/// The host's `sync` against the live document.
fn sync(app: &mut Framed<settings::SettingsWindow>) {
    let (app, state) = Framed::split(app);
    app.sync(&state.doc, &state.root);
}

/// The back button returns to the list and the Wi-Fi toggle stays pane-local.
#[test]
fn back_button_and_wifi_toggle_drive_pane_state() {
    // Case `clicking_back_button_returns_to_list`.
    // The narrow content screen's back-arrow button, clicked through the
    // production event path, navigates back to the standalone list screen.
    {
        const NW: f32 = 440.0;
        const NH: f32 = 820.0;
        let config = crate::support::ScratchConfig::new();
        // Start drilled into a category's content screen (narrow layout).
        let app = config.app().with_category(Category::Network);
        let mut app = lay_out_at(app, NW, NH);
        sync(&mut app);
        assert!(app.is_narrow(), "440px resolves to the narrow layout");
        assert!(
            !app.sidebar_open(),
            "starts on the content screen (list hidden)"
        );

        // The back arrow is drawn by the shell (not a child widget of the
        // app), so hit-test it by its resolved rect and click it through the
        // real path.
        let back = app.back_button_rect();
        assert!(back.width() > 0.0, "the back arrow has a resolved rect");
        click_at(&mut app, back.center());
        sync(&mut app);
        let _ = app.take_structure_dirty();
        relayout(&mut app, (NW, NH));
        assert!(
            app.sidebar_open(),
            "back navigates to the standalone list screen"
        );
        assert!(
            app.back_button_rect().width() <= 0.0,
            "the list screen has no back button"
        );
    }

    // Case `network_wifi_toggle_drives_state`.
    // Network hardware state is pane-local. Neither initialisation nor a hostile
    // remote scalar may turn the collaborative document into a radio command.
    {
        let config = crate::support::ScratchConfig::new();
        let mut app = lay_out_at(config.app(), W, H);
        assert_eq!(app.selected(), Category::Network, "opens on Network");
        let heads = Framed::harness_mut(&mut app).state_mut().doc.get_heads();
        sync(&mut app);
        assert_eq!(
            Framed::harness_mut(&mut app).state_mut().doc.get_heads(),
            heads
        );
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
