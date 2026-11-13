//! The OS draws the window chrome; the app must NOT draw its own title / app-
//! name header (that would duplicate the compositor's window-group pill). This.

use settings::SettingsShell;
use settings::categories::Category;
use ui::framework::ResourceApp;
use ui::widget::{NavShell, Widget};

const W: f32 = 1200.0;
const H: f32 = 760.0;

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

#[test]
fn app_title_lives_only_in_the_window_chrome() {
    assert_eq!(SettingsShell::TITLE, "Settings");
    assert!(SettingsShell::ICON.is_some());

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
