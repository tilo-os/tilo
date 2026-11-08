//! Each person's screen as their presence says it, each window matched against the records the
//! host holds: a record held here is shared with that person, and its window here stands for
//! theirs (its title, its live picture); any other shows as its app alone.

use tilo_desktop_model::{DesktopTree, TabId};

use super::{PeerScreen, PeerWindow};
use crate::here::Person;

/// `people`'s screens, each window matched against the records `records` the host holds: its
/// window `(tab, app, digest)` in wire spelling, the digest naming the record as presence does
/// (`tilo_relay_protocol::live::LiveRecord::short_digest`). `app_name` names an app for people to
/// read; an app it does not know shows by its id.
#[must_use]
pub fn peer_screens(
    people: &[Person],
    tree: &DesktopTree,
    records: &[(TabId, String, [u8; 8])],
    app_name: &dyn Fn(&str) -> Option<String>,
) -> Vec<PeerScreen> {
    people
        .iter()
        .filter_map(|person| {
            let screen = person.screen.as_ref()?;
            let (width, height) = screen.size;
            let windows = screen
                .windows
                .iter()
                .map(|window| {
                    let tab = records
                        .iter()
                        .find(|(_, app, record)| *app == window.app && *record == window.record)
                        .map(|(tab, _, _)| *tab);
                    PeerWindow {
                        rect: window.rect,
                        app: window.app.clone(),
                        app_name: app_name(&window.app).unwrap_or_else(|| window.app.clone()),
                        title: tab.and_then(|tab| tab_title(tree, tab)),
                        picture: tab.and_then(|tab| group_of(tree, tab)),
                        focused: window.focused,
                    }
                })
                .collect();
            Some(PeerScreen {
                id: person.id.clone(),
                aspect: if height > 0.0 {
                    width / height
                } else {
                    16.0 / 10.0
                },
                windows,
            })
        })
        .collect()
}

fn tab_title(tree: &DesktopTree, tab: TabId) -> Option<String> {
    tree.tab(tab)
        .map(|t| t.title.clone())
        .filter(|title| !title.trim().is_empty())
}

/// The window group `tab` is in, whose live picture stands for it.
fn group_of(tree: &DesktopTree, tab: TabId) -> Option<u64> {
    tree.workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .find(|group| group.tabs.iter().any(|t| t.id == tab))
        .map(|group| group.id)
}

#[cfg(test)]
mod tests {
    use tilo_desktop_model::IdMinter;

    use super::*;
    use crate::here::{Screen, ScreenWindow};

    /// A window on someone's screen showing a record this host shows too stands for theirs, with
    /// its title and live picture; any other shows as its app and nothing more.
    #[test]
    fn only_what_this_host_holds_is_named_and_pictured() {
        let mut minter = IdMinter::new(3);
        let tab = tilo_desktop_model::Tab {
            id: minter.mint(),
            title: "Plans".into(),
            tile: tilo_desktop_model::TileContent::LocalApp {
                app_id: "so.tilo.notes".into(),
                label: "Notes".into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        };
        let tab_id = tab.id;
        let tree = tilo_desktop_surface::seed_tree(&mut minter, "Lounge", vec![tab]);
        let window = |record: [u8; 8], focused| ScreenWindow {
            app: "so.tilo.notes".into(),
            record,
            rect: vello::kurbo::Rect::new(0.0, 0.0, 0.5, 1.0),
            focused,
        };
        let person = Person {
            id: "ada".into(),
            user: None,
            label: "Ada".into(),
            devices: Vec::new(),
            rgb: (1, 2, 3),
            agent: false,
            tab: None,
            place: None,
            status: None,
            recent: None,
            screen: Some(Screen {
                size: (1600.0, 1000.0),
                windows: vec![window([1; 8], true), window([2; 8], false)],
            }),
            availability: ui::Availability::Available,
            call: None,
        };
        let records = [(tab_id, "so.tilo.notes".to_owned(), [1; 8])];
        let screens = peer_screens(std::slice::from_ref(&person), &tree, &records, &|app| {
            (app == "so.tilo.notes").then(|| "Notes".to_owned())
        });
        let [screen] = screens.as_slice() else {
            panic!("one screen: {screens:?}");
        };
        assert!((screen.aspect - 1.6).abs() < 1e-9);
        let [shared, private] = screen.windows.as_slice() else {
            panic!("two windows: {:?}", screen.windows);
        };
        assert_eq!(shared.title.as_deref(), Some("Plans"));
        assert_eq!(shared.picture, group_of(&tree, tab_id));
        assert!(shared.picture.is_some() && shared.focused);
        assert!(!private.shared(), "a record this host does not hold");
        assert_eq!((private.title.as_deref(), private.picture), (None, None));
        assert_eq!(private.app, "so.tilo.notes", "its app is all it says");
    }
}
