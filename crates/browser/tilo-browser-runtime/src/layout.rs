//! The layout in the page: the role is [`tilo_session_core::layout`]'s, the one a native
//! session runs, standing on the layout document this page keeps.
//!
//! A page has no compositor to say whether the person is mid-gesture, and the palette's index is
//! the frame's own, which the role cannot reach from here: an action that would wait for the
//! person's hands never waits, and text typed on the canvas places nothing (the page's palette
//! opens what it finds instead). Changing the layout by action — a place moved between rows, a place floated,
//! a strip resized, the person's own undo — is the one attributed change of the layout it is in a
//! session.
//!
//! The first frame needs none of this code, so the program fetches it as the page first draws
//! ([`PAGE_LAYOUT`]); an action asked before it is in waits for it.

use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

use tilo_desktop_surface::DesktopHandle;
use tilo_session_core::layout::LayoutDocument;
use tilo_session_core::layout::{
    Ask, AskOrigin, Busy, Find, FindDone, Item, Layout, LayoutActions, RECORD_RESOURCE, RESOURCE,
};

/// What the page asks of its layout, as the trait object [`PAGE_LAYOUT`] makes.
pub(crate) trait LayoutApi {
    fn resources(&self) -> Vec<tilo_actions::ResourceSpec>;
    fn remember(&self, resources: &[tilo_actions::ResourceSpec]);
    fn take_asked_floats(&self);
    fn invoke(
        &self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, tilo_actions::DoorError>;
}

/// The page's layout, and the actions everything that changes it by action reaches it through.
struct PageActions {
    layout: Rc<RefCell<Layout>>,
    actions: LayoutActions,
}

impl PageActions {
    /// The page's layout of `handle`'s layout, whose changes `layout` keeps, naming windows as the
    /// bundle's registry names them.
    fn new(person: &str, handle: DesktopHandle, layout: Rc<LayoutDocument>) -> Self {
        let layout = Rc::new(RefCell::new(Layout::new(
            person,
            handle,
            layout,
            Rc::new(tilo_desktop_search::names::window_name),
            tilo_session_core::layout::source_words(),
        )));
        // Nothing here answers a search or has an agent to ask, so both seams are honestly empty: a
        // place made by text is the page's palette's to open, never a second index of its own. The
        // empty find answers at once with nothing, as a search over no source would.
        let find: Find = Rc::new(|_: &str, done: FindDone| done(Vec::new()));
        let ask: Ask = Rc::new(|_: &str| None);
        // No app's search is asked from the page either: a text naming an app's type places nothing.
        let ask_origin: AskOrigin = Rc::new(|_: &str| None);
        let actions = LayoutActions::new(
            Rc::clone(&layout),
            Rc::new(Busy::default),
            find,
            ask,
            ask_origin,
        );
        Self { layout, actions }
    }
}

impl LayoutApi for PageActions {
    fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        self.actions.resources()
    }

    fn remember(&self, resources: &[tilo_actions::ResourceSpec]) {
        self.layout.borrow().targets().borrow_mut().take(resources);
    }

    fn take_asked_floats(&self) {
        if let Err(why) = self.layout.borrow_mut().take_asked_floats() {
            tracing::warn!(why, "a window could not be floated or put back");
        }
    }

    fn invoke(
        &self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, tilo_actions::DoorError> {
        let started = self.actions.invoke(address, arguments)?;
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

/// The layout's code, as a program that fetches it on first use hands it over.
#[derive(Clone, Copy)]
pub struct MakePageLayout(fn(&str, DesktopHandle, Rc<LayoutDocument>) -> Box<dyn LayoutApi>);

/// The layout this build links.
#[must_use]
pub fn builtin_page_layout() -> MakePageLayout {
    MakePageLayout(|person, handle, layout| Box::new(PageActions::new(person, handle, layout)))
}

ui::lazy_code! {
    /// The layout's code: linked, or with `deferred-page-layout` fetched as the page first
    /// draws.
    pub static PAGE_LAYOUT: MakePageLayout = builtin_page_layout, feature = "deferred-page-layout";
}

/// The page's layout, whose code may still be on its way: the actions asked meanwhile wait,
/// in order, and run as it arrives.
pub(crate) struct PageLayout {
    code: &'static ui::lazy_code::LazyCode<MakePageLayout>,
    person: String,
    handle: DesktopHandle,
    layout: Rc<LayoutDocument>,
    made: OnceCell<Box<dyn LayoutApi>>,
    /// What the page published when an action was last asked before the code was in.
    remembered: RefCell<Vec<tilo_actions::ResourceSpec>>,
    waiting: RefCell<
        Vec<(
            tilo_actions::ActionAddress,
            serde_json::Map<String, serde_json::Value>,
        )>,
    >,
}

impl PageLayout {
    /// `person`'s layout of `handle`'s layout, whose changes `layout` keeps, its code from
    /// `code` ([`PAGE_LAYOUT`]).
    pub(crate) fn new(
        code: &'static ui::lazy_code::LazyCode<MakePageLayout>,
        person: &str,
        handle: DesktopHandle,
        layout: Rc<LayoutDocument>,
    ) -> Self {
        Self {
            code,
            person: person.to_owned(),
            handle,
            layout,
            made: OnceCell::new(),
            remembered: RefCell::default(),
            waiting: RefCell::default(),
        }
    }

    /// The layout, once its code is in; the actions that waited for it run first.
    fn page_actions(&self) -> Option<&dyn LayoutApi> {
        if let Some(made) = self.made.get() {
            return Some(made.as_ref());
        }
        let made = (self.code.get()?.0)(&self.person, self.handle.clone(), Rc::clone(&self.layout));
        made.remember(&self.remembered.take());
        for (address, arguments) in self.waiting.take() {
            if let Err(error) = made.invoke(&address, &arguments) {
                tracing::warn!(?error, action = %address.action, "a layout action failed");
            }
        }
        Some(self.made.get_or_init(|| made).as_ref())
    }

    /// Whether the layout's code is in.
    pub(crate) fn is_in(&self) -> bool {
        self.page_actions().is_some()
    }

    /// What the layout publishes now: its own resource, the layout's record, and a resource per
    /// row, strip and place on the layout. Nothing until its code is in.
    pub(crate) fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        self.page_actions()
            .map(LayoutApi::resources)
            .unwrap_or_default()
    }

    /// Whether `resource` is one the layout publishes: the layout's record, or the layout
    /// and a row, strip or place of it.
    pub(crate) fn holds(resource: &str) -> bool {
        resource == RESOURCE
            || resource == RECORD_RESOURCE
            || [Item::Place, Item::Strip, Item::Row, Item::Query]
                .into_iter()
                .any(|item| item.key(resource).is_some())
    }

    /// Hand the layout what the page publishes now: what a place made from a resource reads
    /// its app and its views from.
    pub(crate) fn remember(&self, resources: &[tilo_actions::ResourceSpec]) {
        match self.page_actions() {
            Some(actions) => actions.remember(resources),
            None => *self.remembered.borrow_mut() = resources.to_vec(),
        }
    }

    /// Make each window the person floated or put back on the layout their layout action; the
    /// layout keeps what was asked until the code is in.
    pub(crate) fn take_asked_floats(&self) {
        if let Some(actions) = self.page_actions() {
            actions.take_asked_floats();
        }
    }

    /// Carry out `address` with `arguments` as the person here, as the page's action calls do for
    /// a window's application. Before the code is in, the action waits for it and is accepted.
    ///
    /// # Errors
    /// Why the action names nothing published, or why it could not be made.
    pub(crate) fn invoke(
        &self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, tilo_actions::DoorError> {
        if let Some(actions) = self.page_actions() {
            return actions.invoke(address, arguments);
        }
        self.waiting
            .borrow_mut()
            .push((address.clone(), arguments.clone()));
        Ok(serde_json::json!({"state": "accepted"}))
    }
}

#[cfg(test)]
mod tests {
    //! A page's layout, arranged by action: what an action does reaches the layout the page keeps, as
    //! one step of its history the person takes back.

    use super::*;

    use tilo_actions::ActionAddress;
    use tilo_desktop_model::commands::DesktopCommand;
    use tilo_desktop_model::{IdMinter, Tab, TileContent};
    use tilo_desktop_surface::{DesktopSurface, TileSource, seed_tree};
    use ui::widget::TileHost;

    /// What a test layout's windows say. Nothing the page would start.
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
        layout: Rc<LayoutDocument>,
        actions: PageLayout,
    }

    impl Page {
        /// A page of one workspace with a window of Notes in its row, and its layout document,
        /// its layout's code coming from `code`.
        fn new(code: &'static ui::lazy_code::LazyCode<MakePageLayout>) -> Self {
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
            let layout = Rc::new(LayoutDocument::new(
                ui::automerge::AutoCommit::new(),
                "ada",
                None,
            ));
            // The layout as the page opened it: no step of the person's, as a
            // session's first change is not one.
            layout.record_now(
                &handle.read(Clone::clone),
                ui::time::unix_now_i64(),
                &[],
                None,
            );
            let actions = PageLayout::new(code, "ada", handle.clone(), Rc::clone(&layout));
            Self {
                handle,
                layout,
                actions,
            }
        }

        /// The one window on the layout.
        fn window(&self) -> u64 {
            self.handle
                .read(|tree| {
                    tree.active_workspace()
                        .and_then(|workspace| workspace.strips.first())
                        .and_then(|row| row.window_groups.first())
                        .and_then(|group| group.tabs.first())
                        .map(|tab| tab.id)
                })
                .expect("the layout has a window")
        }

        fn invoke(
            &self,
            resource: &str,
            action: &str,
            arguments: &serde_json::Map<String, serde_json::Value>,
        ) -> serde_json::Value {
            self.actions
                .invoke(&ActionAddress::new(resource, action), arguments)
                .expect("the page layout carries the action out")
        }

        /// Whether the window `place` floats above the layout.
        fn floating(&self, place: u64) -> bool {
            self.handle.read(|tree| {
                tree.active_workspace().is_some_and(|workspace| {
                    workspace
                        .strips
                        .iter()
                        .flat_map(|row| &row.window_groups)
                        .filter(|group| group.tabs.iter().any(|tab| tab.id == place))
                        .any(|group| workspace.layout.floats.contains_key(&group.id))
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

        /// A second row on the layout, called `name`.
        fn add_row(&self, name: &str) -> u64 {
            let mut tree = self.handle.read(Clone::clone);
            let made = tilo_desktop_model::commands::apply_to_active_workspace(
                &mut tree,
                &DesktopCommand::CreateStrip {
                    name: name.to_owned(),
                },
                &mut || Ok(self.handle.mint_id()),
            )
            .expect("the layout takes a second row")
            .expect("the row has an id");
            self.handle.replace_tree(tree);
            made.0
        }
    }

    tilo_test_env::scenario! {
        fn the_pages_layout_is_arranged_by_action;

        phase the_page_publishes_the_layouts_places_as_resources;
        // The layout publishes its places's places, rows and record, so the page's palette, a
        // script and an agent can arrange it by action.
        {
            let page = Page::new(&PAGE_LAYOUT);
            let resources = page.actions.resources();
            let layout = resources
                .iter()
                .find(|resource| resource.id.as_str() == RESOURCE)
                .expect("the layout is published");
            let place = Item::Place.resource(&page.window().to_string());
            assert!(
                layout
                    .children
                    .iter()
                    .any(|child| child.id.as_str() == place),
                "the window in front is a place of it: {:?}",
                layout
                    .children
                    .iter()
                    .map(|child| child.id.as_str())
                    .collect::<Vec<_>>()
            );
            assert!(
                PageLayout::holds(&place),
                "an action on that place reaches the layout"
            );
            assert!(
                PageLayout::holds(RECORD_RESOURCE),
                "the layout's record is the person's to step back"
            );
            assert!(
                !PageLayout::holds("so.tilo.notes:record"),
                "an app's resource is not the layout's"
            );
        }

        phase a_float_through_the_actions_is_one_step_the_person_takes_back;
        // A float invoked through the actions floats the page's window as one step of the layout's history,
        // and the person's undo puts it back.
        {
            let page = Page::new(&PAGE_LAYOUT);
            let place = page.window();
            assert!(!page.floating(place), "it stands among the layout's windows");

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
                page.layout.last_step("ada"),
                Some(("ada".to_owned(), "float".to_owned())),
                "floating is the person's own step of the layout's history"
            );

            page.invoke(RECORD_RESOURCE, "undo", &serde_json::Map::new());
            assert!(!page.floating(place), "the person's undo puts it back");
        }

        phase an_action_asked_before_the_code_is_in_runs_when_it_arrives;
        // The layout's code comes after the first frame: a float asked before it is accepted,
        // and the window floats as the code arrives, as one step of the person's.
        {
            static LATE: ui::lazy_code::LazyCode<MakePageLayout> =
                ui::lazy_code::LazyCode::fetched();
            let page = Page::new(&LATE);
            let place = page.window();
            assert!(
                page.actions.resources().is_empty(),
                "nothing is published before the code is in"
            );
            let accepted = page.invoke(
                &Item::Place.resource(&place.to_string()),
                "float",
                &serde_json::Map::from_iter([
                    ("corner".to_owned(), serde_json::json!("bottom-right")),
                    ("dx".to_owned(), serde_json::json!(40)),
                    ("dy".to_owned(), serde_json::json!(24)),
                ]),
            );
            assert_eq!(accepted, serde_json::json!({"state": "accepted"}));
            assert!(!page.floating(place), "the float waits for the code");

            LATE.install(builtin_page_layout());
            assert!(page.actions.is_in());
            assert!(page.floating(place), "the float ran as the code arrived");
            assert_eq!(
                page.layout.last_step("ada"),
                Some(("ada".to_owned(), "float".to_owned())),
                "the float is the person's own step"
            );
            assert!(
                !page.actions.resources().is_empty(),
                "the layout publishes its resources once its code is in"
            );
        }

        phase a_move_through_the_actions_changes_the_pages_layout;
        // A move invoked through the actions puts the page's window into the other row, as one step of
        // the layout's history.
        {
            let page = Page::new(&PAGE_LAYOUT);
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
                page.layout.last_step("ada"),
                Some(("ada".to_owned(), "move".to_owned())),
                "the move is the person's step of the layout's history"
            );
        }

        phase the_action_is_what_the_pages_layout_document_holds;
        // What an action did is what the page's layout document holds: a reload of it reads the
        // arranged layout, not the one before.
        {
            let page = Page::new(&PAGE_LAYOUT);
            let place = page.window();
            page.invoke(
                &Item::Place.resource(&place.to_string()),
                "pin",
                &serde_json::Map::from_iter([("pinned".to_owned(), serde_json::json!(true))]),
            );
            let reloaded = LayoutDocument::new(page.layout.document(), "ada", None);
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
            assert!(pinned, "the layout document holds the place as pinned");
        }
    }
}
