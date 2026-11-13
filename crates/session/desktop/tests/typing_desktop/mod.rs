//! A desk of six app windows, driven in process through the frame path both
//! hosts run: what one keystroke into one of them costs, and what each
//! window's app did about it.
//!
//! The six are the windows the browser's launch demo opens — Notes, Sheets,
//! Writer, Calendar, Calculator and Slides — one window each on one strip, as
//! they stand after a person opened them one by one and went back to typing
//! in their note.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    dead_code,
    reason = "a fixture: a desk that cannot be built is the assertion, and the \
              test and the measurement each use the part they need"
)]

use std::rc::Rc;

use tilo_desktop::resolver::{
    ApplicationCatalogTileResolver, ComponentContextSource, TileRequest, TileResolver,
};
use tilo_desktop_model::{IdMinter, Tab, TabId, TileContent, Workspace};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, TileSource, seed_tree};
use ui::application::{
    ApplicationCatalog, ApplicationDefinition, HostContext, HostContextInputs, LaunchTarget,
    RestoreInput,
};
use ui::application_view::InstanceViewStore;
use ui::headless::FrameHarness;
use ui::record::{RecordCheckout, RecordOwners, RecordStore};
use ui::state::State;
use ui::widget::TileHost;

/// One of the demo's windows: its key in the desk's records, its title, and
/// the app it shows.
pub type DemoApp = (
    &'static str,
    &'static str,
    fn() -> &'static ApplicationDefinition,
);

/// The apps the demo opens, in the order it opens them.
pub const DEMO: [DemoApp; 6] = [
    ("notes", "Notes", notes::definition),
    ("sheets", "Sheets", sheets::definition),
    ("word_processor", "Writer", word_processor::definition),
    ("calendar", "Calendar", calendar::definition),
    ("calculator", "Calculator", calculator::definition),
    ("slides", "Slides", slides::definition),
];

/// The app a person types into: the first window, the note.
pub const TYPED_INTO: &str = "notes";

/// What a component mounted in a window is given besides its own record.
struct Host {
    owners: RecordOwners,
    views: Rc<InstanceViewStore>,
}

impl ComponentContextSource for Host {
    fn prepare(
        &mut self,
        definition: &'static ApplicationDefinition,
        request: &TileRequest<'_>,
        target: &LaunchTarget,
    ) -> Result<(HostContext, RestoreInput), ui::launch::LaunchError> {
        let records = self.owners.open(definition.app_id(), "workspace").unwrap();
        let context = HostContext::prepare(
            definition,
            HostContextInputs {
                records,
                views: self.views.clone(),
                device_id: "device".into(),
                instance_id: request.tab.id,
                permissions: ui::permissions::SharedPermissions::new(
                    ui::permissions::PermissionContext::member(
                        "author",
                        ui::permissions::Role::Owner,
                    ),
                ),
                author: ui::permissions::UserId("author".into()),
                action_context: tilo_actions::ActionContext::default(),
            },
            target,
        )?;
        Ok((context, RestoreInput::None))
    }
}

/// What a window's tab shows: the app mounted, as a page mounts it.
struct Source {
    resolver: ApplicationCatalogTileResolver<Host>,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
    failures: Vec<ui::launch::LaunchError>,
}

impl TileSource for Source {
    fn tile(&mut self, workspace: &Workspace, tab: &Tab) -> TileHost {
        self.resolver
            .resolve(TileRequest {
                room: workspace.id(),
                tab,
            })
            .mount(&mut self.failures, std::sync::Arc::clone(&self.wake))
    }
}

/// A desk of one strip, one window per demo app.
pub struct DemoDesk {
    pub harness: FrameHarness,
    #[allow(
        dead_code,
        reason = "the desk keeps its handle alive while the harness runs it"
    )]
    pub handle: DesktopHandle,
    /// Each window's record, as the store the app reads it through holds it.
    records: Vec<(&'static str, RecordCheckout, RecordStore)>,
    /// The note's tab: the window a person types into.
    note_tab: TabId,
    /// A window woke since the host last ran the frame a wake asks for.
    woken: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl DemoDesk {
    /// The desk, its six apps mounted and drawn.
    pub fn open() -> Self {
        let cache = ui::portable_storage::PortableStorage::new("profile", "device").unwrap();
        let mut owners = RecordOwners::portable(cache.clone());
        let views = Rc::new(InstanceViewStore::portable(cache, "workspace").unwrap());
        let mut catalog = ApplicationCatalog::new();
        let mut records = vec![];
        let mut minter = IdMinter::new(7);
        let mut tabs = vec![];
        for (app_key, title, definition) in DEMO {
            let definition = definition();
            catalog.register(definition).unwrap();
            let id = minter.mint();
            let store = owners.open(definition.app_id(), "workspace").unwrap();
            let checkout = tilo_desktop::records::component_record(
                definition.app_id(),
                id,
                definition.record_kind.clone(),
            );
            store.create_record(&checkout).unwrap();
            records.push((app_key, checkout, store));
            tabs.push(Tab {
                id,
                title: title.to_owned(),
                tile: TileContent::LocalApp {
                    app_id: definition.app_id().to_owned(),
                    label: title.to_owned(),
                    note: String::new(),
                },
                app_title: None,
                view: String::new(),
                pinned: false,
            });
        }
        let note_tab = tabs.first().expect("the note is the first window").id;
        let tree = seed_tree(&mut minter, "Work", tabs);
        // What wakes a window (a worker's reply, another replica's change
        // arriving) asks the host for a frame, as the live desk's event loop
        // is asked for one.
        let woken = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let wake: std::sync::Arc<dyn Fn() + Send + Sync> = {
            let woken = std::sync::Arc::clone(&woken);
            std::sync::Arc::new(move || woken.store(true, std::sync::atomic::Ordering::Release))
        };
        let (surface, handle) = DesktopSurface::new(
            tree,
            7,
            Box::new(Source {
                resolver: ApplicationCatalogTileResolver {
                    catalog,
                    host: Host { owners, views },
                },
                wake,
                failures: vec![],
            }),
        );
        handle.set_viewport_width(1600.0);
        let mut harness = FrameHarness::new_with_controlled_inputs(
            State::new(),
            Box::new(surface),
            (1600, 1000),
            ui::Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        harness.set_caller_palette_enabled(false);
        for _ in 0..8 {
            harness.relayout();
        }
        Self {
            harness,
            handle,
            records,
            note_tab,
            woken,
        }
    }

    /// Every window's record as its committed heads stand, by app.
    pub fn heads(&self) -> Vec<(&'static str, usize)> {
        self.records
            .iter()
            .map(|(app, checkout, store)| {
                let heads = store
                    .with(&checkout.id, ui::automerge::AutoCommit::get_heads)
                    .map_or(0, |heads| heads.len());
                (*app, heads)
            })
            .collect()
    }

    /// What the window with the keyboard publishes as what it holds.
    pub fn focused_name(&self) -> Option<String> {
        self.harness
            .focused_access_node()
            .and_then(|node| node.name)
    }

    /// The note's record as its committed heads stand: what typing into the
    /// note changes.
    pub fn note_heads(&self) -> Vec<ui::automerge::ChangeHash> {
        let (_, checkout, store) = self
            .records
            .iter()
            .find(|(owner, _, _)| *owner == TYPED_INTO)
            .expect("the note is open");
        store
            .with(&checkout.id, ui::automerge::AutoCommit::get_heads)
            .expect("the note's record is open")
    }

    /// How many app windows the desk has open.
    pub fn apps_open(&self) -> usize {
        self.records.len()
    }

    /// Change `app`'s record from outside its window, as another participant's
    /// edit reaches it: their copy of the record, with their change, merges
    /// into this one, and the document moves under the app that shows it.
    pub fn change_elsewhere(&self, app: &str) {
        use ui::automerge::transaction::Transactable as _;
        let (_, checkout, store) = self
            .records
            .iter()
            .find(|(owner, _, _)| *owner == app)
            .expect("the desk has that app open");
        let mut theirs = store
            .with(&checkout.id, ui::automerge::AutoCommit::fork)
            .expect("the record is open");
        theirs.set_actor(ui::automerge::ActorId::random());
        theirs
            .put(ui::automerge::ROOT, "changed-elsewhere", "yes")
            .expect("their copy takes their change");
        assert!(
            store
                .merge_peer_document(checkout, theirs.save())
                .expect("their change merges"),
            "their change is new here"
        );
    }

    /// Put the keyboard in the note's editor, where a person typing has it:
    /// a note is made if the app shows none, and a press lands in the page's
    /// body, where a note is written.
    pub fn focus_the_note(&mut self) {
        use ui::accesskit::Role;
        let tile = self
            .handle
            .shown_tiles()
            .into_iter()
            .find(|(tab, _)| *tab == self.note_tab)
            .expect("the note's window is on the desk")
            .1;
        let in_tile = |node: &ui::accesskit::Node| {
            node.bounds().is_some_and(|bounds| {
                bounds.x0 >= tile.x0
                    && bounds.y0 >= tile.y0
                    && bounds.x1 <= tile.x1
                    && bounds.y1 <= tile.y1
            })
        };
        for _ in 0..4 {
            let body = self
                .harness
                .a11y_nodes()
                .into_iter()
                .find(|(_, node)| in_tile(node) && node.role() == Role::Document);
            if let Some((_, node)) = body {
                let bounds = node.bounds().expect("the page's bounds");
                // A press in the middle of the page, below what it shows.
                self.harness
                    .click_at(f64::midpoint(bounds.x0, bounds.x1), bounds.y0 + 200.0);
                for _ in 0..3 {
                    self.harness.relayout();
                }
                return;
            }
            // An empty notebook shows the way to its first note.
            let Some((new_note, _)) = self
                .harness
                .a11y_nodes()
                .into_iter()
                .find(|(_, node)| in_tile(node) && node.label() == Some("New note"))
            else {
                panic!(
                    "the note's window publishes neither a note to type into nor a way to make one"
                );
            };
            self.harness
                .perform_a11y_action(new_note, &ui::a11y::Action::Activate)
                .expect("the new note opens");
            for _ in 0..3 {
                self.harness.relayout();
            }
        }
        panic!("the note never opened an editor to type into");
    }

    /// Type one key and run the frames the host runs after it.
    pub fn type_key(&mut self, key: &str) {
        self.harness.key(key);
        self.harness.idle();
    }
}

/// What one keystroke cost and what the apps did about it.
pub struct KeyCost {
    /// The CPU time the key cost the thread that ran it, in microseconds.
    pub cpu_us: u64,
    /// How many app passes refreshed the actions their controls look up.
    pub passes: usize,
    /// How many times an app built the edits its views bound: the actions
    /// themselves, which a table or a calendar builds for every row it shows.
    pub declarations: usize,
    /// How many times a window's app synced its view
    /// ([`COMPONENT_SYNC`]): a window nothing reached rests.
    pub syncs: usize,
}

/// The phase an app names while it refreshes the actions its controls look
/// up, once a pass.
pub const ACTION_PASS: &str = "framework.action_bindings";
/// The phase an app names while it builds the edits its views bound.
pub const BOUND_DECLARATIONS: &str = "framework.bound_declarations";
/// The phase a window's app runs its view's sync under, once a sync: a
/// window whose inputs stood still skips it.
pub const COMPONENT_SYNC: &str = "ui.component.sync";

impl DemoDesk {
    /// Type `key` and answer what it cost, from the roles an app names as it
    /// runs ([`tilo_resource::profiling`]).
    pub fn measure_key(&mut self, key: &str) -> KeyCost {
        let started = ui::rt::thread_cpu_time();
        let ((), samples) = tilo_resource::profiling::observe(|| self.type_key(key));
        let cpu_us = started
            .zip(ui::rt::thread_cpu_time())
            .map_or(0, |(start, now)| {
                u64::try_from(now.saturating_sub(start).as_micros()).unwrap_or(u64::MAX)
            });
        let calls = |phase: &str| {
            samples
                .iter()
                .filter(|sample| sample.phase == phase)
                .map(|sample| sample.calls)
                .sum()
        };
        KeyCost {
            cpu_us,
            passes: calls(ACTION_PASS),
            declarations: calls(BOUND_DECLARATIONS),
            syncs: calls(COMPONENT_SYNC),
        }
    }

    /// How many window syncs `step` ran.
    pub fn syncs_during(&mut self, step: impl FnOnce(&mut Self)) -> usize {
        let ((), samples) = tilo_resource::profiling::observe(|| step(self));
        samples
            .iter()
            .filter(|sample| sample.phase == COMPONENT_SYNC)
            .map(|sample| sample.calls)
            .sum()
    }

    /// Idle until no window syncs on a frame of its own: the windows' arrival
    /// is motion, and a window in motion syncs every frame until it lands,
    /// however fast the frames come.
    pub fn rest(&mut self) {
        tilo_test_env::wait_until("every window of the desk at rest", || {
            self.syncs_during(|desk| {
                desk.idle();
                desk.idle();
            }) == 0
        });
    }

    /// Run the frames the host runs while nothing new reaches the desk.
    pub fn idle(&mut self) {
        if self.woken.swap(false, std::sync::atomic::Ordering::AcqRel) {
            self.harness.woken();
        }
        self.harness.idle();
    }
}
