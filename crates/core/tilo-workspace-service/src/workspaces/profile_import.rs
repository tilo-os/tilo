//! What native apps kept on this device in files of their own, before their records were the
//! workspace's, moved into the workspaces they belong to, once.
//!
//! A native app kept each record of a workspace as an Automerge file in the data directory the
//! launcher gave it, and each window's saved view beside it, and read them there. Its records and
//! views are the workspace store's resources now, read and written through this service, and
//! nothing reads those files any more. So this service reads them once for each workspace it
//! holds and moves what they kept into that workspace's store as this device: each record, and
//! each saved view as the view document the app's windows read, founded under its app's kind and
//! id, holding what the file held, as one write a store keeps whole or not at all. A record the store holds already is
//! newer than its file, and is left. The files stay where they are, untouched.
//!
//! Where the files are, below each data home ([`homes`]) as the launcher laid them out:
//!
//! - `tilo-workspaces/<workspace>/<package>/.local/share`, the data directory of each package
//!   launched into a workspace, and the data home itself, an app's run without the launcher;
//! - `tilo-person/<package>`, each package's records of the person's own;
//! - in each, `<app>/workspaces/<workspace>/documents/<key>/state.automerge`, a record (whole,
//!   with the changes after it appended); `<app>/workspaces/<workspace>/state.automerge`, the
//!   workspace's index of the app's records; `<app>/state.automerge`, what the app kept before it
//!   kept anything per workspace; and `instance-views/<hex device>/<hex workspace>/<n>.postcard`,
//!   a window's saved view.
//!
//! The move is an action, [`IMPORT`] on the device's resource ([`device_resource`]) under
//! [`PROVIDER`] on the device action service, and it runs once for each device and workspace:
//! a once-only turn ([`crate::work::Turn::Once`], [`import_turn`]) whose inputs name the device,
//! since the files are on that device alone. The turn keeps what it moved ([`Moved`]), and every
//! later ask, by this device's start or anyone's invocation, is answered with that. The service
//! asks for the turn at each start and each join; a workspace this service does not hold yet
//! keeps its files until a start, or a join, that holds it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc as _};
use collab_types::document::DocumentId;

use super::Workspaces;

/// The provider the import of a device's app files is published under on the device action
/// service.
pub const PROVIDER: &str = "tilo.app-files";
/// The action that moves what a device's apps kept in files into the workspace the request asks
/// in, on the device's resource ([`device_resource`]).
pub const IMPORT: &str = "import";
/// Below a data home: each package's data directory in each workspace it was launched into.
const LAUNCHED: &str = "tilo-workspaces";
/// Below a launched package's directory: the data directory the app kept its files in.
const LAUNCHED_DATA: &str = ".local/share";
/// Below a data home: each package's records of the person's own.
const PERSON: &str = "tilo-person";
/// The scope an app kept the person's own records in, which their own cluster holds now.
const PERSONAL_SCOPE: &str = "personal";
const WORKSPACES: &str = "workspaces";
const DOCUMENTS: &str = "documents";
const DOCUMENT: &str = "state.automerge";
const VIEWS: &str = "instance-views";
/// Where a workspace's index names its records, by id.
const INDEX: &str = "tilo:records";
/// Where a window's view document keeps the view's bytes, as the app's windows read it.
const VIEW: &str = "view";

/// The data homes on this device an app's launches kept their files under: the platform's data
/// directory, and on macOS the one the launcher there gives apps.
#[must_use]
pub fn homes() -> Vec<PathBuf> {
    let mut homes: Vec<PathBuf> = launcher::appdir::host_data_home().into_iter().collect();
    #[cfg(target_os = "macos")]
    homes.extend(
        std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join("Library/Application Support/Tilo")),
    );
    #[cfg(windows)]
    homes.extend(std::env::var_os("LOCALAPPDATA").map(PathBuf::from));
    homes.retain(|home| home.is_absolute());
    homes.dedup();
    homes
}

/// The resource of `device`, by its key in a workspace's roster, that the import of its files
/// acts on.
#[must_use]
pub fn device_resource(device: &[u8; 32]) -> String {
    format!("identity.device:{}", hex::encode(device))
}

/// The import of `device`'s files, as its provider declares it: an undoable change of the
/// workspace, which keeps each record it moves in as a change of its own.
fn import_descriptor(device: &[u8; 32]) -> tilo_actions::ActionDescriptor {
    tilo_actions::ActionDescriptor {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new(device_resource(device), IMPORT),
        resource_kind: "identity.device".into(),
        resource_label: "This device".into(),
        action_label: "Move this device's app files in".into(),
        keywords: Vec::new(),
        icon: None,
        description: Some(
            "Move what this device's apps kept in files into the workspace, once.".into(),
        ),
        locus: tilo_actions::ExecutionLocus::ClientLocal,
        kind: tilo_actions::OperationKind::Effect,
        contract: tilo_actions::ActionContract {
            kind: tilo_actions::ActionKind::Undoable,
            ..tilo_actions::ActionContract::default()
        },
        input: tilo_actions::InputSpec::None,
        params: Vec::new(),
        view: None,
    }
}

/// The once-only turn that moves `device`'s files into a workspace: one per device and
/// workspace, since the files are on that device alone.
#[must_use]
pub fn import_turn(device: &[u8; 32]) -> crate::work::Turn {
    crate::work::Turn::once(
        format!("{PROVIDER}/{}#{IMPORT}", device_resource(device)),
        serde_json::json!({ "device": hex::encode(device) }),
    )
}

/// What the import of a device's files moved into a workspace, the answer its turn keeps.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Moved {
    /// The device whose files they were, its key in hex.
    pub device: String,
    /// How many documents went in, by app: a record, a window's saved view, or what the app kept
    /// before it kept anything per workspace. An app none of whose documents had to go in is
    /// absent.
    pub apps: BTreeMap<String, usize>,
}

/// What one file kept, as it goes into its workspace.
struct Kept {
    app: String,
    /// The record's id.
    id: String,
    /// The kind the record names itself, when it names one.
    kind: Option<String>,
    /// Whether it is a window's saved view, which its person alone reads.
    view: bool,
    /// The record, whole.
    document: AutoCommit,
}

/// Where a profile's files came from, which decides the workspace a scope of it belongs to.
#[derive(Clone, Copy)]
enum Profile<'a> {
    /// A package's data directory in the workspace the launcher named by this directory.
    Launched(&'a str),
    /// The data home itself, or a package's records of the person.
    Other,
}

/// Everything below `homes` that belongs to a workspace, by the workspace: `person` is the root
/// key of the device's person, whose own cluster takes the records they kept as their own.
fn kept_below(homes: &[PathBuf], person: &[u8; 32]) -> BTreeMap<String, Vec<Kept>> {
    let personal = collab_types::personal_cluster(person).to_string();
    let mut kept = BTreeMap::new();
    for home in homes {
        read_profile(home, Profile::Other, &personal, &mut kept);
        for slug in subdirectories(&home.join(LAUNCHED)) {
            let Some(name) = slug.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            for package in subdirectories(&slug) {
                read_profile(
                    &package.join(LAUNCHED_DATA),
                    Profile::Launched(name),
                    &personal,
                    &mut kept,
                );
            }
        }
        for package in subdirectories(&home.join(PERSON)) {
            read_profile(&package, Profile::Other, &personal, &mut kept);
        }
    }
    kept
}

/// The directories in `directory`, none when it is not one.
fn subdirectories(directory: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect();
    found.sort();
    found
}

/// Whether `name` can be an app's id: what an app's directory is named, never a hidden one.
fn app_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('.') && !name.contains(['/', '\\', '\0'])
}

/// The workspace a scope an app kept records in belongs to: a workspace by its id, or the
/// person's own cluster for their own scope.
fn workspace_of(scope: &str, personal: &str) -> Option<String> {
    if scope == PERSONAL_SCOPE {
        return Some(personal.to_owned());
    }
    workspace_id(scope)
}

/// The workspace `name` is the id of, in the form the workspace's routes name it.
fn workspace_id(name: &str) -> Option<String> {
    tilo_failure::ignored!(
        "a name is a workspace only when it is a workspace's id",
        uuid::Uuid::parse_str(name).ok()
    )
    .map(|workspace| workspace.to_string())
}

/// A file's document, loaded whole: a record's file is its saved document with each change
/// after it appended, which loads as one.
fn load(file: &Path) -> Option<AutoCommit> {
    let bytes = std::fs::read(file).ok().filter(|bytes| !bytes.is_empty())?;
    match AutoCommit::load(&bytes) {
        Ok(document) => Some(document),
        Err(error) => {
            tracing::warn!(file = %file.display(), %error, "a file a native app kept does not load, and is left");
            None
        }
    }
}

/// What the app data directory `root` kept, by the workspace each of it belongs to.
fn read_profile(
    root: &Path,
    profile: Profile<'_>,
    personal: &str,
    kept: &mut BTreeMap<String, Vec<Kept>>,
) {
    for app_dir in subdirectories(root) {
        let Some(app) = app_dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if app == VIEWS || !app_name(app) {
            continue;
        }
        for scope_dir in subdirectories(&app_dir.join(WORKSPACES)) {
            let Some(workspace) = scope_dir
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|scope| workspace_of(scope, personal))
            else {
                continue;
            };
            let indexed = load(&scope_dir.join(DOCUMENT))
                .map(|index| indexed_ids(&index))
                .unwrap_or_default();
            for record_dir in subdirectories(&scope_dir.join(DOCUMENTS)) {
                let Some(key) = record_dir.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                let Some(document) = load(&record_dir.join(DOCUMENT)) else {
                    continue;
                };
                let Some(id) = record_id(&document, key, &indexed) else {
                    tracing::warn!(file = %record_dir.display(), "a record a native app kept names no id, and is left");
                    continue;
                };
                let kind = collab_types::document::identity::identity(&document)
                    .map(|checkout| checkout.kind.as_str().to_owned());
                kept.entry(workspace.clone()).or_default().push(Kept {
                    app: app.to_owned(),
                    id: id.route().to_owned(),
                    kind,
                    view: false,
                    document,
                });
            }
        }
        // What the app kept before it kept anything per workspace was taken in by the first
        // workspace the app opened in: the one the launcher gave this data directory.
        if let Profile::Launched(launched) = profile
            && let Some(workspace) = workspace_of(launched, personal)
            && let Some(document) = load(&app_dir.join(DOCUMENT))
        {
            kept.entry(workspace).or_default().push(Kept {
                app: app.to_owned(),
                id: DocumentId::package_primary().route().to_owned(),
                kind: None,
                view: false,
                document,
            });
        }
    }
    read_views(&root.join(VIEWS), kept);
}

/// The ids a workspace's index names.
fn indexed_ids(index: &AutoCommit) -> Vec<DocumentId> {
    let Ok(Some((automerge::Value::Object(ObjType::Map), records))) = index.get(ROOT, INDEX) else {
        return Vec::new();
    };
    index
        .keys(&records)
        .filter_map(|id| DocumentId::parse(&id))
        .collect()
}

/// The id of the record kept under `key`: the one it names itself, else the one of the
/// workspace's own records or its index's whose key it is. A key is the id's
/// [`DocumentId::storage_key`], which no id can be read back from.
fn record_id(document: &AutoCommit, key: &str, indexed: &[DocumentId]) -> Option<DocumentId> {
    let named = collab_types::document::identity::identity(document).map(|checkout| checkout.id);
    named
        .into_iter()
        .chain([
            DocumentId::package_primary(),
            DocumentId::workspace_default(),
        ])
        .chain(DocumentId::window_default_of_storage_key(key))
        .chain(indexed.iter().cloned())
        .find(|id| id.storage_key() == key)
}

/// The fields every saved view starts with, which say whose view it is.
#[derive(serde::Deserialize)]
struct ViewHead {
    _version: u32,
    app_id: String,
    device_id: String,
    workspace_id: String,
    instance_id: u64,
}

/// Each window's saved view below `views`, as the view document its app's windows read: a
/// document of the app's in the view's workspace, named for the device and the window.
fn read_views(views: &Path, kept: &mut BTreeMap<String, Vec<Kept>>) {
    for device in subdirectories(views) {
        for workspace in subdirectories(&device) {
            for file in std::fs::read_dir(&workspace)
                .into_iter()
                .flatten()
                .flatten()
            {
                let path = file.path();
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                let Ok((head, _)) = postcard::take_from_bytes::<ViewHead>(&bytes) else {
                    continue;
                };
                // A view is taken in only where its file is the one its app kept it in.
                let named = |path: &Path, name: String| {
                    path.file_name().and_then(|named| named.to_str()) == Some(name.as_str())
                };
                let kept_where = named(&device, hex::encode(&head.device_id))
                    && named(&workspace, hex::encode(&head.workspace_id))
                    && named(&path, format!("{}.postcard", head.instance_id));
                let Some(id) = workspace_id(&head.workspace_id)
                    .filter(|_| kept_where && app_name(&head.app_id))
                else {
                    continue;
                };
                let mut document = AutoCommit::new();
                if document.put(ROOT, VIEW, bytes).is_err() {
                    continue;
                }
                document.commit();
                let view = DocumentId::window_view(&head.device_id, head.instance_id);
                kept.entry(id).or_default().push(Kept {
                    app: head.app_id,
                    id: view.route().to_owned(),
                    kind: None,
                    view: true,
                    document,
                });
            }
        }
    }
}

impl Workspaces {
    /// Move what native apps kept in files on this device for `workspace` into it, as the
    /// [module](self) says: [`IMPORT`], run once for this device's files through the turn
    /// [`import_turn`] names. A second ask, from this device or any other, is answered with what
    /// the first one moved ([`Moved`]), and moves nothing.
    ///
    /// # Errors
    ///
    /// The turn is not this device's to run now ([`crate::work::NotTaken`]), or a document did
    /// not go in: the turn is left open, and the next start runs it again.
    pub(crate) async fn import_app_files(
        &self,
        workspace: &str,
    ) -> Result<serde_json::Value, String> {
        let _importing = self.inner.host.app_files.importing.lock().await;
        let mut kept = self.app_files_kept().await;
        self.import_once(workspace, kept.remove(workspace).unwrap_or_default())
            .await
    }

    /// Ask for the import of this device's files ([`Self::import_app_files`]) in each workspace
    /// this service holds that its apps kept files of, as a start or a join does: what each one
    /// answered, by workspace. A workspace not held here keeps its files for the start, or the
    /// join, that holds it.
    pub(crate) async fn import_held_app_files(
        &self,
    ) -> BTreeMap<String, Result<serde_json::Value, String>> {
        let _importing = self.inner.host.app_files.importing.lock().await;
        let held = self.held_workspaces();
        let mut answered = BTreeMap::new();
        for (workspace, documents) in self.app_files_kept().await {
            if !held.contains(&workspace) {
                continue;
            }
            let answer = self.import_once(&workspace, documents).await;
            match &answer {
                Ok(moved) => {
                    tracing::info!(%workspace, %moved, "what native apps kept in files on this device is in the workspace");
                }
                Err(error) => {
                    tracing::info!(%workspace, %error, "what native apps kept in files on this device is not in the workspace yet");
                }
            }
            answered.insert(workspace, answer);
        }
        answered
    }

    /// [`Self::import_held_app_files`] on a task of its own, when this device's apps kept files.
    pub(crate) fn import_app_files_soon(&self) {
        if self.inner.host.app_files.homes.is_empty() {
            return;
        }
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let workspaces = self.downgrade();
            runtime.spawn(async move {
                let Some(workspaces) = workspaces.upgrade() else {
                    return;
                };
                workspaces.import_held_app_files().await;
            });
        }
    }

    /// Everything below the homes this device's service was opened with, by workspace.
    async fn app_files_kept(&self) -> BTreeMap<String, Vec<Kept>> {
        let homes = self.inner.host.app_files.homes.clone();
        if homes.is_empty() {
            return BTreeMap::new();
        }
        let person = self.inner.host.cert.user_public_key;
        tokio::task::spawn_blocking(move || kept_below(&homes, &person))
            .await
            .unwrap_or_default()
    }

    /// Take the turn that moves this device's `documents` into `workspace`, and move them: what
    /// it moved, kept as the turn's answer once every document went in, or the answer the turn
    /// was kept with already.
    async fn import_once(
        &self,
        workspace: &str,
        documents: Vec<Kept>,
    ) -> Result<serde_json::Value, String> {
        use crate::work::NotTaken;
        let device = self.lease_device(workspace).await;
        let turn = import_turn(&device);
        let claim = match Box::pin(crate::work::takes_turn(self, workspace, &turn)).await {
            Ok(claim) => claim,
            Err(NotTaken::Done(answer)) => return Ok(answer),
            Err(not_taken) => return Err(not_taken.to_string()),
        };
        let mut moved = Moved {
            device: hex::encode(device),
            apps: BTreeMap::new(),
        };
        let mut left = Vec::new();
        for document in documents {
            let app = document.app.clone();
            match Box::pin(self.import_document(workspace, document)).await {
                Ok(true) => {
                    let count = moved.apps.entry(app).or_default();
                    *count = count.saturating_add(1);
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(%workspace, %app, %error, "a document a native app kept in a file is not in the workspace yet");
                    left.push(format!("{app}: {error}"));
                }
            }
        }
        if !left.is_empty() {
            claim.release().await;
            return Err(format!(
                "{} documents native apps kept in files on this device are not in the \
                 workspace yet, and the next start moves them: {}",
                left.len(),
                left.join("; ")
            ));
        }
        let answer = serde_json::to_value(&moved).map_err(|error| error.to_string())?;
        let kept = Box::pin(crate::work::keep_done(
            self,
            workspace,
            &turn,
            answer.clone(),
        ))
        .await;
        claim.release().await;
        kept.map(|()| answer)
    }

    /// Answer `resource`'s [`IMPORT`], asked on this device's action service in `workspace`: the
    /// import of this device's own files, never another device's, which only that device holds.
    ///
    /// # Errors
    ///
    /// `resource` is another device's, or as [`Self::import_app_files`].
    pub(crate) async fn import_asked(
        &self,
        workspace: &str,
        resource: &str,
    ) -> Result<serde_json::Value, String> {
        let device = self.lease_device(workspace).await;
        if resource != device_resource(&device) {
            return Err(format!(
                "this device moves only its own apps' files, not those of {resource}"
            ));
        }
        self.import_app_files(workspace).await
    }

    /// Found `kept` in `workspace`'s store, holding what its file held, as the kind it names
    /// itself when its app declares that kind, else its app's own: whether it went in. A record
    /// the store holds already is newer than its file, and is left.
    async fn import_document(&self, workspace: &str, kept: Kept) -> Result<bool, String> {
        let Kept {
            app,
            id,
            kind,
            view,
            document,
        } = kept;
        let kinds = self.inner.host.surfaces.kinds_of(&app);
        let kind = kind
            .filter(|kind| kinds.contains(kind))
            .or_else(|| kinds.into_iter().next())
            .ok_or_else(|| format!("{app} declares no kind its records are kept as"))?;
        self.found_record(workspace, (&app, &kind, &id), (&document, view))
            .await
    }
}

/// Where this device's apps kept their files, and the one import that reads them at a time.
#[derive(Default)]
pub(crate) struct AppFiles {
    homes: Vec<PathBuf>,
    importing: tokio::sync::Mutex<()>,
}

impl AppFiles {
    pub(crate) fn below(homes: Vec<PathBuf>) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            homes,
            importing: tokio::sync::Mutex::new(()),
        })
    }
}

/// The import, as the provider every device's calls of it reach ([`PROVIDER`]): the workspace's
/// grants decide the call, and its answer is what the move, run once, moved.
pub(crate) struct Import(super::WeakWorkspaces);

impl Import {
    pub(crate) fn of(workspaces: &Workspaces) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self(workspaces.downgrade()))
    }
}

impl tilo_actions::remote::ActionProvider for Import {
    fn serves(&self, provider: &str) -> bool {
        provider == PROVIDER
    }

    /// The import on this device's resource in the workspace `caller` asks in: what the
    /// provider declares, and the kind its grants are read for.
    fn query(
        &self,
        caller: &tilo_actions::remote::Caller,
        query: tilo_actions::remote::CatalogueQuery,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionCallError>,
    > {
        use tilo_actions::remote::{CataloguePage, CatalogueRow, Pending};
        let (Some(workspaces), Some(workspace), Ok(runtime)) = (
            self.0.upgrade(),
            caller.workspace().map(str::to_owned),
            tokio::runtime::Handle::try_current(),
        ) else {
            return Pending::ready(Ok(CataloguePage::from_rows(
                "0".to_owned(),
                Vec::new(),
                &query,
            )));
        };
        let (done, answer) = tilo_actions::remote::pending();
        runtime.spawn(async move {
            let device = workspaces.lease_device(&workspace).await;
            let row = CatalogueRow::for_action(PROVIDER, PROVIDER, import_descriptor(&device));
            done.complete(Ok(CataloguePage::from_rows(
                "0".to_owned(),
                vec![row],
                &query,
            )));
        });
        answer
    }

    fn invoke(
        &self,
        caller: &tilo_actions::remote::Caller,
        request: tilo_actions::remote::InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
    > {
        use tilo_actions::remote::{ActionCallError, InvocationReceipt, Pending, ReceiptStatus};
        let (Some(workspaces), Some(workspace), Ok(runtime)) = (
            self.0.upgrade(),
            caller.workspace().map(str::to_owned),
            tokio::runtime::Handle::try_current(),
        ) else {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        };
        if request.target.address.action != IMPORT {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        }
        let (done, answer) = tilo_actions::remote::pending();
        runtime.spawn(async move {
            let resource = &request.target.address.resource;
            let receipt = match workspaces.import_asked(&workspace, resource).await {
                Ok(moved) => Ok(InvocationReceipt {
                    invocation: request.invocation,
                    status: ReceiptStatus::Completed {
                        output: tilo_actions::ActionOutput {
                            value: Some(moved),
                            resources: Vec::new(),
                        },
                    },
                }),
                Err(reason) => Err(ActionCallError::Refused { reason }),
            };
            done.complete(receipt);
        });
        answer
    }

    fn observe(
        &self,
        _: &tilo_actions::remote::Caller,
        _: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        None
    }

    fn cancel(
        &self,
        _: &tilo_actions::remote::Caller,
        _: &tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionCallError>> {
        tilo_actions::remote::Pending::ready(Err(
            tilo_actions::remote::ActionCallError::UnknownInvocation,
        ))
    }
}

#[cfg(test)]
#[path = "profile_import_tests.rs"]
mod tests;
