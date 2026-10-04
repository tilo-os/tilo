//! Framework apps the shell mounts in its own process.
//!
//! A tab of an app the shell hosts itself (the terminal) is the app's own
//! component: its definition, on a record of its own in the workspace's
//! room, with the dispatcher, history and published actions the app has
//! anywhere else. So the palette and agents reach the tab through the same
//! door as the app's own window, and what the tab keeps (the terminal's
//! last target, a Notes tab's notebook) is in its record. What only the shell can give the app (a
//! connection to the session's process supervisor) it lends
//! ([`HostContext::lend`]).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use ui::application::{
    AppComponent, ApplicationDefinition, HostContext, HostContextInputs, LaunchTarget, RestoreInput,
};
use ui::application_view::InstanceViewStore;
use ui::launch::LaunchError;
use ui::permissions::{Actor, PermissionContext, Role, UserId};
use ui::record::{RecordCheckout, RecordId, RecordOpenError, RecordOwners};

/// The apps the shell mounts in-process, with the store their records and
/// views live in, as one person on one device.
pub struct InProcessApps {
    store: Rc<dyn durable_store::DurableStore>,
    owners: RecordOwners,
    views: HashMap<String, Rc<InstanceViewStore>>,
    device_id: String,
    author: UserId,
}

impl InProcessApps {
    /// Apps keeping their records in `store`, mounted for `author` on
    /// `device_id`.
    pub fn new(store: Rc<dyn durable_store::DurableStore>, device_id: &str, author: &str) -> Self {
        Self {
            owners: RecordOwners::with_store(Rc::clone(&store)),
            store,
            views: HashMap::new(),
            device_id: device_id.to_owned(),
            author: UserId(author.to_owned()),
        }
    }

    /// The apps of a session whose state lives in `profile_root`: their
    /// records and views there, as this device.
    ///
    /// # Errors
    /// A relative root or corrupt device identity.
    pub fn at(profile_root: &std::path::Path, author: &str) -> Result<Self, LaunchError> {
        std::fs::create_dir_all(profile_root)
            .map_err(|error| LaunchError::Operation(error.to_string()))?;
        let profile = ui::application::NativeProfile::open(profile_root, None)?;
        Ok(Self::new(
            Rc::new(app_state::FsStore::new(profile_root.to_path_buf())),
            profile.device_id(),
            author,
        ))
    }

    /// The session's apps: kept under the shell's own state, beside the
    /// records of its scratch tabs.
    ///
    /// # Errors
    /// As [`Self::at`].
    pub fn native(author: &str) -> Result<Self, LaunchError> {
        Self::at(
            &cli::data_home()
                .join(crate::shell::SHELL_RECORDS_PACKAGE)
                .join("apps"),
            author,
        )
    }

    /// Mount `definition` for tab `tab` of `room` on `record` (the tab's
    /// own, or the room's default the app shares between its windows),
    /// created by the app's genesis the first time, reopened every time
    /// after. `lend` hands the context what the shell lends the app.
    ///
    /// # Errors
    /// The record cannot be opened or created, or the app refuses the
    /// context.
    pub fn mount(
        &mut self,
        definition: &'static ApplicationDefinition,
        room: &str,
        tab: u64,
        record: RecordId,
        lend: impl FnOnce(&mut HostContext),
    ) -> Result<AppComponent, LaunchError> {
        let records = self
            .owners
            .open(definition.app_id, room)
            .map_err(LaunchError::Record)?;
        let views = if let Some(views) = self.views.get(room) {
            Rc::clone(views)
        } else {
            let views = Rc::new(
                InstanceViewStore::with_store(Rc::clone(&self.store), &self.device_id, room)
                    .map_err(LaunchError::Restore)?,
            );
            self.views.insert(room.to_owned(), Rc::clone(&views));
            views
        };
        let inputs = || HostContextInputs {
            records: records.clone(),
            views: Rc::clone(&views),
            device_id: self.device_id.clone(),
            instance_id: tab,
            permissions: Rc::new(RefCell::new(PermissionContext::from_capabilities(
                Actor::person(Some(Role::Owner), &self.author.0),
                vec![],
            ))),
            author: self.author.clone(),
            action_context: tilo_actions::ActionContext::default(),
        };
        let checkout = RecordCheckout::new(record, definition.record_kind.clone());
        let (mut context, target) = match records.try_open_record(&checkout) {
            Ok(_) => {
                let target = LaunchTarget::Record {
                    checkout,
                    reveal: None,
                };
                (HostContext::prepare(definition, inputs(), &target)?, target)
            }
            Err(RecordOpenError::MissingRecord) => (
                HostContext::prepare_new_record(definition, inputs(), checkout.id)?,
                LaunchTarget::New,
            ),
            Err(error) => return Err(LaunchError::Record(error)),
        };
        if let Ok(person) = ui::record::person::PersonRecords::open(
            &mut self.owners,
            definition.app_id,
            self.author.clone(),
        ) {
            context.provide_person_records(person);
        }
        lend(&mut context);
        definition.instantiate(context, target, RestoreInput::None)
    }
}

/// The in-process apps a host shares between the tiles it resolves.
pub type SharedInProcessApps = Rc<RefCell<InProcessApps>>;

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_resource::Resource as _;

    const ROOM: &str = "6a3f0c2e-0f1b-4c5d-9e8f-123456789abc";
    const TAB: u64 = 41;

    fn mount_terminal(apps: &mut InProcessApps) -> ui::headless::FrameHarness {
        let component = apps
            .mount(
                terminal::definition(),
                ROOM,
                TAB,
                RecordId::for_tab(TAB),
                |context| {
                    context.lend(terminal::HostedProcessServices::unavailable());
                },
            )
            .unwrap();
        let mut harness = ui::headless::FrameHarness::from_component(
            component,
            (900, 600),
            ui::application::ComponentRunnerInputs::from_native_process(),
        )
        .unwrap();
        harness.relayout();
        harness
    }

    /// A terminal tab the shell hosts is the terminal's own component: it
    /// offers the terminal's actions through the shell's door, and what it
    /// keeps is in the tab's record, there again when the tab is mounted
    /// again.
    #[test]
    fn a_hosted_terminal_tab_publishes_its_actions_and_keeps_its_record() {
        let profile = tempfile::tempdir().unwrap();
        let mut apps = InProcessApps::at(profile.path(), "test-person").unwrap();
        let mut harness = mount_terminal(&mut apps);
        let session = harness
            .resources()
            .into_iter()
            .find(|resource| resource.kind == terminal::Terminal::KIND)
            .expect("the tab publishes the terminal's session");
        for action in ["run", "send", "interrupt", "open-in"] {
            assert!(
                session.actions.iter().any(|spec| spec.id == action),
                "{action} reaches the shell's palette and agents"
            );
        }
        let target = terminal::target::SessionTarget::Legacy { vm: "build".into() };
        let retarget = serde_json::json!({
            tilo_resource::APPLY_PARAM: serde_json::to_value(
                terminal::resource::TerminalAction::SetTarget { target: target.encode() }
            )
            .unwrap()
        });
        harness
            .invoke_bound_action(
                &ui::bound_dispatch::BoundActionAddress {
                    instance_id: TAB,
                    address: tilo_actions::ActionAddress::new(
                        terminal::Terminal::KIND,
                        tilo_resource::APPLY,
                    ),
                },
                retarget.as_object().unwrap(),
            )
            .unwrap();
        harness.relayout();
        // The tab closes as the shell closes it: its record stored first.
        harness.flush_component_records().unwrap();
        harness.close_components();
        drop(harness);

        let _again = mount_terminal(&mut apps);
        let records = apps
            .owners
            .open(terminal::definition().app_id, ROOM)
            .unwrap();
        let kept = records
            .with(&RecordId::for_tab(TAB), |doc| {
                terminal::Terminal::read(doc).map(|state| state.target)
            })
            .expect("the tab's record is open")
            .unwrap();
        assert_eq!(kept, Some(target), "the tab's record keeps its last target");
    }

    /// Files mounted in the shell starts in the folder the shell names, and
    /// a folder pinned to its Favourites is in the room's record of places,
    /// there for the next Files tab.
    #[test]
    fn a_hosted_files_tab_starts_where_the_shell_says_and_keeps_its_favourites() {
        let profile = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("harbour-plan.txt"), "plan").unwrap();
        let folder = folder.path().canonicalize().unwrap();
        let mut apps = InProcessApps::at(profile.path(), "test-person").unwrap();
        let mount = |apps: &mut InProcessApps, tab: u64| {
            let startup = file_explorer::StartupInputs {
                initial_dir: folder.clone(),
                workspace_roots: Vec::new(),
                record_roots: Vec::new(),
                remote_accounts: Vec::new(),
                trusted_humans: file_explorer::identity_directory::TrustedHumanDirectory::default(),
            };
            let component = apps
                .mount(
                    file_explorer::definition(),
                    ROOM,
                    tab,
                    RecordId::room_default(),
                    |context| {
                        context.lend(startup);
                    },
                )
                .unwrap();
            let mut harness = ui::headless::FrameHarness::from_component(
                component,
                (1000, 700),
                ui::application::ComponentRunnerInputs::from_native_process(),
            )
            .unwrap();
            harness.relayout();
            harness
        };
        let mut harness = mount(&mut apps, 51);
        assert!(
            harness
                .inspection_json()
                .unwrap()
                .contains("harbour-plan.txt"),
            "Files lists the folder the shell named"
        );
        let places = |apps: &mut InProcessApps| {
            let records = apps
                .owners
                .open(file_explorer::definition().app_id, ROOM)
                .unwrap();
            records
                .with(&RecordId::room_default(), |doc| {
                    file_explorer::FileExplorer::read(doc).map(|state| state.bookmarks)
                })
                .expect("the room's record of places is open")
                .unwrap()
        };
        let before = places(&mut apps);
        let place = folder.to_string_lossy().into_owned();
        harness
            .invoke_bound_action(
                &ui::bound_dispatch::BoundActionAddress {
                    instance_id: 51,
                    address: tilo_actions::ActionAddress::new("files.folder", "add-bookmark"),
                },
                serde_json::json!({ "path": place }).as_object().unwrap(),
            )
            .unwrap();
        harness.relayout();
        harness.flush_component_records().unwrap();
        harness.close_components();
        drop(harness);

        let _next = mount(&mut apps, 52);
        let after = places(&mut apps);
        assert_eq!(
            after.len(),
            before.len() + 1,
            "the pinned folder is in the room's places: {after:?}"
        );
    }

    /// The shell's tab `tab` of `kind`, mounted as the shell's tile resolver
    /// mounts it.
    fn mount_tab(
        inputs: &crate::shell::hosted_inputs::HostedTileInputs,
        tab: u64,
        kind: crate::shell::state::AppKind,
    ) -> ui::headless::FrameHarness {
        let tab = crate::shell::state::Tab {
            id: tab,
            title: "Tab".into(),
            tile: crate::shell::state::TileContent::AppPlaceholder {
                kind,
                label: "Tab".into(),
            },
        };
        let room = crate::shell::state::RoomId::new();
        let request = tilo_desktop::resolver::TileRequest { room, tab: &tab };
        let Some(tilo_desktop::resolver::TileResolution::Component(component)) =
            crate::shell::mount_in_process(&request, inputs)
        else {
            panic!("the tab mounts its app's own component");
        };
        let mut harness = ui::headless::FrameHarness::from_component(
            component,
            (1000, 700),
            ui::application::ComponentRunnerInputs::from_native_process(),
        )
        .unwrap();
        harness.relayout();
        harness
    }

    fn notes_kind() -> crate::shell::state::AppKind {
        crate::shell::state::AppKind::Other("Notes".into())
    }

    /// Every Notes tab is a notebook of its own, on the tab's record: a note
    /// written into one is not in the other, and each is there again when
    /// its tab is mounted again.
    #[test]
    fn each_notes_tab_is_a_notebook_of_its_own_kept_in_its_record() {
        let profile = tempfile::tempdir().unwrap();
        let mut apps = InProcessApps::at(profile.path(), "test-person").unwrap();
        let write = |apps: &mut InProcessApps, tab: u64, title: &str| {
            let component = apps
                .mount(
                    notes::definition(),
                    ROOM,
                    tab,
                    RecordId::for_tab(tab),
                    |_| {},
                )
                .unwrap();
            let mut harness = ui::headless::FrameHarness::from_component(
                component,
                (1000, 700),
                ui::application::ComponentRunnerInputs::from_native_process(),
            )
            .unwrap();
            harness.relayout();
            let import = notes::actions::import_markdown_action(
                uuid::Uuid::new_v4(),
                title,
                &format!("{title} body"),
                Vec::new(),
            );
            harness
                .invoke_bound_action(
                    &ui::bound_dispatch::BoundActionAddress {
                        instance_id: tab,
                        address: tilo_actions::ActionAddress::new(
                            notes::Notebook::KIND,
                            tilo_resource::APPLY,
                        ),
                    },
                    serde_json::json!({
                        tilo_resource::APPLY_PARAM: serde_json::to_value(import).unwrap()
                    })
                    .as_object()
                    .unwrap(),
                )
                .unwrap();
            harness.relayout();
            harness.flush_component_records().unwrap();
            harness.close_components();
        };
        write(&mut apps, 61, "Alpha");
        write(&mut apps, 62, "Beta");

        let titles = |apps: &mut InProcessApps, tab: u64| -> Vec<String> {
            let records = apps.owners.open(notes::definition().app_id, ROOM).unwrap();
            records
                .try_open_record(&RecordCheckout::new(
                    RecordId::for_tab(tab),
                    notes::definition().record_kind.clone(),
                ))
                .expect("the tab's record was kept");
            records
                .with(&RecordId::for_tab(tab), |doc| {
                    notes::Notebook::read(doc).map(|state| {
                        state
                            .pages()
                            .iter()
                            .map(|page| page.title.clone())
                            .collect::<Vec<_>>()
                    })
                })
                .expect("the tab's notebook is in its record")
                .unwrap()
        };
        assert_eq!(titles(&mut apps, 61), vec!["Alpha".to_owned()]);
        assert_eq!(
            titles(&mut apps, 62),
            vec!["Beta".to_owned()],
            "the second tab's notebook is its own"
        );
    }

    /// The shell's tile resolver mounts Notes and Editor tabs as those apps'
    /// own components; an Editor tab opened on a file is the editor's
    /// project of that file.
    #[test]
    fn notes_and_editor_tabs_are_their_apps_own_components() {
        let profile = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join("harbour-plan.txt");
        std::fs::write(&file, "moor at the east quay\n").unwrap();
        let file = file.canonicalize().unwrap();
        let mut inputs = crate::shell::hosted_inputs::HostedTileInputs::default();
        inputs.mount_apps_in_process(InProcessApps::at(profile.path(), "test-person").unwrap());

        let notes = mount_tab(&inputs, 71, notes_kind());
        assert!(
            notes
                .resources()
                .iter()
                .any(|resource| resource.kind == notes::Notebook::KIND),
            "a Notes tab publishes its notebook"
        );

        inputs.bind_document(
            72,
            crate::shell::hosted_inputs::HostedDocument {
                provider_id: "org.nirios.Editor",
                path: file,
                text: "moor at the east quay\n".into(),
            },
        );
        let editor = mount_tab(
            &inputs,
            72,
            crate::shell::state::AppKind::Other("Editor".into()),
        );
        assert!(
            editor
                .resources()
                .iter()
                .any(|resource| resource.kind == code_editor::CodeEditor::KIND),
            "an Editor tab publishes its project"
        );
        let mut editor = editor;
        assert!(
            editor.wait_until(ui::time::Duration::from_secs(10), |editor| {
                editor
                    .inspection_json()
                    .is_ok_and(|inspection| inspection.contains("harbour-plan.txt"))
            }),
            "the Editor tab opened the file it was opened on"
        );
    }
}
