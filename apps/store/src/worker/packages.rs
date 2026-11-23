//! The apps a workspace runs and the packages this device has fetched for it, as the Store's
//! worker reads and changes them through the session's action API.
//!
//! A version is chosen on the workspace's tree (`workspace/apps#install`, and one app's
//! `#update`, `#pin`, `#unpin` and `#uninstall`), which every device of the workspace follows.
//! What a device has fetched reads as rows on its package cache
//! (`devices/<device>/packages`, provider `so.tilo.packages`): the list, and one app's row,
//! which the Store follows while an install runs so a card shows its progress. The cache also
//! reads the workspace's Tilo repositories for the Store (`catalogue`, `runtime-offer`) and
//! fetches a runtime one offers (`runtime-fetch`).

use std::collections::BTreeMap;

use crate::sources::WorkspaceApp;
use package_actions::PackageRow;
use ui::tilo_actions::ActionAddress;
use ui::tilo_actions::remote::{
    CatalogueQuery, InvokeArgument, MAX_CATALOGUE_PAGE, RemoteActionTarget, ResourceReadRequest,
    WORKSPACE_APPS, WORKSPACE_GOVERNANCE_PROVIDER, WatchScope, workspace_app,
    workspace_governance_child,
};
use ui::{Arrival, ChangedCall, FollowedRead, InvocationCall, RemoteActions};

/// The provider a device's package cache answers under.
const PACKAGES_PROVIDER: &str = "so.tilo.packages";

pub use crate::sources::Installed;

/// What a change to the tree asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// Install the newest version the app's source offers.
    Get,
    /// Move the app to the newest version its source offers.
    Update,
    Uninstall,
    Pin,
    Unpin,
}

impl Change {
    /// The change as it shows in the window's status while it runs.
    #[must_use]
    pub const fn progress(self) -> crate::ActionKind {
        match self {
            Self::Get => crate::ActionKind::Get,
            Self::Update => crate::ActionKind::Update,
            Self::Uninstall => crate::ActionKind::Remove,
            Self::Pin => crate::ActionKind::MarkInstalled,
            Self::Unpin => crate::ActionKind::Unmark,
        }
    }
}

/// A change the worker started, and the call that says how it ended.
pub struct Started {
    pub change: Change,
    pub app: String,
    pub call: InvocationCall,
}

/// The workspace's apps and this device's packages, over the session's action API.
pub struct AppPackages {
    actions: RemoteActions,
    workspace: Option<String>,
    installed: Installed,
    /// The resource this device's package cache reads as, once its catalogue named it.
    device: Option<String>,
    /// The rows this window follows, while the device's resource is known.
    rows: Option<FollowedRead>,
    /// The catalogue read that names this device's package cache.
    device_catalogue: Option<ui::CatalogueCall>,
    /// The catalogue pages listing the apps the workspace runs.
    tree_catalogue: Option<ui::CatalogueCall>,
    tree_rows: BTreeMap<String, WorkspaceApp>,
    tree_cursor: Vec<u8>,
    tree_revision: Option<String>,
    /// Waiting for the tree to change, so its apps are read again.
    tree_changed: Option<ChangedCall>,
    /// Why the device's packages could not be read, said once.
    error: Option<String>,
    /// This computer answered no package cache, so it is not asked again until a refresh.
    device_given_up: bool,
    /// The tree could not be read, so it is not asked again until a refresh.
    tree_given_up: bool,
}

/// What one pass over the session's answers found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Advanced {
    /// The installed view changed: the workspace's apps or this device's rows did.
    pub changed: bool,
    /// Why a read failed, for the window to say once.
    pub error: Option<&'static str>,
}

impl AppPackages {
    /// The apps of `workspace`'s tree and this device's packages, over `actions`.
    #[must_use]
    pub fn new(actions: RemoteActions, workspace: Option<String>) -> Self {
        Self {
            actions,
            workspace,
            installed: Installed::default(),
            device: None,
            rows: None,
            device_catalogue: None,
            tree_catalogue: None,
            tree_rows: BTreeMap::new(),
            tree_cursor: Vec::new(),
            tree_revision: None,
            tree_changed: None,
            error: None,
            device_given_up: false,
            tree_given_up: false,
        }
    }

    /// What the Store knows of the workspace's apps and this device's packages.
    #[must_use]
    pub const fn installed(&self) -> &Installed {
        &self.installed
    }

    /// The failed read the window has not been told yet, taken.
    fn take_error(&mut self) -> Option<&'static str> {
        self.error.take()
    }

    /// Whether an answer is still expected soon: the worker looks again promptly.
    #[must_use]
    pub const fn reading(&self) -> bool {
        self.device_catalogue.is_some() || self.tree_catalogue.is_some()
    }

    /// Whether a change of the tree is waited for: the worker looks occasionally so a provider
    /// that cannot wake this thread is still read.
    #[must_use]
    pub const fn waiting(&self) -> bool {
        self.tree_changed.is_some()
    }

    /// Take in what the session answered since the last pass.
    #[must_use]
    pub fn advance(&mut self) -> Advanced {
        let mut advanced = Advanced::default();
        advanced.changed |= self.advance_device_catalogue();
        advanced.changed |= self.advance_rows();
        advanced.changed |= self.advance_tree();
        advanced.error = self.take_error();
        advanced
    }

    /// Ask the catalogue which resource this device's package cache reads as.
    fn advance_device_catalogue(&mut self) -> bool {
        if self.device.is_some() || self.device_given_up {
            self.device_catalogue = None;
            return false;
        }
        if self.device_catalogue.is_none() {
            self.device_catalogue = Some(self.actions.catalogue(CatalogueQuery {
                provider: Some(PACKAGES_PROVIDER.to_owned()),
                max_results: 1,
                ..CatalogueQuery::default()
            }));
            return false;
        }
        match self.device_catalogue.as_ref().map(ui::Arriving::get) {
            Some(Arrival::Ready(Ok(page))) => {
                self.device_catalogue = None;
                let Some(device) = page
                    .rows
                    .iter()
                    .map(|row| row.resource.id.as_str())
                    .find(|resource| {
                        resource.starts_with("devices/") && resource.ends_with("/packages")
                    })
                    .map(str::to_owned)
                else {
                    // This computer keeps no packages (no package cache answers).
                    self.error = Some("the_packages");
                    self.device_given_up = true;
                    return false;
                };
                self.rows = Some(self.actions.follow_resource(
                    None,
                    ResourceReadRequest {
                        app: PACKAGES_PROVIDER.to_owned(),
                        record: device.clone(),
                        joined: false,
                        after: None,
                        wait_ms: None,
                    },
                ));
                self.device = Some(device);
                false
            }
            Some(Arrival::Ready(Err(_)) | Arrival::Failed(_)) => {
                self.device_catalogue = None;
                self.error = Some("the_packages");
                self.device_given_up = true;
                false
            }
            Some(Arrival::Pending { .. }) | None => false,
        }
    }

    /// Take this device's rows as they now stand.
    fn advance_rows(&mut self) -> bool {
        let Some(rows) = self.rows.as_ref() else {
            return false;
        };
        let Some(device) = self.device.as_deref() else {
            return false;
        };
        let reading = rows.call().get();
        let next = match reading {
            Arrival::Ready(value) => match rows_of(&value, device) {
                Ok(rows) => rows,
                Err(why) => {
                    self.rows = None;
                    self.error = Some(why);
                    return false;
                }
            },
            Arrival::Failed(_) => {
                // A reader that may no longer read keeps what it last showed.
                self.rows = None;
                return false;
            }
            Arrival::Pending { .. } => return false,
        };
        if next == self.installed.device {
            return false;
        }
        self.installed.device = next;
        true
    }

    /// Read the apps the workspace runs, and wait for its tree to change.
    fn advance_tree(&mut self) -> bool {
        let Some(workspace) = self.workspace.clone() else {
            return false;
        };
        if self.tree_given_up {
            return false;
        }
        let mut changed = false;
        match self.tree_changed.as_ref().map(ui::Arriving::get) {
            Some(Arrival::Ready(())) => self.tree_changed = None,
            Some(Arrival::Failed(_)) => {
                // A provider that cannot say when the tree changes is not
                // read again until a refresh.
                self.tree_changed = None;
                self.tree_catalogue = None;
                self.tree_given_up = true;
            }
            Some(Arrival::Pending { .. }) | None => {}
        }
        if self.tree_catalogue.is_none() && self.tree_changed.is_none() {
            if self.tree_cursor.is_empty() {
                self.tree_rows.clear();
            }
            self.tree_catalogue = Some(self.actions.catalogue(CatalogueQuery {
                provider: Some(WORKSPACE_GOVERNANCE_PROVIDER.to_owned()),
                parent_resource: Some(workspace_governance_child(&workspace, Some(WORKSPACE_APPS))),
                max_results: MAX_CATALOGUE_PAGE,
                cursor: std::mem::take(&mut self.tree_cursor),
                revision: self.tree_revision.take(),
                ..CatalogueQuery::default()
            }));
        }
        let Some(call) = self.tree_catalogue.as_ref() else {
            return changed;
        };
        match call.get() {
            Arrival::Ready(Ok(page)) => {
                for row in &page.rows {
                    if let Some(app) = workspace_app_named_of(&workspace, &row.resource.id) {
                        let entry = self
                            .tree_rows
                            .entry(app.to_owned())
                            .or_insert(WorkspaceApp { pinned: false });
                        if row
                            .action
                            .as_ref()
                            .is_some_and(|action| action.address.action == "unpin")
                        {
                            entry.pinned = true;
                        }
                    }
                }
                if page.exhausted || page.next_cursor.is_empty() {
                    self.tree_catalogue = None;
                    if self.tree_rows != self.installed.workspace {
                        self.installed.workspace.clone_from(&self.tree_rows);
                        changed = true;
                    }
                    self.tree_changed = Some(self.actions.changed(WatchScope::Records));
                } else {
                    self.tree_cursor = page.next_cursor;
                    self.tree_revision = Some(page.revision);
                    self.tree_catalogue = None;
                }
            }
            Arrival::Ready(Err(_)) | Arrival::Failed(_) => {
                self.tree_catalogue = None;
                self.tree_given_up = true;
            }
            Arrival::Pending { .. } => {}
        }
        changed
    }

    /// Start `change` of `app`, whose newest version is `release`.
    ///
    /// # Errors
    /// This window is in no workspace, or the app has no version to install.
    #[must_use]
    pub fn start(
        &self,
        change: Change,
        app: &str,
        release: Option<serde_json::Value>,
    ) -> Result<Started, String> {
        let workspace = self.workspace.as_deref().ok_or_else(|| {
            "This window is in no workspace, so it cannot add an app to one.".to_owned()
        })?;
        let target = match change {
            Change::Get => RemoteActionTarget::workspace_governance(workspace, "install-app"),
            Change::Update => app_action(workspace, app, "update"),
            Change::Uninstall => app_action(workspace, app, "uninstall"),
            Change::Pin => app_action(workspace, app, "pin"),
            Change::Unpin => app_action(workspace, app, "unpin"),
        };
        let argument = match change {
            Change::Get | Change::Update => {
                let release = release.ok_or_else(|| {
                    "This app has no version its source offers to install.".to_owned()
                })?;
                let serde_json::Value::Object(release) = release else {
                    return Err("the app's release is no object".to_owned());
                };
                InvokeArgument::Json(release)
            }
            Change::Uninstall | Change::Pin | Change::Unpin => {
                InvokeArgument::Json(serde_json::Map::new())
            }
        };
        let call = self.actions.invoke(target, argument, None);
        Ok(Started {
            change,
            app: app.to_owned(),
            call,
        })
    }

    /// Ask again what earlier reads could not answer: on a refresh.
    pub fn retry(&mut self) {
        self.tree_rows.clear();
        self.tree_cursor.clear();
        self.tree_revision = None;
        self.device_given_up = false;
        self.tree_given_up = false;
        self.rows = None;
        self.device = None;
        self.tree_catalogue = None;
        self.tree_changed = None;
    }

    /// Take `installed` as what the Store knows, as a provider that answered before the window
    /// opened has said it.
    #[cfg(test)]
    pub fn set_installed(&mut self, installed: Installed) {
        self.installed = installed;
    }

    /// Ask after the changes this window started, as a provider that does not wait needs.
    pub fn poll(&self) {
        self.actions.poll();
    }

    /// Wait for `call` to end, asking after it while a provider that does not wait still runs it.
    ///
    /// # Errors
    /// The change was refused, cancelled, or did not end in `timeout`.
    pub fn await_call(
        &self,
        call: &InvocationCall,
        timeout: std::time::Duration,
    ) -> Result<(), String> {
        self.await_value(call, timeout).map(drop)
    }

    /// Wait for `call` to end, as [`Self::await_call`] does, and answer what it answered.
    ///
    /// # Errors
    /// The call was refused, cancelled, or did not end in `timeout`.
    pub fn await_value(
        &self,
        call: &InvocationCall,
        timeout: std::time::Duration,
    ) -> Result<serde_json::Value, String> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(ended) = answered(call) {
                return ended;
            }
            if std::time::Instant::now() >= deadline {
                return Err("The workspace did not answer the change in time.".to_owned());
            }
            self.actions.poll();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Ask this device's package cache for its action or reading `action` with `arguments`: the
    /// call that answers it, `Ok(None)` while the cache's resource is not known yet.
    ///
    /// # Errors
    /// This computer answers no package cache (`"the_packages"`).
    pub fn cache_call(
        &self,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<InvocationCall>, &'static str> {
        if self.device_given_up {
            return Err("the_packages");
        }
        let Some(device) = self.device.as_deref() else {
            return Ok(None);
        };
        let target = RemoteActionTarget {
            provider: PACKAGES_PROVIDER.to_owned(),
            address: ActionAddress::new(device, action),
        };
        Ok(Some(self.actions.invoke(
            target,
            InvokeArgument::Json(arguments),
            None,
        )))
    }

    /// Ask this device's package cache for `action` with `arguments`, as [`Self::cache_call`]
    /// does, and wait for its answer, finding the cache first when no read has named it yet.
    ///
    /// # Errors
    /// This computer answers no package cache, or the call failed or did not end in `timeout`.
    pub fn cache_call_now(
        &mut self,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
        timeout: std::time::Duration,
    ) -> Result<serde_json::Value, String> {
        let deadline = std::time::Instant::now() + timeout;
        let unavailable = || crate::resource::CATALOG.t("store-packages-unavailable");
        loop {
            match self.cache_call(action, arguments.clone()) {
                Ok(Some(call)) => {
                    let left = deadline.saturating_duration_since(std::time::Instant::now());
                    return self.await_value(&call, left);
                }
                Ok(None) => {}
                Err(_) => return Err(unavailable()),
            }
            if std::time::Instant::now() >= deadline {
                return Err(unavailable());
            }
            let _ = self.advance_device_catalogue();
            self.actions.poll();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Hand the workspace the package file `bytes` in the pieces it takes, waiting for each:
    /// their BLAKE3, which an install names as the version's file.
    ///
    /// # Errors
    /// This window is in no workspace, or the workspace refused a piece.
    pub fn upload(&self, bytes: &[u8], timeout: std::time::Duration) -> Result<String, String> {
        let workspace = self.workspace.as_deref().ok_or_else(|| {
            "This window is in no workspace, so it cannot add an app to one.".to_owned()
        })?;
        let hash = blake3::hash(bytes).to_hex().to_string();
        let total = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let mut offset = 0_u64;
        for piece in bytes.chunks(UPLOAD_PIECE) {
            let mut arguments = serde_json::Map::new();
            arguments.insert("file".into(), serde_json::Value::String(hash.clone()));
            arguments.insert("offset".into(), offset.into());
            arguments.insert("total".into(), total.into());
            arguments.insert("data".into(), hex::encode(piece).into());
            let call = self.actions.invoke(
                RemoteActionTarget::workspace_governance(workspace, "upload-app-file"),
                InvokeArgument::Json(arguments),
                None,
            );
            self.await_call(&call, timeout)?;
            offset = offset.saturating_add(u64::try_from(piece.len()).unwrap_or(u64::MAX));
        }
        Ok(hash)
    }
}

/// The bytes of a package file one upload piece carries: the most the workspace takes at once.
const UPLOAD_PIECE: usize = 256 * 1024;

/// One app's action on the workspace's tree.
fn app_action(workspace: &str, app: &str, action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: WORKSPACE_GOVERNANCE_PROVIDER.to_owned(),
        address: ActionAddress::new(workspace_app(workspace, app), action),
    }
}

/// The app `resource` names among `workspace`'s apps, when it names one.
fn workspace_app_named_of<'a>(workspace: &str, resource: &'a str) -> Option<&'a str> {
    let apps = workspace_governance_child(workspace, Some(WORKSPACE_APPS));
    let app = resource.strip_prefix(apps.as_str())?.strip_prefix('/')?;
    (!app.is_empty()
        && app
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')))
    .then_some(app)
}

/// The rows a read of this device's packages answered, or why it could not be read.
fn rows_of(
    value: &serde_json::Value,
    device: &str,
) -> Result<BTreeMap<String, PackageRow>, &'static str> {
    let records = value
        .get("records")
        .and_then(serde_json::Value::as_array)
        .ok_or("the_package_rows")?;
    let record = records
        .iter()
        .find(|record| record.get("record").and_then(serde_json::Value::as_str) == Some(device));
    let Some(record) = record else {
        return Ok(BTreeMap::new());
    };
    if record.get("absent").and_then(serde_json::Value::as_bool) == Some(true) {
        return Ok(BTreeMap::new());
    }
    let Some(rows) = record.get("value") else {
        return Ok(BTreeMap::new());
    };
    let rows: Vec<PackageRow> =
        serde_json::from_value(rows.clone()).map_err(|_| "the_package_rows")?;
    Ok(rows.into_iter().map(|row| (row.app.clone(), row)).collect())
}

/// How a change ended: its refusal or cancellation, or `Ok(())`; `None` while it runs.
#[must_use]
pub fn settled(call: &InvocationCall) -> Option<Result<(), String>> {
    answered(call).map(|ended| ended.map(drop))
}

/// How a call ended: what it answered (`null` for nothing), or its refusal or cancellation;
/// `None` while it runs.
#[must_use]
pub fn answered(call: &InvocationCall) -> Option<Result<serde_json::Value, String>> {
    match call.get() {
        Arrival::Ready(ui::tilo_actions::InvocationUpdate::Succeeded { output, .. }) => {
            Some(Ok(output.value.unwrap_or(serde_json::Value::Null)))
        }
        Arrival::Ready(ui::tilo_actions::InvocationUpdate::Failed { error, .. }) => {
            Some(Err(error.message))
        }
        Arrival::Ready(ui::tilo_actions::InvocationUpdate::Cancelled { message, .. }) => Some(Err(
            message.unwrap_or_else(|| "The change was cancelled.".to_owned()),
        )),
        Arrival::Ready(
            ui::tilo_actions::InvocationUpdate::Accepted { .. }
            | ui::tilo_actions::InvocationUpdate::Progress { .. }
            | ui::tilo_actions::InvocationUpdate::Item { .. },
        )
        | Arrival::Pending { .. } => None,
        Arrival::Failed(error) => Some(Err(error.to_string())),
    }
}
