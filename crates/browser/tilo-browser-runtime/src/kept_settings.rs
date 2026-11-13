//! The person's settings, kept by the page as their session keeps them on a
//! device.
//!
//! Settings' sections (appearance, accessibility, region, ...) are the
//! person's records, kept by their session under
//! [`settings_records::PROVIDER`]: a window shows them and binds its controls
//! to their field edits and verbs, but each change is the keeper's action,
//! invoked where it is kept ([`ui::framework::Records::kept_by`]). On a page
//! the page is that session: it keeps the settings document
//! (`crate::settings`), so it answers those actions itself, in the person's
//! name, and the change lands in the page's settings at once.

use tilo_actions::remote::settings_records;
use ui::headless::FrameHarness;
use ui::storage::SettingsDocument;

/// Answer the settings sections the windows in `frame` change, kept in
/// `document`, as `person`'s: what they change is theirs, undone and redone
/// as their steps.
pub(crate) fn serve(frame: &mut FrameHarness, person: &str, document: SettingsDocument) {
    let records = tilo_app::framework::SettingsRecords::of_person_named_in(
        settings_sections::APP_ID,
        settings_sections::APP_TITLE,
        person,
        &[],
        &[settings_sections::declare],
        document,
    );
    frame.serve_provider(settings_records::PROVIDER, records.served_for(person));
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use std::collections::{BTreeMap, HashMap};

    use settings::categories::Category;
    use ui::accesskit::{Node, NodeId, Role};

    type Window = tilo_app::framework::testing::TestApp<settings::SettingsShell>;

    /// Settings as the page hosts it: the page, not the test, keeps the
    /// person's settings its controls change.
    fn on_the_page() -> (Window, ui::storage::SettingsDocument) {
        let mut app = Window::new().expect("settings opens headlessly");
        let mut kept = None;
        app.kept_by_host(|frame, document, person| {
            kept = Some(document.clone());
            super::serve(frame, person, document.clone());
        });
        (app, kept.expect("the host is told the settings document"))
    }

    /// Every section as the page keeps it now.
    fn stored(document: &ui::storage::SettingsDocument) -> BTreeMap<&'static str, String> {
        let records = tilo_app::framework::SettingsRecords::in_document(
            settings_sections::APP_ID,
            &[settings_sections::declare],
            document.clone(),
        );
        settings_sections::kept()
            .into_iter()
            .map(|(kind, _)| {
                let value = records
                    .stored(kind)
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                (kind, value)
            })
            .collect()
    }

    fn open(app: &mut Window, category: Category) {
        app.effect(&settings::effects::open_pane(category))
            .expect("the pane opens");
        app.harness().resize((1200, 3200)).expect("lays out");
        app.settle();
    }

    /// A control the person changes a setting with: its role, its name and
    /// which of the controls so named and of that role it is.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Control {
        role: Role,
        name: String,
        nth: usize,
    }

    /// The value controls of the pane on screen: switches, choices, the
    /// buttons of a segmented choice, choosers and the theme tiles. Not the
    /// sidebar, nor a button that opens something else.
    fn controls(nodes: &[(NodeId, Node)]) -> Vec<Control> {
        let mut parent: HashMap<NodeId, &Node> = HashMap::new();
        for (_, node) in nodes {
            for child in node.children() {
                parent.insert(*child, node);
            }
        }
        let mut seen: HashMap<(Role, String), usize> = HashMap::new();
        let mut out = Vec::new();
        for (id, node) in nodes {
            let Some(name) = node.label().map(str::to_owned) else {
                continue;
            };
            let in_choice = parent
                .get(id)
                .is_some_and(|group| group.role() == Role::Group && group.value().is_some());
            let theme = node.role() == Role::Group && name == "Theme";
            let wanted = matches!(
                node.role(),
                Role::Switch | Role::RadioButton | Role::ComboBox
            ) || (node.role() == Role::Button && in_choice)
                || theme;
            if !wanted {
                continue;
            }
            let count = seen.entry((node.role(), name.clone())).or_default();
            let nth = *count;
            *count = count.saturating_add(1);
            out.push(Control {
                role: node.role(),
                name,
                nth,
            });
        }
        out
    }

    fn find(app: &mut Window, control: &Control) -> Option<(NodeId, Node)> {
        app.harness()
            .a11y_nodes()
            .into_iter()
            .filter(|(_, node)| {
                node.role() == control.role && node.label() == Some(control.name.as_str())
            })
            .nth(control.nth)
    }

    fn centre(node: &Node) -> (f64, f64) {
        let bounds = node.bounds().expect("a control the person sees has bounds");
        (
            f64::midpoint(bounds.x0, bounds.x1),
            f64::midpoint(bounds.y0, bounds.y1),
        )
    }

    fn click(app: &mut Window, (x, y): (f64, f64)) {
        app.harness().click_at(x, y);
        app.settle();
    }

    /// Whether the control already shows the value a click would choose.
    fn chosen(node: &Node) -> bool {
        node.is_selected() == Some(true) || node.toggled() == Some(ui::accesskit::Toggled::True)
    }

    /// The options a chooser shows once it is open.
    fn options(app: &mut Window) -> Vec<(NodeId, Node)> {
        app.harness()
            .a11y_nodes()
            .into_iter()
            .filter(|(_, node)| {
                matches!(
                    node.role(),
                    Role::ListBoxOption | Role::MenuItem | Role::MenuItemRadio
                ) && node.bounds().is_some()
            })
            .collect()
    }

    /// Click `control` as a person does and say what went wrong, if anything:
    /// a notice over the window, or a setting that did not change.
    fn change(
        app: &mut Window,
        document: &ui::storage::SettingsDocument,
        category: Category,
        control: &Control,
    ) -> Option<String> {
        let (_, node) = find(app, control)?;
        let before = stored(document);
        let mut expect_change = !chosen(&node);
        match control.role {
            Role::ComboBox => {
                click(app, centre(&node));
                let current = node.value().map(str::to_owned);
                let pick = options(app).into_iter().find(|(_, option)| {
                    option.label().map(str::to_owned) != current && !chosen(option)
                });
                let Some((_, option)) = pick else {
                    app.harness().key("Escape");
                    app.settle();
                    return None;
                };
                click(app, centre(&option));
                expect_change = true;
            }
            Role::Group => {
                // The theme tiles: pictures, each a theme, in rows. The
                // first point that lands on another theme's tile picks it.
                let bounds = node.bounds().expect("the tiles are on screen");
                let first = node.value().map(str::to_owned);
                let along = |from: f64, to: f64| {
                    std::iter::successors(Some(from + 12.0), |at| Some(at + 24.0))
                        .take_while(move |at| *at < to)
                };
                let points = along(bounds.y0, bounds.y1)
                    .flat_map(|y| along(bounds.x0, bounds.x1).map(move |x| (x, y)))
                    .collect::<Vec<_>>();
                for point in points {
                    click(app, point);
                    let now = find(app, control).and_then(|(_, n)| n.value().map(str::to_owned));
                    if now != first || app.harness().refusal_said().is_some() {
                        break;
                    }
                }
                expect_change = true;
            }
            _ => click(app, centre(&node)),
        }
        if let Some(said) = app.harness().refusal_said().map(str::to_owned) {
            app.harness().key("Escape");
            app.settle();
            return Some(format!("{category:?} {control:?}: said {said:?}"));
        }
        (expect_change && stored(document) == before)
            .then(|| format!("{category:?} {control:?}: the setting did not change"))
    }

    /// Every control of every Settings pane the web carries, clicked as a
    /// person clicks it, changes the person's settings where the page keeps
    /// them: the theme, the accent, the text size and the rest. None says
    /// anything over the window.
    #[test]
    fn every_settings_control_on_the_page_takes_effect() {
        let (mut app, document) = on_the_page();
        let mut failed = Vec::new();
        let mut changed = 0;
        for category in Category::WEB {
            open(&mut app, *category);
            let nodes = app.harness().a11y_nodes();
            for control in controls(&nodes) {
                let before = stored(&document);
                match change(&mut app, &document, *category, &control) {
                    Some(failure) => failed.push(failure),
                    None if stored(&document) != before => changed += 1,
                    None => {}
                }
                open(&mut app, *category);
            }
        }
        assert!(failed.is_empty(), "{failed:#?}");
        assert!(changed >= 20, "only {changed} controls changed a setting");
    }
}
