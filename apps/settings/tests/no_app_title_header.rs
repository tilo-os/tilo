//! The OS draws the window chrome; the app must NOT draw its own title / app-
//! name header (that would duplicate the compositor's window-group pill). This
//! guards the removal of the old in-pane **app-name** title and the sidebar's
//! "Settings" title. The category's name is the page's heading, in
//! the shell's header row: it says where the detail screen is, especially
//! after a narrow-screen back affordance.
//!
//! It asserts on the real composed tree: it walks every widget's accessibility
//! node (a `Text`'s accessible name *is* its rendered string) and proves the
//! app name is not painted anywhere, while the title is still exposed the
//! sanctioned way — through the app's `ResourceApp::TITLE`, which the host
//! hands the OS for the pill. The selected category's label is the page's
//! heading, not a duplicate app header, and there is no subtitle under it.

use settings::SettingsShell;
use settings::categories::Category;
use ui::framework::ResourceApp;
use ui::widget::{NavShell, Widget};

const W: f32 = 1200.0;
const H: f32 = 760.0;

/// Every accessible name rendered anywhere in the composed tree under the
/// app's landmark, including the nodes a widget publishes for parts it
/// paints itself (the shell's column headings).
fn rendered_names(shell: &dyn Widget) -> Vec<String> {
    fn collect(node: ui::a11y::AccessNode, out: &mut Vec<String>) {
        if let Some(name) = node.name.clone() {
            out.push(name);
        }
        for child in node.children {
            collect(child, out);
        }
    }
    fn walk(w: &dyn Widget, out: &mut Vec<String>) {
        if let Some(node) = w.accessibility() {
            collect(node, out);
        }
        for c in w.children() {
            walk(c, out);
        }
    }
    let mut out = Vec::new();
    walk(shell, &mut out);
    out
}

/// Whether the tree publishes a heading named `label`.
fn has_heading(shell: &dyn Widget, label: &str) -> bool {
    fn found(node: &ui::a11y::AccessNode, label: &str) -> bool {
        (matches!(node.role, ui::a11y::Role::Heading { .. }) && node.name.as_deref() == Some(label))
            || node.children.iter().any(|child| found(child, label))
    }
    fn walk(w: &dyn Widget, label: &str) -> bool {
        w.accessibility().is_some_and(|node| found(&node, label))
            || w.children().into_iter().any(|c| walk(c, label))
    }
    walk(shell, label)
}

/// The title is exposed through the app and no pane draws it.
#[test]
fn app_title_lives_only_in_the_window_chrome() {
    // Case `app_title_is_exposed_only_through_the_app`.
    // The sanctioned channel: the host hands the OS pill this. Present and
    // correct.
    assert_eq!(SettingsShell::TITLE, "Settings");
    assert!(SettingsShell::ICON.is_some());

    // Case `no_pane_draws_the_app_name_but_each_page_names_its_destination`.
    // Across a representative set of categories, the app must never paint its
    // own name. The selected destination is the page's heading in the
    // window's header row, with no tagline under it.
    for &cat in &[
        Category::Network,
        Category::ExternalAccounts,
        Category::RegionFormats,
        Category::About,
    ] {
        let mut app = crate::support::window_on(cat, (W, H));
        let (names, heading) = app
            .with_widget::<NavShell, _>(|shell| {
                (rendered_names(shell), has_heading(shell, &cat.label()))
            })
            .expect("settings shows its shell");

        assert!(
            !names.iter().any(|n| n == "Settings"),
            "{cat:?}: the app must not draw its own \"Settings\" app-name header \
             (the OS pill carries it); rendered names were {names:?}"
        );
        assert!(
            heading,
            "{cat:?}: the selected settings destination is the page's heading; \
             rendered names were {names:?}"
        );
        assert!(
            !names.iter().any(|n| *n == cat.subtitle()),
            "{cat:?}: a heading has no tagline under it; rendered names were {names:?}"
        );
    }
}
