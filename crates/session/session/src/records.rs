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
//! from where they are held (this device's workspace service on a computer, the page's replicated
//! store on the web), so a record made elsewhere — another workspace's note, a file-backed
//! document, one a peer brought into a workspace — is found by its name. Bringing it here is then
//! one write, a reference into the workspace in front: no bytes move between the two workspaces'
//! documents and no id is minted, because the id is the record.
//! [`ui::resource::index::duplicate`] is the deliberate act of copying one instead.
//!
//! Reading happens away from the session's loop where the host has threads, and again whenever
//! the change stream says a record of this device moved; the loop takes what it last read.

use std::cell::RefCell;
use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
use std::sync::Arc;

use crate::platform::Store;
use crate::platform::records::{Changes, Reading, Source, Watch};
use durable_store::DurableStore;
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
    /// What it holds as its app tells search, where this host reads that here
    /// ([`Records::over`]); empty otherwise.
    contents: Arc<[ui::search::index_publication::IndexEntry]>,
    /// For what a part of a chain the app keeps beside its own record holds, the part's
    /// storage key: its items are the app's own, opened through it.
    part: Option<String>,
}

/// Name a record in a workspace, in the index of an app: `(workspace, package, record)`. Whether
/// it is named.
#[cfg(target_os = "linux")]
type Bring = Box<dyn Fn(&str, &str, &DocumentCheckout) -> bool>;

/// The device's record indexes, as search offers them.
pub struct Records {
    /// The session's own store, where the activity and the mentions keep what this device saw.
    store: Store,
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
    /// The session's own store at `root`: the records the workspaces hold are read once the
    /// session holds them ([`Self::read_from`]).
    #[cfg(target_os = "linux")]
    #[must_use]
    pub fn of(root: PathBuf) -> Self {
        Self::new(app_state::shared_store(root), Changes::default())
    }

    /// The records of the workspace whose records `store` keeps as they reach this host, read
    /// again whenever `changes` says one moved: a page's, whose replication writes the store and
    /// notes each record it takes in. What it holds is the one workspace's, so nothing of another
    /// is brought here.
    ///
    /// `contents` reads each document for what its app tells search it holds
    /// ([`tilo_app::framework::resource_index::hosted_entries`]): what a note says in its body,
    /// and each note of a notebook as a document of its own. A page whose workspace answers its
    /// search needs none of that (the device serving the workspace indexes every document), and
    /// reading it would open every document the page holds each time one of them changed: the
    /// rows are offered by name from the workspace's indexes instead.
    #[must_use]
    pub fn over(store: Store, changes: Changes, contents: bool) -> Self {
        let records = Self::new(Store::clone(&store), changes.clone());
        let kept = crate::platform::records::Kept::over(Store::clone(&store), changes);
        #[cfg(target_os = "linux")]
        let source: Source = Arc::new(kept);
        #[cfg(not(target_os = "linux"))]
        let source: Source = std::rc::Rc::new(kept);
        *records.reading.borrow_mut() =
            Some(reading(source, &records.changes, contents.then_some(store)));
        records
    }

    fn new(store: Store, changes: Changes) -> Self {
        Self {
            store,
            changes,
            reading: RefCell::default(),
            #[cfg(target_os = "linux")]
            bring: RefCell::default(),
            names: RefCell::default(),
        }
    }

    /// Read the workspaces' documents from this device's workspace service `reads` from now on,
    /// as the change stream says they move, and name a record brought here through the session's
    /// copies of them (`named` ) as `person` . The session's index of what apps hold answers for
    /// their content ([`crate::resource_index`]): the records are offered by name.
    #[cfg(target_os = "linux")]
    pub(crate) fn read_from(
        &self,
        reads: Arc<dyn tilo_workspace_service::workspace_documents::DeviceDocuments>,
        named: crate::engine_records::EngineRecords,
        person: String,
    ) {
        let held = crate::platform::records::Held::over(reads, self.changes.clone());
        *self.reading.borrow_mut() = Some(reading(Arc::new(held), &self.changes, None));
        *self.bring.borrow_mut() = Some(Box::new(move |workspace, package, record| {
            named.reference(workspace, package, record, &person)
        }));
    }

    /// The session's own store.
    #[must_use]
    pub fn store(&self) -> Store {
        Store::clone(&self.store)
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
                contents: Arc::clone(&record.contents),
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

/// The indexes `source` holds, read now and again whenever the records of this device move; with
/// what each record holds as its app tells search where `contents` is the store they are kept in.
#[cfg(target_os = "linux")]
fn reading(source: Source, changes: &Changes, contents: Option<Store>) -> Reading<Vec<Held>> {
    let read = Contents::default();
    Reading::start(
        "tilo-search-records",
        changes.clone(),
        Watch::Any,
        move || {
            read_indexes(
                source.as_ref(),
                contents
                    .as_ref()
                    .map(|store| (store.as_ref() as &dyn DurableStore, &read)),
            )
        },
        || {},
    )
}

/// [`reading`] where this host has one thread: read when search asks, if the stream says they
/// moved since.
#[cfg(not(target_os = "linux"))]
fn reading(source: Source, changes: &Changes, contents: Option<Store>) -> Reading<Vec<Held>> {
    let read = Contents::default();
    Reading::read(changes, &Watch::Any, move || {
        read_indexes(
            source.as_ref(),
            contents
                .as_ref()
                .map(|store| (store.as_ref() as &dyn DurableStore, &read)),
        )
    })
}

/// What each record held as its app told search, by its id, with the heads it was read at: a record
/// that did not move since is not read for it again.
#[derive(Default)]
struct Contents(std::sync::Mutex<std::collections::HashMap<String, ReadContents>>);

/// The heads a record was read at, and what it held as its app told search then.
type ReadContents = (
    Vec<automerge::ChangeHash>,
    Arc<[ui::search::index_publication::IndexEntry]>,
);

/// How the storage key of a part of a chain starts ([`ui::resource::DocumentId::part`]).
const CHAIN_PART: &str = "derived_part.";

/// Every record the indexes `source` holds name, by id, with the workspaces that name it, and
/// with what each holds as its app tells search where `contents` keeps it. A record two
/// workspaces hold is one entry here: it is one record.
fn read_indexes(
    source: &dyn crate::platform::records::Records,
    contents: Option<(&dyn DurableStore, &Contents)>,
) -> Vec<Held> {
    let mut held: BTreeMap<String, Held> = BTreeMap::new();
    let primary = ui::resource::DocumentId::package_primary();
    for workspace in source.workspaces() {
        for (package, route) in source.held(&workspace) {
            // The app's own record of the workspace is the document that keeps its index.
            if !route.is_empty() {
                continue;
            }
            // The app's own record in the workspace (Notes' notebook, Tasks' tracker) names the
            // other records and not itself: what it holds is found as theirs is, where this host
            // reads what records hold.
            if let Some((store, _)) = contents {
                held.insert(
                    format!("{package}\u{1f}{workspace}"),
                    Held {
                        id: primary.as_str().to_owned(),
                        kind: String::new(),
                        name: None,
                        package: package.clone(),
                        workspaces: vec![workspace.clone()],
                        contents: Arc::from([]),
                        part: None,
                    },
                );
                // The parts of the chains the app keeps beside its own record (Notes' notes,
                // Tasks' issues) are named by no index: what they hold is found as the app's own.
                for stored in store.documents(&package, &workspace).unwrap_or_default() {
                    if !stored.starts_with(CHAIN_PART) {
                        continue;
                    }
                    held.insert(
                        format!("{package}\u{1f}{workspace}\u{1f}{stored}"),
                        Held {
                            id: primary.as_str().to_owned(),
                            kind: String::new(),
                            name: None,
                            package: package.clone(),
                            workspaces: vec![workspace.clone()],
                            contents: Arc::from([]),
                            part: Some(stored),
                        },
                    );
                }
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
                        contents: Arc::from([]),
                        part: None,
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
    let mut held: Vec<Held> = held.into_values().collect();
    if let Some((store, contents)) = contents {
        let mut kept = contents
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut read = std::collections::HashMap::with_capacity(held.len());
        for record in &mut held {
            let Some(mut doc) = resource_document(store, record) else {
                continue;
            };
            let heads = doc.get_heads();
            let key = format!(
                "{}\u{1f}{}\u{1f}{}",
                record.package,
                record.workspaces.first().map_or("", String::as_str),
                record.part.as_deref().unwrap_or(&record.id)
            );
            let entries = match kept.remove(&key) {
                Some((at, entries)) if at == heads => entries,
                _ => tilo_app::framework::resource_index::hosted_entries(&record.package, &doc)
                    .into(),
            };
            record.contents = Arc::clone(&entries);
            read.insert(key, (heads, entries));
        }
        *kept = read;
    }
    held
}

/// The document of `record`, as the first workspace naming it keeps it: what its app tells search
/// it holds is read from it. `None` for a record whose document is not here yet.
fn resource_document(store: &dyn DurableStore, record: &Held) -> Option<automerge::AutoCommit> {
    let workspace = record.workspaces.first()?;
    if let Some(part) = &record.part {
        return crate::platform::records::stored(store, &record.package, workspace, part);
    }
    let primary = ui::resource::DocumentId::package_primary();
    let route = if record.id == primary.as_str() {
        primary.route().to_owned()
    } else {
        ui::resource::DocumentId::parse(&record.id)?
            .route()
            .to_owned()
    };
    crate::platform::records::document(store, &record.package, workspace, &route)
}

/// This device as search's tests start from it: an engine holding workspaces, and the session's
/// copies of their records, through which a test writes an app's index as its windows do.
#[cfg(test)]
#[cfg(target_os = "linux")]
pub(crate) mod testing {
    use std::sync::Arc;

    use ed25519_dalek::SigningKey;
    use ui::permissions::MemberId;
    use ui::resource::{DocumentId, ResourceType};

    use crate::engine::Engine;
    use crate::engine_records::EngineRecords;

    /// Who writes the indexes and brings the records here.
    const PERSON: &str = "ada";
    /// The kind of every app's own record in these tests: the record that keeps its index.
    const OWN: &str = "notes.notebook";

    /// A test's stand-in for this device's workspace service: the engine's own copy of its
    /// workspaces, read as the service reads the documents it holds. The service itself holds
    /// them in production; these tests have no endpoint to open it on.
    pub(crate) struct EngineDocuments(pub(crate) Arc<Engine>);

    impl tilo_workspace_service::workspace_documents::DeviceDocuments for EngineDocuments {
        fn read(
            &self,
            workspace: &str,
            app: &str,
            resource: &str,
            since: Option<&[[u8; 32]]>,
        ) -> Result<tilo_workspace_service::workspace_documents::DocumentRead, String> {
            use tilo_workspace_service::workspace_documents::DocumentRead;
            match self.0.record_update(workspace, app, resource, since)? {
                crate::engine::ResourceReading::Captured {
                    revision,
                    heads,
                    bytes,
                } => Ok(DocumentRead::Whole {
                    revision,
                    heads,
                    bytes,
                }),
                crate::engine::ResourceReading::Changes {
                    revision,
                    heads,
                    bytes,
                } => Ok(DocumentRead::Changes {
                    revision,
                    heads,
                    bytes,
                }),
                crate::engine::ResourceReading::Unchanged(revision) => {
                    Ok(DocumentRead::Unchanged(revision))
                }
                crate::engine::ResourceReading::Pending => Ok(DocumentRead::Pending),
            }
        }

        fn listing(&self, workspace: &str) -> Result<Vec<(String, String)>, String> {
            self.0.documents(workspace)
        }

        fn holds(&self, workspace: &str, app: &str, resource: &str) -> bool {
            self.0.holds_resource(workspace, app, resource)
        }

        fn workspaces(&self) -> Vec<String> {
            self.0
                .workspaces()
                .into_iter()
                .map(|workspace| workspace.to_string())
                .collect()
        }
    }

    /// A test's stand-in for this device's workspace service, writing the engine's own copy of a
    /// workspace's documents as the service writes the ones it holds. The service itself holds
    /// them in production; these tests have no endpoint to open it on.
    pub(crate) struct EngineWrites(pub(crate) Arc<Engine>);

    impl tilo_workspace_service::workspace_documents::DeviceWrites for EngineWrites {
        fn write(
            &self,
            workspace: &str,
            app: &str,
            document: &str,
            changes: &[u8],
            founding: bool,
            sealed: Option<&key_group::ContentKeys>,
        ) -> tilo_workspace_service::workspace_documents::DocumentWrite {
            let engine = Arc::clone(&self.0);
            let workspace = workspace.to_owned();
            let (app, document) = (app.to_owned(), document.to_owned());
            let changes = changes.to_vec();
            let sealed = sealed.cloned();
            Box::pin(async move {
                let id = tilo_desktop_model::WorkspaceId::parse(&workspace).ok_or_else(|| {
                    tilo_peer_state::owner::WriteRefusal::Refused(format!(
                        "{workspace} is not a workspace"
                    ))
                })?;
                engine
                    .write_resource(id, (&app, &document), &changes, founding, sealed.as_ref())
                    .await
            })
        }
    }

    pub(crate) struct Device {
        engine: Arc<Engine>,
        records: EngineRecords,
        /// Search's records of this device, until a test takes them.
        found: std::cell::RefCell<Option<super::Records>>,
        _state: tempfile::TempDir,
    }

    impl Device {
        /// A device holding each of `workspaces` , with their keys.
        pub(crate) fn holding(workspaces: &[&str]) -> Self {
            let state = tempfile::tempdir().expect("a state directory");
            let root = SigningKey::from_bytes(&[0x30; 32]);
            let key = SigningKey::from_bytes(&[0x31; 32]);
            let cert = tilo_peer_state::owner::testing::certificate(&root, &key, "Laptop");
            let engine = Arc::new(
                Engine::open(
                    tilo_identity::identity_store::DeviceIdentity::kept(cert.clone(), key.clone()),
                    Some(state.path()),
                    std::iter::empty(),
                    crate::platform::membership_check::of_this_session(),
                )
                .expect("this device's engine opens"),
            );
            for workspace in workspaces {
                let id = tilo_desktop_model::WorkspaceId::parse(workspace).expect("a workspace");
                tilo_peer_state::owner::testing::enroll_interactive_governor_in(
                    engine.owner(),
                    &key,
                    &cert,
                    workspace,
                )
                .expect("the workspace is held here");
                engine.bind(id);
                let keys = key_group::KeyGroup::create(
                    id,
                    key_group::MemberKey::generate("owner").expect("a member key"),
                )
                .expect("the workspace's group")
                .content_keys()
                .expect("the workspace's keys");
                engine.hand_keys(id, keys);
            }
            let found = super::Records::of(state.path().join("data"));
            let changes = found.changes();
            let reads = Arc::new(EngineDocuments(Arc::clone(&engine)));
            let writes = Arc::new(EngineWrites(Arc::clone(&engine)));
            let records = EngineRecords::new(Arc::new(|| {}));
            records.attach(Arc::clone(&engine), reads.clone(), writes, &changes, None);
            found.read_from(reads, records.clone(), PERSON.to_owned());
            Self {
                engine,
                records,
                found: std::cell::RefCell::new(Some(found)),
                _state: state,
            }
        }

        /// Search's records of this device, read where the workspaces hold them, bringing a record
        /// here through the session's copies, as the session reads and brings them. Taken once.
        pub(crate) fn search(&self) -> super::Records {
            self.found
                .borrow_mut()
                .take()
                .expect("search's records, taken once")
        }

        /// `app`'s index of `workspace` names `rows` (id, kind, name) and nothing else, written as
        /// its windows write it and taken by the workspace.
        pub(crate) fn name(&self, app: &str, workspace: &str, rows: &[(&str, &str, Option<&str>)]) {
            let index = self
                .records
                .in_workspace(workspace, MemberId::person(PERSON.to_owned()))
                .open_primary(app, OWN, &|_| Ok(()))
                .expect("the app's own record");
            index
                .edit(false, |doc| {
                    for held in ui::resource::index::references(doc) {
                        ui::resource::index::unreference(doc, &held.id);
                    }
                    for (id, kind, name) in rows {
                        let id = DocumentId::parse(id).expect("a record id");
                        ui::resource::index::reference(doc, &id, &ResourceType::new(*kind));
                        ui::resource::index::set_name(doc, &id, *name);
                    }
                    Ok(())
                })
                .expect("the index is written");
            index.flush();
            let mut named: Vec<String> = rows.iter().map(|row| row.0.to_owned()).collect();
            named.sort();
            let started = std::time::Instant::now();
            while self.names(app, workspace) != named {
                assert!(
                    started.elapsed() < tilo_test_env::hang_bound(),
                    "the workspace's index names {:?}, not {named:?}; the session's copy names {:?}",
                    self.names(app, workspace),
                    index.read(ui::resource::index::references),
                );
                self.records.replicate();
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }

        /// The records `app`'s index of `workspace` names, as the workspace holds it.
        pub(crate) fn names(&self, app: &str, workspace: &str) -> Vec<String> {
            let Ok(crate::engine::ResourceReading::Captured { bytes, .. }) =
                self.engine.record_update(workspace, app, "", None)
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

        /// Replicate the session's copies until `done`.
        pub(crate) fn until(&self, done: impl Fn() -> bool) {
            let started = std::time::Instant::now();
            while !done() {
                assert!(
                    started.elapsed() < tilo_test_env::hang_bound(),
                    "the workspace never took what the session wrote"
                );
                self.records.replicate();
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

    const NOTES: &str = ui::apps::NOTES;
    const KITCHEN: &str = "6d1c1f36-0000-4000-8000-000000000001";
    const STUDIO: &str = "6d1c1f36-0000-4000-8000-000000000002";
    const NOTE: &str = "tab:00000000000000a1";

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
