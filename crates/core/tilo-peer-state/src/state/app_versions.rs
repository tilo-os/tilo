//! The version of each app a workspace runs.
//!
//! A member the workspace lets install apps (`install-app`) names a release of an app: its
//! version, the surface its changes name, the package every member fetches and verifies, and
//! who signed it. The workspace keeps it as one of its resources, a record of the system's
//! `workspace.apps` kind, so installing is a change like any other: every member reads which
//! version runs and who installed it, and undoing the install takes it back. The device that
//! holds the resource hands this copy what it reads ([`StatePeer::read_apps`]); admission reads
//! the apps from that alone ([`WorkspaceApps`]).
//!
//! A change made with another version (its op names another surface) is set aside, not merged
//! and not refused: once the workspace runs a version that names that surface, the change is
//! decided against it.
//!
//! Workspaces made before the resource kept their apps in the `app_versions_v1` map of their
//! governance. A copy that read no apps yet decides by it ([`StatePeer::apps`]), and bringing
//! the workspace over writes what it names into the resource; it is never written again.

use std::collections::BTreeMap;
use std::sync::Arc;

use automerge::{AutoCommit, ROOT, ReadDoc};
use collab_types::signer::DeviceSigner;
use tilo_action_policy::Change;
use tilo_action_policy::roles::{self, governance};
use tilo_user_identity::DeviceCert;

use super::facades::PeerGovernance;
use super::{AdmissionReport, StatePeer, SystemTime, person_of};
use crate::workspace_membership::{AppRelease, WorkspaceMembershipCommand};

/// The governance map workspaces made before the apps resource kept their app versions in, by
/// app id.
pub(super) const APP_VERSIONS: &str = "app_versions_v1";

/// An app version the workspace runs.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InstalledApp {
    pub release: AppRelease,
    /// The person who installed it, hex.
    pub installed_by: String,
    /// When, ns since the epoch.
    pub installed_at_ns: u64,
}

/// The apps a workspace runs, and every release it ran each with, as the copy holding it last
/// read them from the workspace's apps resource. Admission reads the apps from this alone: the
/// running version decides which surface a change of an app's records is decided under, and
/// being installed is what lets the app keep its own records ([`with_installed_apps`]).
///
/// A copy reads it after the resource changed, so it can trail the resource for a moment. That
/// is safe because a change naming a surface the workspace has not run is only ever set aside
/// ([`super::authority::SurfaceGate`]): reading the apps again decides it once more.
/// A host sends it, its heads beside it, to a device that keeps no store.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceApps {
    running: BTreeMap<String, InstalledApp>,
    ran: BTreeMap<String, Vec<AppRelease>>,
    /// The heads of the apps resource they were read at.
    #[serde(skip)]
    heads: Vec<[u8; 32]>,
}

impl WorkspaceApps {
    /// The apps `running`, by app id, and each release `ran` names for each app, oldest first.
    /// A running release is one the workspace ran.
    #[must_use]
    pub fn new(
        running: BTreeMap<String, InstalledApp>,
        mut ran: BTreeMap<String, Vec<AppRelease>>,
    ) -> Self {
        for (app, installed) in &running {
            let releases = ran.entry(app.clone()).or_default();
            if !releases.contains(&installed.release) {
                releases.push(installed.release.clone());
            }
        }
        Self {
            running,
            ran,
            heads: Vec::new(),
        }
    }

    /// These apps, read from the apps resource at `heads`.
    #[must_use]
    pub fn at(mut self, heads: Vec<[u8; 32]>) -> Self {
        self.heads = heads;
        self
    }

    /// The heads of the apps resource these apps were read at.
    #[must_use]
    pub fn heads(&self) -> &[[u8; 32]] {
        &self.heads
    }

    /// These apps as a host sends them over its link
    /// ([`neighbour_wire::NeighbourMsg::WorkspaceApps`]).
    #[must_use]
    pub fn message(&self, workspace: &str) -> neighbour_wire::NeighbourMsg {
        neighbour_wire::NeighbourMsg::WorkspaceApps {
            cluster_id: workspace.to_owned(),
            heads: self.heads.clone(),
            // Every field is the workspace's own and encodes; a release that does not is none
            // the resource holds.
            apps: serde_json::to_vec(self).unwrap_or_default(),
        }
    }

    /// The apps `message` carries, when it is a host's apps for `workspace` that read whole.
    #[must_use]
    pub fn sent(message: &neighbour_wire::NeighbourMsg, workspace: &str) -> Option<Self> {
        let neighbour_wire::NeighbourMsg::WorkspaceApps {
            cluster_id,
            heads,
            apps,
        } = message
        else {
            return None;
        };
        let sent: Self = tilo_failure::ignored!(
            "apps that do not decode are none a host of this build sent",
            serde_json::from_slice(apps).ok()
        )
        .filter(|_| cluster_id == workspace)?;
        Some(Self::new(sent.running, sent.ran).at(heads.clone()))
    }

    /// Each app version the workspace runs, by app id.
    #[must_use]
    pub fn running(&self) -> &BTreeMap<String, InstalledApp> {
        &self.running
    }

    /// Each release the workspace ran `app` with, oldest first.
    #[must_use]
    pub fn ran(&self, app: &str) -> &[AppRelease] {
        self.ran.get(app).map_or(&[], Vec::as_slice)
    }

    /// These apps once `app` runs `installed`, or once it is uninstalled with `None`: what
    /// installing or uninstalling it leaves.
    #[must_use]
    pub fn installing(&self, app: &str, installed: Option<InstalledApp>) -> Self {
        let mut running = self.running.clone();
        let mut ran = self.ran.clone();
        match installed {
            Some(installed) => {
                // The release it runs now is the one it ran last.
                let releases = ran.entry(app.to_owned()).or_default();
                releases.retain(|earlier| *earlier != installed.release);
                releases.push(installed.release.clone());
                running.insert(app.to_owned(), installed);
            }
            None => drop(running.remove(app)),
        }
        Self::new(running, ran).at(self.heads.clone())
    }
}

impl StatePeer {
    /// Refuse an app version command: the workspace's apps are its apps resource, which
    /// installing and uninstalling write. `None` for every other command.
    ///
    /// Kept while the oldest supported build is 61782b0f16a, which sends them.
    pub(super) fn apply_app_command(
        command: &WorkspaceMembershipCommand,
    ) -> Option<Result<bool, String>> {
        matches!(
            command,
            WorkspaceMembershipCommand::InstallApp(_)
                | WorkspaceMembershipCommand::UninstallApp { .. }
        )
        .then(|| Err("a workspace's apps are installed through its apps resource".to_owned()))
    }

    /// Read which apps the workspace runs as `apps`, from now on, and decide again every change
    /// set aside for the surface it names. `None` when the copy read these apps already.
    pub fn read_apps(&mut self, apps: WorkspaceApps, now: SystemTime) -> Option<AdmissionReport> {
        let doc = self.core.workspace_doc.as_mut()?;
        if doc.apps_read().is_some_and(|held| **held == apps) {
            return None;
        }
        doc.with_apps(Some(&Arc::new(apps)));
        Some(self.redecide_set_aside(now))
    }

    /// The apps the workspace runs, as this copy last read them.
    #[must_use]
    pub fn apps(&self) -> Arc<WorkspaceApps> {
        self.core
            .workspace_doc
            .as_ref()
            .map(crate::cached_document::CachedDocument::apps)
            .unwrap_or_default()
    }

    /// Whether the workspace ran `app` at all, as this copy reads its apps.
    pub(super) fn ran_app(&self, app: &str) -> bool {
        !self.apps().ran(app).is_empty()
    }

    /// Write the grant of `app`'s own records, as `caller`, a member the workspace lets install
    /// apps, installing it: unless the workspace's operations keep it already. Being installed is
    /// what gives the app its own records ([`with_installed_apps`]); the grant is written all the
    /// same because the oldest build this one still supports reads it to admit the app's own
    /// writes. Whether it wrote one.
    ///
    /// Kept while the oldest supported build is 61782b0f16a, which reads it.
    ///
    /// # Errors
    /// The caller may not install apps, or the workspace refused the change.
    pub fn grant_app_records(
        &mut self,
        caller: &DeviceCert,
        app: &str,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let member = self.check_app_governance(caller, now)?;
        if self.stored_app_grant(app) {
            return Ok(false);
        }
        let grant = Change::Grant(roles::app_records_grant(&self.core.workspace, app, &member));
        self.write_app_grant(grant, key, now)
    }

    /// Revoke the grant of `app`'s own records, as `caller`, a member the workspace lets install
    /// apps, uninstalling it: where the workspace's operations keep one. Uninstalling is what
    /// takes the app's own records away; the grant goes too because the oldest build this one
    /// still supports reads it. Whether it revoked one.
    ///
    /// Kept while the oldest supported build is 61782b0f16a, which reads it.
    ///
    /// # Errors
    /// The caller may not install apps, or the workspace refused the change.
    pub fn revoke_app_records(
        &mut self,
        caller: &DeviceCert,
        app: &str,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.check_app_governance(caller, now)?;
        if !self.stored_app_grant(app) {
            return Ok(false);
        }
        self.write_app_grant(Change::Revoke(roles::app_records_grant_id(app)), key, now)
    }

    /// Whether the workspace's operations keep the grant of `app`'s own records: as they keep it,
    /// never as admission reads it.
    fn stored_app_grant(&self, app: &str) -> bool {
        let id = roles::app_records_grant_id(app);
        self.core.workspace_doc.as_ref().is_some_and(|doc| {
            crate::members::stored(doc, &self.core.workspace)
                .grants
                .iter()
                .any(|grant| grant.id == id)
        })
    }

    /// Record `change` of an app's own records grant as this device's operation.
    fn write_app_grant(
        &mut self,
        change: Change,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let mut doc = self
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        let before = doc.get_heads();
        self.write_membership(
            doc,
            &before,
            key,
            now,
            Some(crate::members::MembershipOpCommand::Project {
                changes: vec![change],
            }),
        )
    }

    /// The person behind `caller`, when the workspace lets them install apps.
    fn check_app_governance(&self, caller: &DeviceCert, now: SystemTime) -> Result<String, String> {
        let member = self.membership(caller.device_public_key, now)?;
        member
            .policy
            .check_action(
                &member.authority(),
                governance::INSTALL_APP,
                &tilo_action_policy::Context::at_time(now),
                false,
            )
            .map_err(|refusal| refusal.to_string())?;
        Ok(person_of(&member)?.to_owned())
    }

    /// The surface the version of `app` the workspace runs names, when it runs one: what a change
    /// of the app's documents names, so it is merged rather than set aside for a version the
    /// workspace does not run.
    #[must_use]
    pub fn running_surface(&self, app: &str) -> Option<[u8; 32]> {
        self.governance().app_version(app)?.release.surface_hash()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl StatePeer {
    /// Write `row` as the version of its app in the workspace's app versions map, signed by
    /// `key`, as every build before the apps resource wrote one: what a workspace such a build
    /// made holds.
    ///
    /// # Errors
    /// The workspace has no governance yet, or refused the change.
    pub fn write_versions_map_as_before(
        &mut self,
        row: &InstalledApp,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        use automerge::transaction::Transactable as _;
        let mut doc = self
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        let before = doc.get_heads();
        doc.set_actor(self.workspace_actor_of(key));
        let versions = super::workspace_map(&doc, APP_VERSIONS)?;
        let bytes = serde_json::to_vec(row).map_err(|error| error.to_string())?;
        doc.put(
            &versions,
            row.release.app.as_str(),
            automerge::ScalarValue::Bytes(bytes),
        )
        .map_err(|error| error.to_string())?;
        self.write_governance(doc, &before, key, now)
    }
}

/// The apps the app versions map of `doc` names, and every release it ran each with: what a
/// copy decides by before it read the apps resource, as every build before the resource did, and
/// what bringing a workspace over to the resource writes into it.
///
/// Kept while the oldest supported build is 61782b0f16a, which reads it.
pub(crate) fn apps_in_map(doc: &AutoCommit) -> WorkspaceApps {
    let Ok(Some((_, versions))) = doc.get(ROOT, APP_VERSIONS) else {
        return WorkspaceApps::default();
    };
    let heads = doc.committed_document().get_heads();
    let running: BTreeMap<String, InstalledApp> = doc
        .keys(&versions)
        .filter_map(|app| Some((app.clone(), read_version_at(doc, &versions, &app, &heads)?)))
        .collect();
    let mut ran: BTreeMap<String, Vec<AppRelease>> = BTreeMap::new();
    for change in doc.committed_document().get_changes(&[]) {
        for app in doc.keys_at(&versions, &[change.hash()]) {
            let Some(row) = read_version_at(doc, &versions, &app, &[change.hash()]) else {
                continue;
            };
            let releases = ran.entry(app).or_default();
            releases.retain(|earlier| *earlier != row.release);
            releases.push(row.release);
        }
    }
    WorkspaceApps::new(running, ran)
}

/// `policy` with the right each app `apps` runs has to its own records: installing an app gives
/// it ([`roles::app_records_grant`], by the member who installed it) and uninstalling it takes it
/// away. A grant of that id a policy keeps itself is no part of it: the right is whether the app
/// is installed, read in one place.
pub(crate) fn with_installed_apps(
    mut policy: tilo_action_policy::WorkspacePolicy,
    apps: &WorkspaceApps,
) -> tilo_action_policy::WorkspacePolicy {
    policy
        .grants
        .retain(|grant| !roles::is_app_records_grant(grant));
    let workspace = policy.workspace.clone();
    policy.grants.extend(
        apps.running
            .iter()
            .map(|(app, row)| roles::app_records_grant(&workspace, app, &row.installed_by)),
    );
    policy
}

impl PeerGovernance<'_> {
    /// Every app version the workspace runs, by app id.
    #[must_use]
    pub fn app_versions(self) -> Vec<InstalledApp> {
        self.peer.apps().running.values().cloned().collect()
    }

    /// The version of `app` the workspace runs, if it chose one.
    #[must_use]
    pub fn app_version(self, app: &str) -> Option<InstalledApp> {
        self.peer.apps().running.get(app).cloned()
    }

    /// The releases of `app` the workspace ran before the one it runs now, the most recent
    /// first, each once. What going back a version goes back to.
    #[must_use]
    pub fn earlier_app_versions(self, app: &str) -> Vec<AppRelease> {
        // Keeping a version or letting it go is no other version.
        let unpinned = |mut release: AppRelease| {
            release.pinned = false;
            release
        };
        let apps = self.peer.apps();
        let current = apps
            .running
            .get(app)
            .map(|row| unpinned(row.release.clone()));
        let mut earlier: Vec<AppRelease> = Vec::new();
        for release in apps.ran(app).iter().rev().cloned().map(unpinned) {
            if Some(&release) != current.as_ref() && !earlier.contains(&release) {
                earlier.push(release);
            }
        }
        earlier
    }
}

/// The row kept under `app` as the document stood at `heads`, if it read as a valid release of
/// that app then: a row naming another app than its key is not trusted.
fn read_version_at(
    doc: &impl ReadDoc,
    versions: &automerge::ObjId,
    app: &str,
    heads: &[automerge::ChangeHash],
) -> Option<InstalledApp> {
    let (automerge::Value::Scalar(value), _) = doc.get_at(versions, app, heads).ok()?? else {
        return None;
    };
    let automerge::ScalarValue::Bytes(bytes) = value.as_ref() else {
        return None;
    };
    tilo_failure::ignored!(
        "a row that did not decode then named no version the workspace ran",
        serde_json::from_slice::<InstalledApp>(bytes)
            .ok()
            .filter(|row| row.release.app == app && row.release.validate().is_ok())
    )
}
