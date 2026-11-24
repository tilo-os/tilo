//! The records the workspaces on this device hold, and the reference that
//! brings one of them into the workspace in front.
//!
//! A workspace holds a record by naming it: each of its apps keeps a
//! workspace index ([`ui::resource::index`]) saying which records the workspace
//! holds and what kind each one is, one document per app and workspace.
//! Every app of the workspace writes that index as a shared writer, so this
//! session writes it too: naming a record there is the same act as the app
//! naming it, and what it names is what the workspace's apps read back as the
//! records they may open.
//!
//! Search offers every index the workspaces on this device hold, as the worker last read them
//! from this device's workspace service, so a record made elsewhere — another workspace's note, a file-backed
//! document, one a peer brought into a workspace — is found by its name. Bringing it here is then
//! one write, a reference into the workspace in front: no bytes move between the two workspaces'
//! documents and no id is minted, because the id is the record.
//! [`ui::resource::index::duplicate`] is the deliberate act of copying one instead.
//!
//! Reading happens away from the session's loop where the host has threads, and again whenever
//! the change stream says a record of this device moved; the loop takes what it last read.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::platform::records::{Changes, Reading, Source, Watch};
use tilo_desktop_search::providers::records::HeldDocument;
use ui::resource::DocumentCheckout;

/// A record one or more workspaces on this device hold: what search offers,
/// without the names of the workspaces, which only the shell knows.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Held {
    id: String,
    kind: String,
    /// What the index that named it first says it is called.
    name: Option<String>,
    /// The app whose index named it first: the app that opens it, and whose index the reference
    /// goes into when the record is brought here.
    package: String,
    /// Every workspace whose index names it.
    workspaces: Vec<String>,
}

/// Name a record in a workspace, in the index of an app: `(workspace, package, record)`. Whether
/// it is named.
#[cfg(target_os = "linux")]
type Bring = Box<dyn Fn(&str, &str, &DocumentCheckout) -> bool>;

/// The device's record indexes, as search offers them.
#[derive(Default)]
pub struct Records {
    /// The one stream of record changes of this device.
    changes: Changes,
    /// The indexes as last read, read again whenever the change stream says a record of this
    /// device moved: nothing until the records are read from where they are held.
    reading: RefCell<Option<Reading<Vec<Held>>>>,
    /// Where a record is named in a workspace when it is brought here; `None` for a host whose
    /// records reach it from one workspace, which brings none of another here. A page is such a
    /// host.
    #[cfg(target_os = "linux")]
    bring: RefCell<Option<Bring>>,
    /// The name the shell gives each workspace, as of its last frame: a record held elsewhere is
    /// offered under the name of the workspace that holds it, and a workspace id alone names
    /// nothing a person knows.
    names: RefCell<Vec<(String, String)>>,
}

impl Records {
    /// The records the workspaces on this device hold, read once the session holds them
    /// ([`Self::read_from`]).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the workspaces' documents from this device's workspace service `reads` from now on,
    /// as the change stream says they move, and name a resource brought here in its app's index
    /// through the index's own action at the device's daemon (`daemon`), which runs it as the
    /// person. The session's index of what apps hold answers for their content
    /// ([`crate::resource_index`]): the resources are offered by name.
    #[cfg(target_os = "linux")]
    pub(crate) fn read_from(
        &self,
        reads: Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
        daemon: tilo_session_client::daemon_documents::DaemonShell,
    ) {
        let held = crate::platform::records::Held::over(reads, self.changes.clone());
        *self.reading.borrow_mut() = Some(reading(Arc::new(held), &self.changes));
        *self.bring.borrow_mut() = Some(Box::new(move |workspace, package, record| {
            match daemon.invoke_blocking(workspace, reference(package, record)) {
                Ok(_) => true,
                Err(error) => {
                    tracing::warn!(%error, package, "a resource was not brought here");
                    false
                }
            }
        }));
    }

    /// The one stream of record changes of this device.
    #[must_use]
    pub fn changes(&self) -> Changes {
        self.changes.clone()
    }

    /// The workspaces this device shows, by id and the name the shell gives it: what a row says of a
    /// record no workspace in front holds.
    pub fn set_workspaces(&self, workspaces: Vec<(String, String)>) {
        *self.names.borrow_mut() = workspaces;
    }

    /// The records the workspaces on this device hold, as search offers them in the workspace
    /// `workspace` : one row per record, saying whether this workspace holds it and, when another
    /// one does, which.
    #[must_use]
    pub fn held(&self, workspace: &str) -> Vec<HeldDocument> {
        let held = self.latest();
        let names = self.names.borrow();
        held.iter()
            .map(|record| HeldDocument {
                id: record.id.clone(),
                kind: record.kind.clone(),
                name: record.name.clone(),
                package: record.package.clone(),
                here: record.workspaces.iter().any(|held_in| held_in == workspace),
                workspace: record
                    .workspaces
                    .iter()
                    .find(|held_in| held_in.as_str() != workspace)
                    .and_then(|held_in| {
                        names
                            .iter()
                            .find(|(named, _)| named == held_in)
                            .map(|(_, name)| name.clone())
                    }),
                contents: Arc::from([]),
            })
            .collect()
    }

    /// Whether any workspace on this device still names `record` . A row is collected from one
    /// reading of the indexes and chosen later, and an app may have stopped naming the record in
    /// between: naming it here would hold a record no workspace has, so such a row is refused
    /// instead.
    #[must_use]
    pub fn holds(&self, record: &str) -> bool {
        self.latest().iter().any(|held| held.id == record)
    }

    /// Name `record` in the workspace `workspace` , in the index of `package` : the reference it
    /// holds from then on, and the whole of what bringing a record here does, written into the
    /// workspace as its app's windows write it. Whether this host brings records here at all.
    pub fn bring_here(&self, workspace: &str, package: &str, record: &DocumentCheckout) -> bool {
        #[cfg(target_os = "linux")]
        if let Some(bring) = self.bring.borrow().as_ref() {
            return bring(workspace, package, record);
        }
        let _ = (workspace, package, record);
        false
    }

    /// The indexes as last read.
    fn latest(&self) -> Arc<Vec<Held>> {
        self.reading
            .borrow()
            .as_ref()
            .map_or_else(Arc::default, |reading| {
                reading.following(&self.changes, &Watch::Any)
            })
    }
}

/// The action naming `record` in `package`'s index of a workspace.
#[cfg(target_os = "linux")]
fn reference(package: &str, record: &DocumentCheckout) -> tilo_actions::remote::InvokeRequest {
    use ui::resource::index;
    let mut arguments = serde_json::Map::new();
    arguments.insert("id".to_owned(), record.id.as_str().into());
    arguments.insert("kind".to_owned(), record.kind.as_str().into());
    tilo_actions::remote::InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: tilo_actions::remote::RemoteActionTarget {
            provider: format!("{}:{package}:", tilo_actions::remote::RESOURCE_PROVIDER),
            address: tilo_actions::ActionAddress::new(index::KIND, index::REFERENCE),
        },
        argument: tilo_actions::remote::InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// The indexes `source` holds, read now and again whenever the records of this device move.
#[cfg(target_os = "linux")]
fn reading(source: Source, changes: &Changes) -> Reading<Vec<Held>> {
    Reading::start(
        "tilo-search-records",
        changes.clone(),
        Watch::Any,
        move || read_indexes(source.as_ref()),
        || {},
    )
}

/// [`reading`] where this host has one thread: read when search asks, if the stream says they
/// moved since.
#[cfg(not(target_os = "linux"))]
fn reading(source: Source, changes: &Changes) -> Reading<Vec<Held>> {
    Reading::read(changes, &Watch::Any, move || read_indexes(source.as_ref()))
}

/// Every record the indexes `source` holds name, by id, with the workspaces that name it. A record
/// two workspaces hold is one entry here: it is one record.
fn read_indexes(source: &dyn crate::platform::records::Records) -> Vec<Held> {
    let mut held: BTreeMap<String, Held> = BTreeMap::new();
    for workspace in source.workspaces() {
        for (package, route) in source.held(&workspace) {
            // The app's own record of the workspace is the document that keeps its index.
            if !route.is_empty() {
                continue;
            }
            let Some(index) = source.document(&package, &workspace, "") else {
                continue;
            };
            for record in ui::resource::index::discoverable_references(&index) {
                let entry = held
                    .entry(record.id.as_str().to_owned())
                    .or_insert_with(|| Held {
                        id: record.id.as_str().to_owned(),
                        kind: record.kind.as_str().to_owned(),
                        name: None,
                        package: package.clone(),
                        workspaces: Vec::new(),
                    });
                if entry.name.is_none() {
                    entry.name = ui::resource::index::name_of(&index, &record.id);
                }
                if !entry.workspaces.contains(&workspace) {
                    entry.workspaces.push(workspace.clone());
                }
            }
        }
    }
    held.into_values().collect()
}

/// This device as search's tests start from it: a daemon holding workspaces, and the session's
/// copies of their records, through which a test writes an app's index as its windows do.
#[cfg(test)]
#[cfg(target_os = "linux")]
pub(crate) mod testing {
    use std::sync::Arc;

    use ui::resource::{DocumentCheckout, DocumentId, ResourceType};

    /// The type of every app's own resource in these tests: the one that keeps its index.
    const OWN: &str = "notes.notebook";

    pub(crate) struct Device {
        service: crate::daemon_fixture::TestDaemon,
        changes: crate::platform::records::Changes,
        /// Search's view of this device, until a test takes it.
        found: std::cell::RefCell<Option<super::Records>>,
    }

    impl Device {
        /// A device holding each of `workspaces` on its daemon.
        pub(crate) fn holding(workspaces: &[&str]) -> Self {
            let service = crate::daemon_fixture::TestDaemon::start();
            for workspace in workspaces {
                let id = tilo_desktop_model::WorkspaceId::parse(workspace).expect("a workspace");
                service.found(id, workspace);
            }
            let found = super::Records::new();
            let changes = found.changes();
            found.read_from(service.reads(), service.shell());
            Self {
                service,
                changes,
                found: std::cell::RefCell::new(Some(found)),
            }
        }

        /// Search's view of this device, read where the workspaces hold it, bringing a resource
        /// here through the daemon, as the session reads and brings them. Taken once.
        pub(crate) fn search(&self) -> super::Records {
            self.found
                .borrow_mut()
                .take()
                .expect("search's view, taken once")
        }

        /// The session's one stream of resource changes.
        pub(crate) fn changes(&self) -> crate::platform::records::Changes {
            self.changes.clone()
        }

        /// The workspaces this device's service holds, as the session's roles reach them.
        pub(crate) fn workspaces(&self) -> tilo_workspace_service::workspaces::Workspaces {
            self.service.workspaces().clone()
        }

        /// Write `app`'s `id` of `workspace` as `edit` changes it, as this device writes it: what
        /// the workspace holds of it changed, or `found` when it holds nothing of it yet.
        fn write(
            &self,
            workspace: &str,
            (app, id): (&str, &str),
            found: impl FnOnce() -> automerge::AutoCommit,
            edit: impl FnOnce(&mut automerge::AutoCommit),
        ) {
            use tilo_peer_state::workspace_documents::{DocumentRead, DocumentWriting};
            let held = match self.service.reads().read(workspace, app, id, None) {
                Ok(DocumentRead::Whole { bytes, .. }) => {
                    Some(automerge::AutoCommit::load(&bytes).expect("the workspace's copy loads"))
                }
                Ok(DocumentRead::Absent) => None,
                other => panic!(
                    "the workspace does not read {app}'s {id:?}: {:?}",
                    other.err()
                ),
            };
            let founding = held.is_none();
            let mut doc = held.unwrap_or_else(found);
            let heads = if founding {
                Vec::new()
            } else {
                doc.get_heads()
            };
            edit(&mut doc);
            let changes = if founding {
                doc.save()
            } else {
                doc.save_after(&heads)
            };
            tilo_rt::app()
                .block_on(self.service.writes().write(
                    workspace,
                    &changes,
                    DocumentWriting {
                        app,
                        document: id,
                        readers: None,
                        founding,
                        snapshot: founding,
                        sealed: None,
                    },
                ))
                .expect("the runtime runs")
                .expect("the workspace takes the write");
        }

        /// Keep `doc` as `app`'s `record` of `workspace`, written into the workspace as a service of
        /// the session writes it.
        pub(crate) fn keep(
            &self,
            workspace: &str,
            (app, record): (&str, &str),
            mut doc: automerge::AutoCommit,
            _initialize: &dyn Fn(&mut automerge::AutoCommit) -> Result<(), String>,
        ) {
            let bytes = doc.save();
            self.write(
                workspace,
                (app, record),
                || automerge::AutoCommit::load(&bytes).expect("the resource loads"),
                |held| {
                    held.load_incremental(&bytes)
                        .expect("the resource takes what it holds");
                },
            );
        }

        /// The workspaces this device's daemon holds, as the session reads and writes a record of
        /// theirs itself.
        pub(crate) fn held(&self) -> crate::device_workspaces::DeviceWorkspaces {
            crate::device_workspaces::DeviceWorkspaces::new(
                self.service.view(),
                None,
                self.service.reads(),
                self.service.writes(),
                self.changes.clone(),
            )
        }

        /// What reads the documents of the workspaces this device's service holds.
        pub(crate) fn reads(
            &self,
        ) -> Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments> {
            self.service.reads()
        }

        /// `app`'s index of `workspace` names `rows` (id, type, name) and nothing else, written as
        /// its windows write it and taken by the workspace.
        pub(crate) fn name(&self, app: &str, workspace: &str, rows: &[(&str, &str, Option<&str>)]) {
            self.write(
                workspace,
                (app, ""),
                || {
                    let mut doc = automerge::AutoCommit::new();
                    let checkout = DocumentCheckout::new(
                        DocumentId::package_primary(),
                        ResourceType::new(OWN),
                    );
                    ui::resource::genesis(&mut doc, &checkout, workspace, |_| Ok(()))
                        .expect("the app's own resource is made");
                    doc
                },
                |doc| {
                    for held in ui::resource::index::references(doc) {
                        ui::resource::index::unreference(doc, &held.id);
                    }
                    for (id, kind, name) in rows {
                        let id = DocumentId::parse(id).expect("a resource id");
                        ui::resource::index::reference(doc, &id, &ResourceType::new(*kind));
                        ui::resource::index::set_name(doc, &id, *name);
                    }
                    doc.commit();
                },
            );
            let mut named: Vec<String> = rows.iter().map(|row| row.0.to_owned()).collect();
            named.sort();
            assert_eq!(
                self.names(app, workspace),
                named,
                "the workspace's index names the rows"
            );
        }

        /// The resources `app`'s index of `workspace` names, as the workspace holds it.
        pub(crate) fn names(&self, app: &str, workspace: &str) -> Vec<String> {
            let Ok(tilo_peer_state::workspace_documents::DocumentRead::Whole { bytes, .. }) =
                self.service.reads().read(workspace, app, "", None)
            else {
                return Vec::new();
            };
            let mut named: Vec<String> = automerge::AutoCommit::load(&bytes)
                .map(|index| ui::resource::index::references(&index))
                .unwrap_or_default()
                .into_iter()
                .map(|checkout| checkout.id.as_str().to_owned())
                .collect();
            named.sort();
            named
        }

        /// Wait until `done`.
        pub(crate) fn until(&self, done: impl Fn() -> bool) {
            let started = std::time::Instant::now();
            while !done() {
                assert!(
                    started.elapsed() < tilo_test_env::hang_bound(),
                    "the workspace never took what the session wrote"
                );
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::testing::Device;
    use ui::resource::{DocumentCheckout, DocumentId, ResourceType};

    const NOTES: &str = "so.tilo.notes";
    const KITCHEN: &str = "6d1c1f36-0000-4000-8000-000000000001";
    const STUDIO: &str = "6d1c1f36-0000-4000-8000-000000000002";
    const NOTE: &str = "tab:00000000000000a1";

    /// A document the workspace holds nothing of reads as absent, as its service answers for one
    /// it holds nothing of: never as an empty document, which a read takes for the one the
    /// workspace keeps.
    #[test]
    fn a_document_the_workspace_holds_nothing_of_reads_as_absent() {
        use tilo_peer_state::workspace_documents::DocumentRead;

        let device = Device::holding(&[KITCHEN]);
        let reads = device.reads();
        assert!(
            matches!(
                reads.read(KITCHEN, NOTES, "", None),
                Ok(DocumentRead::Absent)
            ),
            "no one wrote the app's own document yet"
        );
        device.name(NOTES, KITCHEN, &[(NOTE, "document", Some("Groceries"))]);
        drop(device);
        assert!(
            matches!(
                reads.read(KITCHEN, NOTES, "", None),
                Ok(DocumentRead::Whole { .. })
            ),
            "the document the workspace holds reads whole"
        );
    }

    /// A record one workspace's app names is found from another workspace under the name of the
    /// workspace holding it, and brought here is named in this workspace's index: what the app's
    /// windows here read as the records the workspace holds.
    #[test]
    fn a_record_of_another_workspace_is_found_and_brought_here() {
        let device = Device::holding(&[KITCHEN, STUDIO]);
        device.name(NOTES, KITCHEN, &[(NOTE, "document", Some("Groceries"))]);
        let records = device.search();
        records.set_workspaces(vec![
            (KITCHEN.to_owned(), "Kitchen".to_owned()),
            (STUDIO.to_owned(), "Studio".to_owned()),
        ]);

        // Search reads the indexes again once the session's write is noted as taken.
        device.until(|| records.holds(NOTE));
        let found = records.held(STUDIO);
        let row = found
            .iter()
            .find(|row| row.id == NOTE)
            .unwrap_or_else(|| panic!("search offers the note: {found:?}"));
        assert_eq!(
            row.name.as_deref(),
            Some("Groceries"),
            "named as its index names it"
        );
        assert_eq!(row.package, NOTES);
        assert!(!row.here, "the workspace in front does not hold it yet");
        assert_eq!(row.workspace.as_deref(), Some("Kitchen"), "{row:?}");

        let note = DocumentCheckout::new(
            DocumentId::parse(NOTE).expect("a record id"),
            ResourceType::new("document"),
        );
        assert!(records.bring_here(STUDIO, NOTES, &note));
        device.until(|| device.names(NOTES, STUDIO) == [NOTE]);
        // The write the workspace took is news to search, which reads the indexes again.
        device.until(|| {
            records
                .held(STUDIO)
                .iter()
                .any(|row| row.id == NOTE && row.here)
        });
    }
}
