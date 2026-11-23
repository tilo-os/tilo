//! A shell of three app windows, driven in process through the frame path both
//! hosts run: what one keystroke into one of them costs, and what each
//! window's app did about it.
//!
//! The three are windows the browser's launch demo opens — Sheets, Writer
//! and Slides — one window each on one strip, as they stand after a person
//! opened them one by one and went back to typing in their document.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    dead_code,
    reason = "a fixture: a shell that cannot be built is the assertion, and the \
              test and the measurement each use the part they need"
)]

use std::rc::Rc;

use tilo_desktop::resolver::{
    ApplicationCatalogTileResolver, ComponentContextSource, TileRequest, TileResolver,
};
use tilo_desktop_model::{IdMinter, Tab, TabId, TileContent, Workspace};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, TileSource};
use tilo_session_core::desktop::seed_tree;
use ui::application::{
    ApplicationCatalog, ApplicationDefinition, LaunchTarget, ProcessContext, ProcessContextInputs,
    RestoreInput,
};
use ui::application_view::InstanceViewStore;
use ui::headless::FrameHarness;
use ui::resource::{DocumentCheckout, ResourceOwners, ResourceStore};
use ui::state::State;
use ui::widget::TileHost;

/// One of the demo's windows: its key in the shell's records, its title, and
/// the app it shows.
pub type DemoApp = (
    &'static str,
    &'static str,
    fn() -> &'static ApplicationDefinition,
);

/// The apps the demo opens, in the order it opens them.
pub const DEMO: [DemoApp; 3] = [
    ("word_processor", "Writer", word_processor::definition),
    ("sheets", "Sheets", sheets::definition),
    ("slides", "Slides", slides::definition),
];

/// The app a person types into: the first window, the document.
pub const TYPED_INTO: &str = "word_processor";

/// What a component mounted in a window is given besides its own record.
struct Host {
    owners: ResourceOwners,
    views: Rc<InstanceViewStore>,
}

impl ComponentContextSource for Host {
    fn prepare(
        &mut self,
        definition: &'static ApplicationDefinition,
        request: &TileRequest<'_>,
        target: &LaunchTarget,
    ) -> Result<(ProcessContext, RestoreInput), ui::launch::LaunchError> {
        let records = self.owners.open(definition.app_id(), "workspace").unwrap();
        let context = ProcessContext::prepare(
            definition,
            ProcessContextInputs {
                records,
                views: self.views.clone(),
                device_id: "device".into(),
                instance_id: request.tab.id,
                permissions: ui::permissions::SharedPermissions::new(
                    ui::permissions::PermissionContext::member(
                        &ui::permissions::MemberId::person("author"),
                        ui::permissions::Role::Owner,
                    ),
                ),
                author: ui::permissions::MemberId::person("author"),
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
                workspace: workspace.id(),
                tab,
            })
            .mount(&mut self.failures, std::sync::Arc::clone(&self.wake))
    }
}

/// A shell of one strip, one window per demo app.
pub struct Demo {
    pub harness: FrameHarness,
    #[allow(
        dead_code,
        reason = "the shell keeps its handle alive while the harness runs it"
    )]
    pub handle: DesktopHandle,
    /// Each window's record, as the store the app reads it through holds it.
    records: Vec<(&'static str, DocumentCheckout, ResourceStore)>,
    /// The note's tab: the window a person types into.
    typed_tab: TabId,
    /// A window woke since the host last ran the frame a wake asks for.
    woken: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Demo {
    /// The shell, its three apps mounted and drawn.
    pub fn open() -> Self {
        let cache = ui::portable_storage::PortableStorage::new("profile", "device").unwrap();
        let mut owners = ResourceOwners::portable(cache.clone());
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
            let checkout = tilo_desktop::records::component_resource(
                definition.app_id(),
                id,
                definition.resource_type.clone(),
            );
            // The record as its app starts one: its genesis, storage version
            // included, so an app that moved what it keeps still reads it.
            let genesis = tilo_app::framework::hosted_kinds()
                .get(checkout.kind.as_str())
                .copied();
            store
                .reserve_resource(&checkout)
                .and_then(|mut reservation| {
                    reservation.initialize(|doc| {
                        genesis.map_or(Ok(()), |kind| {
                            kind.initialize(doc).map_err(|error| error.to_string())
                        })
                    })
                })
                .unwrap();
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
        let typed_tab = tabs.first().expect("the note is the first window").id;
        let tree = seed_tree(&mut minter, "Work", tabs);
        // What wakes a window (a worker's reply, another replica's change
        // arriving) asks the host for a frame, as the live shell's event loop
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
            ui::permissions::MemberId::person("author"),
        );
        harness.set_caller_palette_enabled(false);
        for _ in 0..8 {
            harness.relayout();
        }
        Self {
            harness,
            handle,
            records,
            typed_tab,
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

    /// The typed-into app's records as their committed heads stand: the
    /// document the keys went into. What typing changes.
    pub fn typed_heads(&self) -> Vec<ui::automerge::ChangeHash> {
        let (_, _, store) = self
            .records
            .iter()
            .find(|(owner, _, _)| *owner == TYPED_INTO)
            .expect("the document is open");
        let mut heads = Vec::new();
        for id in store.held_resources() {
            if let Some(held) = store.with(&id, ui::automerge::AutoCommit::get_heads) {
                heads.extend(held);
            }
        }
        heads
    }

    /// How many app windows the shell has open.
    pub fn apps_open(&self) -> usize {
        self.records.len()
    }

    /// Change `app`'s record from outside its window, as another member's
    /// edit reaches it: their copy of the record, with their change, merges
    /// into this one, and the document moves under the app that shows it.
    pub fn change_elsewhere(&self, app: &str) {
        use ui::automerge::transaction::Transactable as _;
        let (_, checkout, store) = self
            .records
            .iter()
            .find(|(owner, _, _)| *owner == app)
            .expect("the shell has that app open");
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

    /// Put the keyboard in the document's page, where a person typing has it: a press lands in
    /// the body the app writes into.
    pub fn focus_the_editor(&mut self) {
        use ui::accesskit::Role;
        let tile = self
            .handle
            .shown_tiles()
            .into_iter()
            .find(|(tab, _)| *tab == self.typed_tab)
            .expect("the document's window is on the shell")
            .1;
        let in_tile = |node: &ui::accesskit::Node| {
            node.bounds().is_some_and(|bounds| {
                bounds.x0 >= tile.x0
                    && bounds.y0 >= tile.y0
                    && bounds.x1 <= tile.x1
                    && bounds.y1 <= tile.y1
            })
        };
        // The page may still be laid out on the frame after the window opens: look again while
        // it is not there yet.
        for _ in 0..8 {
            let body = self
                .harness
                .a11y_nodes()
                .into_iter()
                .find(|(_, node)| in_tile(node) && node.role() == Role::Document);
            if let Some((_, node)) = body
                && let Some(bounds) = node.bounds()
            {
                // A press in the middle of the page, below what it shows.
                self.harness
                    .click_at(f64::midpoint(bounds.x0, bounds.x1), bounds.y0 + 200.0);
                for _ in 0..3 {
                    self.harness.relayout();
                }
                return;
            }
            for _ in 0..3 {
                self.harness.relayout();
            }
        }
        panic!("the document's window publishes no page to type into");
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
    /// How many frames rebuilt the shell's layout tree.
    pub rebuilds: usize,
}

/// The phase an app names while it refreshes the actions its controls look
/// up, once a pass.
pub const ACTION_PASS: &str = "framework.action_bindings";
/// The phase an app names while it builds the edits its views bound.
pub const BOUND_DECLARATIONS: &str = "framework.bound_declarations";
/// The phase a window's app runs its view's sync under, once a sync: a
/// window whose inputs stood still skips it.
pub const COMPONENT_SYNC: &str = "ui.component.sync";

impl Demo {
    /// Type `key` and answer what it cost, from the roles an app names as it
    /// runs ([`tilo_resource::profiling`]).
    pub fn measure_key(&mut self, key: &str) -> KeyCost {
        let started = ui::rt::thread_cpu_time();
        let rebuilt_before = self.harness.rebuilt_frames();
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
            rebuilds: usize::try_from(self.harness.rebuilt_frames().saturating_sub(rebuilt_before))
                .unwrap_or(usize::MAX),
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
        tilo_test_env::wait_until("every window of the shell at rest", || {
            self.syncs_during(|shell| {
                shell.idle();
                shell.idle();
            }) == 0
        });
    }

    /// Run the frames the host runs while nothing new reaches the shell.
    pub fn idle(&mut self) {
        if self.woken.swap(false, std::sync::atomic::Ordering::AcqRel) {
            self.harness.woken();
        }
        self.harness.idle();
    }
}
