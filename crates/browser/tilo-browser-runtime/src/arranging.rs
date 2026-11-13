//! The arrangement in the page: the role is [`tilo_session_core::arrangement`]'s, the one a native
//! session runs, standing on the desk document this page keeps.
//!
//! A page has no compositor to say whether the person is mid-gesture, and the palette's index is
//! the frame's own, which the role cannot reach from here: an action that would wait for the
//! person's hands never waits, and text typed on the canvas places nothing (the page's palette
//! opens what it finds instead). Arranging by action — a place moved between rows, a place floated,
//! a strip resized, the person's own undo — is the one attributed change of the desk it is in a
//! session.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_desktop_surface::DesktopHandle;
use tilo_session_core::arrangement::{
    Arrangement, Ask, Busy, Door, Find, FindDone, RECORD_RESOURCE,
};
use tilo_session_core::desk::Desk;

/// The page's arrangement, and the door everything that arranges by action reaches it through.
pub(crate) struct Arranging {
    arrangement: Rc<RefCell<Arrangement>>,
    door: Door,
}

impl Arranging {
    /// The page's arrangement of `handle`'s desk, whose changes `desk` keeps, naming windows as the
    /// bundle's registry names them.
    pub(crate) fn new(person: &str, handle: DesktopHandle, desk: Rc<Desk>) -> Self {
        let arrangement = Rc::new(RefCell::new(Arrangement::new(
            person,
            handle,
            desk,
            Rc::new(tilo_desktop_search::names::window_name),
        )));
        // Nothing here answers a search or has an agent to ask, so both seams are honestly empty: a
        // place made by text is the page's palette's to open, never a second index of its own. The
        // empty find answers at once with nothing, as a search over no source would.
        let find: Find = Rc::new(|_: &str, done: FindDone| done(Vec::new()));
        let ask: Ask = Rc::new(|_: &str| false);
        let door = Door::new(Rc::clone(&arrangement), Rc::new(Busy::default), find, ask);
        Self { arrangement, door }
    }

    /// What the arrangement publishes now: its own resource, the desk's record, and a resource per
    /// row, strip and place on the desk.
    pub(crate) fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        self.door.resources()
    }

    /// Whether `resource` is one the arrangement publishes: the desk's record, or the arrangement
    /// and a row, strip or place of it.
    pub(crate) fn holds(&self, resource: &str) -> bool {
        fn held(resources: &[tilo_actions::ResourceSpec], id: &str) -> bool {
            resources
                .iter()
                .any(|resource| resource.id.as_str() == id || held(&resource.children, id))
        }
        resource == RECORD_RESOURCE || held(&self.door.resources(), resource)
    }

    /// Hand the arrangement what the page publishes now: what a place made from a resource reads
    /// its app and its views from.
    pub(crate) fn remember(&self, resources: &[tilo_actions::ResourceSpec]) {
        self.arrangement
            .borrow()
            .targets()
            .borrow_mut()
            .take(resources);
    }

    /// Carry out `address` with `arguments` as the person here, as the page's action calls do for
    /// a window's application.
    ///
    /// # Errors
    /// Why the action names nothing published, or why it could not be made.
    pub(crate) fn invoke(
        &self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, tilo_actions::DoorError> {
        let started = self.door.invoke(address, arguments)?;
        Ok(match started {
            ui::host_operation::ResourceOperationStart::Completed(output) => {
                serde_json::json!({"state": "completed", "output": output})
            }
            ui::host_operation::ResourceOperationStart::Accepted => {
                serde_json::json!({"state": "accepted"})
            }
        })
    }
}

#[cfg(test)]
mod tests {
    //! A page's desk, arranged by action: what an action does reaches the desk the page keeps, as
    //! one step of its history the person takes back.

    use super::*;

    use tilo_actions::ActionAddress;
    use tilo_desktop_model::commands::DesktopCommand;
    use tilo_desktop_model::{IdMinter, Tab, TileContent};
    use tilo_desktop_surface::{DesktopSurface, TileSource, seed_tree};
    use tilo_session_core::arrangement::{Item, RECORD_RESOURCE, RESOURCE};
    use ui::widget::TileHost;

    /// What a test desk's windows say. Nothing the page would start.
    struct Tiles;

    impl TileSource for Tiles {
        fn tile(&mut self, _: &tilo_desktop_model::Workspace, tab: &Tab) -> TileHost {
            TileHost::new(ui::widget::TileContent::Notice {
                title: tab.title.clone(),
                detail: String::new(),
            })
        }
    }

    struct Page {
        handle: DesktopHandle,
        desk: Rc<Desk>,
        arrangement: Arranging,
    }

    impl Page {
        /// A page of one workspace with a window of Notes in its row, and its desk document.
        fn new() -> Self {
            let mut minter = IdMinter::new(7);
            let notes = Tab {
                id: minter.mint(),
                title: "Notes".into(),
                tile: TileContent::LocalApp {
                    app_id: "so.tilo.notes".into(),
                    label: "Notes".into(),
                    note: String::new(),
                },
                app_title: None,
                view: String::new(),
                pinned: false,
            };
            let tree = seed_tree(&mut minter, "Studio", vec![notes]);
            let (_surface, handle) = DesktopSurface::new(tree, 7, Box::new(Tiles));
            let desk = Rc::new(Desk::new(ui::automerge::AutoCommit::new(), "ada", None));
            // The desk as the page opened it: no step of the person's, as a
            // session's first change is not one.
            desk.record_now(
                &handle.read(Clone::clone),
                ui::time::unix_now_i64(),
                &[],
                None,
            );
            let arrangement = Arranging::new("ada", handle.clone(), Rc::clone(&desk));
            Self {
                handle,
                desk,
                arrangement,
            }
        }

        /// The one window on the desk.
        fn window(&self) -> u64 {
            self.handle
                .read(|tree| {
                    tree.active_workspace()
                        .and_then(|workspace| workspace.strips.first())
                        .and_then(|row| row.window_groups.first())
                        .and_then(|group| group.tabs.first())
                        .map(|tab| tab.id)
                })
                .expect("the desk has a window")
        }

        fn invoke(
            &self,
            resource: &str,
            action: &str,
            arguments: &serde_json::Map<String, serde_json::Value>,
        ) -> serde_json::Value {
            self.arrangement
                .invoke(&ActionAddress::new(resource, action), arguments)
                .expect("the arrangement carries the action out")
        }

        /// Whether the window `place` floats above the desk.
        fn floating(&self, place: u64) -> bool {
            self.handle.read(|tree| {
                tree.active_workspace().is_some_and(|workspace| {
                    workspace
                        .strips
                        .iter()
                        .flat_map(|row| &row.window_groups)
                        .filter(|group| group.tabs.iter().any(|tab| tab.id == place))
                        .any(|group| workspace.arrangement.floats.contains_key(&group.id))
                })
            })
        }

        /// The row the window `place` stands in.
        fn row_of(&self, place: u64) -> Option<u64> {
            self.handle.read(|tree| {
                tree.active_workspace().and_then(|workspace| {
                    workspace
                        .strips
                        .iter()
                        .find(|row| {
                            row.window_groups
                                .iter()
                                .any(|group| group.tabs.iter().any(|tab| tab.id == place))
                        })
                        .map(|row| row.id.0)
                })
            })
        }

        /// A second row on the desk, called `name`.
        fn add_row(&self, name: &str) -> u64 {
            let mut tree = self.handle.read(Clone::clone);
            let made = tilo_desktop_model::commands::apply_to_active_workspace(
                &mut tree,
                &DesktopCommand::CreateStrip {
                    name: name.to_owned(),
                },
                &mut || Ok(self.handle.mint_id()),
            )
            .expect("the desk takes a second row")
            .expect("the row has an id");
            self.handle.replace_tree(tree);
            made.0
        }
    }

    /// The arrangement publishes the desk's places, rows and record, so the page's palette, a
    /// script and an agent can arrange it by action.
    #[test]
    fn the_page_publishes_the_desks_places_as_arranging_resources() {
        let page = Page::new();
        let resources = page.arrangement.resources();
        let arrangement = resources
            .iter()
            .find(|resource| resource.id.as_str() == RESOURCE)
            .expect("the arrangement is published");
        let place = Item::Place.resource(&page.window().to_string());
        assert!(
            arrangement
                .children
                .iter()
                .any(|child| child.id.as_str() == place),
            "the window in front is a place of it: {:?}",
            arrangement
                .children
                .iter()
                .map(|child| child.id.as_str())
                .collect::<Vec<_>>()
        );
        assert!(
            page.arrangement.holds(&place),
            "an action on that place reaches the arrangement"
        );
        assert!(
            page.arrangement.holds(RECORD_RESOURCE),
            "the desk's record is the person's to step back"
        );
        assert!(
            !page.arrangement.holds("so.tilo.notes:record"),
            "an app's resource is not the arrangement's"
        );
    }

    /// A float invoked through the door floats the page's window as one step of the desk's history,
    /// and the person's undo puts it back.
    #[test]
    fn a_float_through_the_door_is_one_step_the_person_takes_back() {
        let page = Page::new();
        let place = page.window();
        assert!(!page.floating(place), "it stands among the desk's windows");

        page.invoke(
            &Item::Place.resource(&place.to_string()),
            "float",
            &serde_json::Map::from_iter([
                ("corner".to_owned(), serde_json::json!("bottom-right")),
                ("dx".to_owned(), serde_json::json!(40)),
                ("dy".to_owned(), serde_json::json!(24)),
            ]),
        );
        assert!(page.floating(place), "the window floats");
        assert_eq!(
            page.desk.last_step("ada"),
            Some(("ada".to_owned(), "float".to_owned())),
            "floating is the person's own step of the desk's history"
        );

        page.invoke(RECORD_RESOURCE, "undo", &serde_json::Map::new());
        assert!(!page.floating(place), "the person's undo puts it back");
    }

    /// A move invoked through the door puts the page's window into the other row, as one step of
    /// the desk's history.
    #[test]
    fn a_move_through_the_door_changes_the_pages_desk() {
        let page = Page::new();
        let place = page.window();
        let before = page.row_of(place).expect("the window stands in a row");
        let second = page.add_row("Second");
        assert_ne!(before, second);

        page.invoke(
            &Item::Place.resource(&place.to_string()),
            "move",
            &serde_json::Map::from_iter([(
                "row".to_owned(),
                serde_json::json!(Item::Row.resource(&second.to_string())),
            )]),
        );
        assert_eq!(
            page.row_of(place),
            Some(second),
            "the window stands in the row it was moved to"
        );
        assert_eq!(
            page.desk.last_step("ada"),
            Some(("ada".to_owned(), "move".to_owned())),
            "the move is the person's step of the desk's history"
        );
    }

    /// What an action did is what the page's desk document holds: a reload of it reads the arranged
    /// desk, not the one before.
    #[test]
    fn the_action_is_what_the_pages_desk_document_holds() {
        let page = Page::new();
        let place = page.window();
        page.invoke(
            &Item::Place.resource(&place.to_string()),
            "pin",
            &serde_json::Map::from_iter([("pinned".to_owned(), serde_json::json!(true))]),
        );
        let reloaded = Desk::new(page.desk.document(), "ada", None);
        let pinned = reloaded
            .tree()
            .and_then(|tree| {
                tree.active_workspace().and_then(|workspace| {
                    workspace
                        .strips
                        .iter()
                        .flat_map(|row| &row.window_groups)
                        .flat_map(|group| &group.tabs)
                        .find(|tab| tab.id == place)
                        .map(|tab| tab.pinned)
                })
            })
            .unwrap_or(false);
        assert!(pinned, "the desk document holds the place as pinned");
    }
}
