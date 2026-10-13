//! Search field focus + filter, driven through the production event path.
//!
//! This is the regression guard for the "clicking the search bar does nothing"
//! bug. The root cause was that the field lived inside the `NavSplit`, whose
//! children are laid out in an isolated inner taffy tree the host's focus walk
//! never visits — so the field was never in the focus order, a click's focus
//! could not be restored after the per-keystroke filter rebuild, and typing
//! died after one character. The fix hoists the field into the app's top strip,
//! in the *main* taffy tree.

use settings::SettingsApp;
use ui::headless::Framed;
use ui::widget::Widget;

const SEARCH: &str = "settings.search";

fn hosted(config: &crate::support::ScratchConfig) -> Framed<SettingsApp> {
    Framed::new(
        ui::State::new(),
        config.app(),
        ui::headless::viewport((1200.0, 760.0)),
        ui::Theme::dark(),
    )
}

fn search_field(root: &dyn Widget) -> Option<&dyn Widget> {
    if root.test_id() == Some(SEARCH) {
        return Some(root);
    }
    root.children().into_iter().find_map(search_field)
}

fn sidebar_row_count(root: &dyn Widget) -> usize {
    let own = root
        .as_any()
        .and_then(|value| value.downcast_ref::<ui::widget::NavList>())
        .map_or(0, |list| list.visible_item_ids().len());
    root.children()
        .into_iter()
        .map(sidebar_row_count)
        .fold(own, usize::saturating_add)
}

fn search_focused(h: &Framed<SettingsApp>) -> bool {
    search_field(&**h)
        .expect("the search field exists")
        .is_focused()
}

/// The search field is top-level focusable, focuses on click and keeps focus while typing.
#[test]
fn search_field_is_focusable_and_keeps_focus_while_filtering() {
    // The structural fix: the field lives in the main taffy tree, so the
    // host's focus walk reaches it. (Buried in `NavSplit` it never would.)
    {
        let config = crate::support::ScratchConfig::new();
        let mut h = hosted(&config);
        assert!(
            !search_focused(&h),
            "the field starts unfocused (no autofocus on launch)"
        );
        let stops = Framed::harness(&h).focus_order().len();
        let reached = (0..stops).any(|_| {
            Framed::harness_mut(&mut h).tab(false);
            search_focused(&h)
        });
        assert!(
            reached,
            "Tab reaches the search field (it is not trapped inside NavSplit's isolated inner tree)"
        );
    }

    // Clicking the field focuses it.
    {
        let config = crate::support::ScratchConfig::new();
        let mut h = hosted(&config);
        Framed::harness_mut(&mut h).click_named(SEARCH).unwrap();
        assert!(
            search_focused(&h),
            "clicking the search field must focus it"
        );
    }

    // Typing filters the sidebar and keeps focus.
    {
        let config = crate::support::ScratchConfig::new();
        let mut h = hosted(&config);
        let before = sidebar_row_count(&*h);
        assert!(before > 3, "the unfiltered sidebar lists every category");

        let harness = Framed::harness_mut(&mut h);
        harness.click_named(SEARCH).unwrap();
        harness.type_text("net");

        assert_eq!(h.search_query(), "net", "each typed char reached the query");
        let after = sidebar_row_count(&*h);
        assert!(
            after < before && after >= 1,
            "typing filtered the sidebar (before={before}, after={after})"
        );

        // The field survived every per-keystroke rebuild: it is still focused
        // and holds the full typed text (the caret never got dropped, so all
        // three characters landed — the exact thing the old bug broke).
        let field = search_field(&*h).expect("field still present after filtering");
        assert!(field.is_focused(), "focus survived the filter rebuilds");
        assert_eq!(
            field.text_review().map(|(_, t)| t),
            Some("net".to_string()),
            "the field holds the full query it accumulated across rebuilds"
        );
    }
}
