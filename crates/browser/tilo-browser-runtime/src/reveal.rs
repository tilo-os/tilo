//! What a window is asked to run when the page asks it to show something.
//!
//! A search hit, a `settings:` link and a notice's choice each ask an app's
//! window to `open` a resource of it: a note, a Settings page. The app's own
//! `open` shows any of them, told the resource as its anchor, as an item's
//! own `open` is that same operation bound to the item. A resource that
//! publishes no `open` of its own (a Settings page's row) is shown the same
//! way, so the page asks the app's `open` and never the resource's.

use tilo_actions::{ActionAddress, ResourceSpec};

/// The action of an app's resources that shows one of them.
const OPEN: &str = tilo_actions::remote::APPLICATION_OPEN_ACTION;
/// The argument naming what `open` shows.
const ANCHOR: &str = "anchor";

/// Whether a window opened for `action` on a resource has anything to run
/// once it is up: an `open` of the app's own resource (`narrower` false) is
/// the window itself, unless it names an anchor to show there (a search hit
/// on a Settings page opens Settings at that page).
pub(crate) fn asks_window(
    action: &str,
    narrower: bool,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    action != OPEN
        || narrower
        || arguments
            .get(ANCHOR)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|anchor| !anchor.is_empty())
}

/// Where `action` on `resource`, asked of a window whose top resources are
/// `roots`, runs, and with what: an `open` is the app's own `open` anchored
/// on `resource` (an anchor already given stands); any other action runs
/// where it was asked. `None` while the window publishes no `open` of the
/// app's yet (it is still coming up).
pub(crate) fn address(
    resource: &str,
    action: &str,
    arguments: &serde_json::Map<String, serde_json::Value>,
    roots: &[ResourceSpec],
) -> Option<(ActionAddress, serde_json::Map<String, serde_json::Value>)> {
    if action != OPEN {
        return Some((ActionAddress::new(resource, action), arguments.clone()));
    }
    let app = roots
        .iter()
        .find(|root| root.actions.iter().any(|spec| spec.id == OPEN))?;
    let mut arguments = arguments.clone();
    arguments
        .entry(ANCHOR)
        .or_insert_with(|| serde_json::Value::from(resource));
    Some((ActionAddress::new(app.id.as_str(), OPEN), arguments))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> tilo_actions::ActionSpec {
        tilo_actions::ActionSpec::typed_operation(
            OPEN,
            "Open",
            "Item to open",
            tilo_actions::OperationKind::View,
            "open.type",
            Vec::new(),
            |_| tilo_actions::EncodedAction {
                type_id: "open.type".into(),
                payload: Vec::new(),
            },
        )
    }

    /// The window Settings shows: its own resource, which opens any page at
    /// its anchor, and a page's row, which publishes no `open` of its own.
    fn settings() -> Vec<ResourceSpec> {
        let mut root = ResourceSpec::new("settings.root", "settings.root", "Settings");
        root.actions.push(open());
        root.children.push(ResourceSpec::new(
            "settings.section:external_accounts",
            "settings.section",
            "Accounts",
        ));
        vec![root]
    }

    /// A Settings page a search hit or a `settings:` link names is shown by
    /// Settings' own `open`, anchored on the page's row: the row has no
    /// `open` to ask.
    #[test]
    fn a_page_opens_through_the_apps_open_at_its_anchor() {
        let (at, arguments) = address(
            "settings.section:external_accounts",
            OPEN,
            &serde_json::Map::new(),
            &settings(),
        )
        .expect("Settings' window shows its pages");
        assert_eq!(at, ActionAddress::new("settings.root", OPEN));
        assert_eq!(
            arguments.get(ANCHOR),
            Some(&serde_json::Value::from(
                "settings.section:external_accounts"
            ))
        );
    }

    /// Search's row for a Settings page opens Settings' own resource at the
    /// page: the window has the page to show, where an open of the app's own
    /// resource naming nothing is the window alone.
    #[test]
    fn an_open_of_the_app_at_an_anchor_still_shows_the_anchor() {
        let mut anchored = serde_json::Map::new();
        anchored.insert(ANCHOR.into(), "settings.section:external_accounts".into());
        assert!(asks_window(OPEN, false, &anchored));
        assert!(!asks_window(OPEN, false, &serde_json::Map::new()));
        assert!(asks_window(OPEN, true, &serde_json::Map::new()));
        assert!(asks_window("join", false, &serde_json::Map::new()));

        let (at, arguments) = address("settings.root", OPEN, &anchored, &settings()).unwrap();
        assert_eq!(at, ActionAddress::new("settings.root", OPEN));
        assert_eq!(arguments, anchored);
    }

    /// An anchor the caller named stands (a notice's place inside the
    /// resource); an action other than `open` runs where it was asked; a
    /// window that publishes no `open` yet is waited for.
    #[test]
    fn what_was_asked_stands_and_a_window_coming_up_is_waited_for() {
        let mut named = serde_json::Map::new();
        named.insert(ANCHOR.into(), "notes.note:n1".into());
        let (_, arguments) = address("notes.notebook:a", OPEN, &named, &settings()).unwrap();
        assert_eq!(arguments.get(ANCHOR), Some(&"notes.note:n1".into()));

        let (at, _) = address("chat.room:r", "join", &serde_json::Map::new(), &[]).unwrap();
        assert_eq!(at, ActionAddress::new("chat.room:r", "join"));

        assert!(address("settings.section:x", OPEN, &serde_json::Map::new(), &[]).is_none());
    }
}
