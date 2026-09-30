//! The OS draws the window chrome; the app must NOT draw its own title / app-
//! name header (that would duplicate the compositor's window-group pill). This
//! guards the removal of the old in-pane **app-name** title and the sidebar's
//! "Personal Settings" title. A category page heading is intentional: it says
//! where the detail screen is, especially after a narrow-screen back affordance.
//!
//! It asserts on the real composed tree: it walks every widget's accessibility
//! node (a `Text`'s accessible name *is* its rendered string) and proves the
//! app name is not painted anywhere, while the title is still exposed the
//! sanctioned way — through `AppWidget::app_title`, which the OS reads for the
//! pill. The selected category's label and subtitle are deliberately rendered
//! as the content page's hierarchy, not as a duplicate app header.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use settings::SettingsApp;
use settings::categories::Category;
use ui::headless::Framed;
use ui::widget::Widget;
use ui::{AppWidget, IconDescriptor};

const W: f32 = 1200.0;
const H: f32 = 760.0;

fn lay_out(app: SettingsApp) -> Framed<SettingsApp> {
    Framed::new(
        ui::State::new(),
        app,
        ui::headless::viewport((W, H)),
        ui::Theme::dark(),
    )
}

/// Every accessible name rendered anywhere in the composed tree.
fn rendered_names(app: &SettingsApp) -> Vec<String> {
    fn walk(w: &dyn Widget, out: &mut Vec<String>) {
        if let Some(node) = w.accessibility()
            && let Some(name) = node.name
        {
            out.push(name);
        }
        for c in w.children() {
            walk(c, out);
        }
    }
    let mut out = Vec::new();
    walk(app, &mut out);
    out
}

/// The title is exposed through the app widget and no pane draws it.
#[test]
fn app_title_lives_only_in_the_window_chrome() {
    // Case `app_title_is_exposed_only_through_app_widget`.
    {
        let config = crate::support::ScratchConfig::new();
        // The sanctioned channel: the OS pill reads this. Present and correct.
        let app = config.app();
        assert_eq!(app.app_title(), "Personal Settings");
        assert!(matches!(app.app_icon(), IconDescriptor::Glyph(_)));
    }

    // Case `no_pane_draws_the_app_name_but_each_page_names_its_destination`.
    {
        let config = crate::support::ScratchConfig::new();
        // Across a representative set of categories, the app must never paint its
        // own name. It must paint the selected destination and its explanatory
        // subtitle so the detail pane stands on its own outside the sidebar.
        for &cat in &[
            Category::Network,
            Category::ExternalAccounts,
            Category::RegionFormats,
            Category::About,
        ] {
            let app = config.app().with_category(cat);
            let app = lay_out(app);
            let names = rendered_names(&app);

            assert!(
                !names.iter().any(|n| n == "Personal Settings"),
                "{cat:?}: the app must not draw its own \"Personal Settings\" app-name header \
             (the OS pill carries it); rendered names were {names:?}"
            );
            assert!(
                names.iter().any(|n| *n == cat.label()),
                "{cat:?}: the selected settings destination needs an in-content heading; \
             rendered names were {names:?}"
            );
            assert!(
                names.iter().any(|n| *n == cat.subtitle()),
                "{cat:?}: the heading needs its explanatory subtitle; rendered names were {names:?}"
            );
        }
    }
}
