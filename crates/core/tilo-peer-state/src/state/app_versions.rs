//! The version of each app a workspace runs.
//!
//! A member the workspace lets install apps (`install-app`) names a release of
//! an app: its version, the surface its changes name, the package every
//! member fetches and verifies, and who signed it. The workspace keeps it in its
//! governance, so every member reads which version runs and who installed
//! it, and every member is to run it. A change made with
//! another version (its op names another surface) is held, not merged and
//! not refused: once its author runs the workspace's version, the same changes
//! name that version's surface and are decided against it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use collab_types::signer::DeviceSigner;
use tilo_action_policy::Change;
use tilo_action_policy::roles::{self, governance};
use tilo_user_identity::DeviceCert;

use super::facades::PeerGovernance;
use super::{StatePeer, SystemTime, err, now_ns, person_of, workspace_map};
use crate::workspace_membership::{AppRelease, WorkspaceMembershipCommand};

/// The governance map that keeps the workspace's app versions, by app id.
pub(super) const APP_VERSIONS: &str = "app_versions_v1";

/// An app version the workspace runs, as its governance keeps it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InstalledApp {
    pub release: AppRelease,
    /// The person who installed it, hex.
    pub installed_by: String,
    /// When, ns since the epoch.
    pub installed_at_ns: u64,
}

impl StatePeer {
    /// Apply an app version command; `None` for every other command.
    pub(super) fn apply_app_command(
        &mut self,
        caller: &DeviceCert,
        command: &WorkspaceMembershipCommand,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Option<Result<bool, String>> {
        match command {
            WorkspaceMembershipCommand::InstallApp(release) => {
                Some(self.apply_install_app(caller, release, key, now))
            }
            WorkspaceMembershipCommand::UninstallApp { app } => {
                Some(self.apply_uninstall_app(caller, app, key, now))
            }
            _ => None,
        }
    }

    fn apply_install_app(
        &mut self,
        caller: &DeviceCert,
        release: &AppRelease,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let member = self.check_app_governance(caller, now)?;
        let installed = self.governance().app_version(&release.app);
        if installed
            .as_ref()
            .is_some_and(|installed| &installed.release == release)
        {
            return Ok(false);
        }
        // Being installed is what gives the app its own records ([`with_installed_apps`]). The
        // grant is written all the same, in the same change, because the oldest build this one
        // still supports reads it to admit the app's own writes. Moving an installed app to
        // another version gives nothing again.
        let granted = installed.is_none().then(|| {
            Change::Grant(roles::app_records_grant(
                &self.core.workspace,
                &release.app,
                &member,
            ))
        });
        let row = InstalledApp {
            release: release.clone(),
            installed_by: member,
            installed_at_ns: now_ns(now),
        };
        self.put_app_version(&release.app, Some(&row), granted, key, now)
    }

    fn apply_uninstall_app(
        &mut self,
        caller: &DeviceCert,
        app: &str,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.check_app_governance(caller, now)?;
        if self.governance().app_version(app).is_none() {
            return Ok(false);
        }
        // Removing the row takes the app's own records away with it. The grant installing wrote
        // is revoked in the same change, because the oldest build this one still supports reads
        // it: as the workspace's operations keep it, never as admission reads it.
        let id = roles::app_records_grant_id(app);
        let revoked = self
            .core
            .workspace_doc
            .as_ref()
            .is_some_and(|doc| {
                crate::members::stored(doc, &self.core.workspace)
                    .grants
                    .iter()
                    .any(|grant| grant.id == id)
            })
            .then_some(Change::Revoke(id));
        self.put_app_version(app, None, revoked, key, now)
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

    /// Write or remove the version of `app` in one commit, with `grant`, the grant installing
    /// writes or uninstalling revokes, recorded in the same change: every member admits it as
    /// they admit any.
    fn put_app_version(
        &mut self,
        app: &str,
        row: Option<&InstalledApp>,
        grant: Option<Change>,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let mut doc = self
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        let before = doc.get_heads();
        doc.set_actor(self.workspace_actor_of(key));
        let versions = workspace_map(&doc, APP_VERSIONS)?;
        match row {
            Some(row) => doc
                .put(
                    &versions,
                    app,
                    automerge::ScalarValue::Bytes(serde_json::to_vec(row).map_err(err)?),
                )
                .map_err(err)?,
            None => doc.delete(&versions, app).map_err(err)?,
        }
        match grant {
            Some(change) => self.write_membership(
                doc,
                &before,
                key,
                now,
                Some(crate::members::MembershipOpCommand::Project {
                    changes: vec![change],
                }),
            ),
            None => self.write_governance(doc, &before, key, now),
        }
    }

    /// The surface the version of `app` the workspace runs names, when it runs one: what a change
    /// of the app's documents names, so it is merged rather than set aside for a version the
    /// workspace does not run.
    #[must_use]
    pub fn running_surface(&self, app: &str) -> Option<[u8; 32]> {
        self.workspace_surface(app)
    }

    /// The surface the workspace runs `app` with, when it chose a version.
    pub(super) fn workspace_surface(&self, app: &str) -> Option<[u8; 32]> {
        self.governance().app_version(app)?.release.surface_hash()
    }

    /// Every surface the workspace ran `app` with, the one it runs now included, as the
    /// workspace this peer judges by stands: a change naming one of them is decided under the
    /// surface it names, and one naming any other surface waits.
    pub(super) fn ran_surfaces(&self, app: &str) -> Arc<BTreeSet<[u8; 32]>> {
        let Some(doc) = self.core.workspace_doc.as_ref() else {
            return Arc::default();
        };
        doc.ran_surfaces(app, || {
            workspace_apps()
                .ran(&self.core.workspace, doc, app)
                .iter()
                .filter_map(AppRelease::surface_hash)
                .collect()
        })
    }
}

/// Where admission reads which apps a workspace runs: the version of each, who installed it,
/// and every version it ran before. The running version decides which surface a change of the
/// app's records is decided under, and being installed is what lets the app keep its own records
/// ([`with_installed_apps`]). Nothing in admission reads the apps another way.
///
/// `governance` is the workspace as the decision reads it: the state a write was written under
/// when it names one, so every copy decides a write alike whatever order it reaches them in.
pub trait WorkspaceApps: Send + Sync {
    /// Each app version `workspace` runs, by app id.
    fn running(&self, workspace: &str, governance: &AutoCommit) -> BTreeMap<String, InstalledApp>;

    /// Each release `workspace` ran `app` with, the one it runs now included, each once, in the
    /// order it last ran them. A release once in it stays in it.
    fn ran(&self, workspace: &str, governance: &AutoCommit, app: &str) -> Vec<AppRelease>;
}

/// The apps as the workspace's `app_versions_v1` map keeps them.
pub struct AppVersionsMap;

impl WorkspaceApps for AppVersionsMap {
    fn running(&self, _: &str, governance: &AutoCommit) -> BTreeMap<String, InstalledApp> {
        let Ok(Some((_, versions))) = governance.get(ROOT, APP_VERSIONS) else {
            return BTreeMap::new();
        };
        governance
            .keys(&versions)
            .filter_map(|app| Some((app.clone(), read_version(governance, &versions, &app)?)))
            .collect()
    }

    fn ran(&self, _: &str, governance: &AutoCommit, app: &str) -> Vec<AppRelease> {
        let Ok(Some((_, versions))) = governance.get(ROOT, APP_VERSIONS) else {
            return Vec::new();
        };
        let mut ran: Vec<AppRelease> = Vec::new();
        for change in governance.committed_document().get_changes(&[]) {
            let Some(row) = read_version_at(governance, &versions, app, &[change.hash()]) else {
                continue;
            };
            ran.retain(|earlier| *earlier != row.release);
            ran.push(row.release);
        }
        ran
    }
}

/// The source every peer of this process reads its workspace's apps from.
fn source() -> &'static std::sync::RwLock<Arc<dyn WorkspaceApps>> {
    static SOURCE: std::sync::OnceLock<std::sync::RwLock<Arc<dyn WorkspaceApps>>> =
        std::sync::OnceLock::new();
    SOURCE.get_or_init(|| std::sync::RwLock::new(Arc::new(AppVersionsMap)))
}

/// Read which apps every workspace of this process runs from `apps`: what a host sets once,
/// before it opens or restores its workspaces.
pub fn read_apps_with(apps: Arc<dyn WorkspaceApps>) {
    *source()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = apps;
}

/// Where this process reads its workspaces' apps from ([`read_apps_with`]).
pub(crate) fn workspace_apps() -> Arc<dyn WorkspaceApps> {
    Arc::clone(
        &source()
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    )
}

/// `policy` as `governance` gives it, with the right each installed app has to its own records:
/// installing an app gives it ([`roles::app_records_grant`], by the member who installed it) and
/// uninstalling it takes it away, in the same change. A grant of that id a policy keeps itself
/// is no part of it: the right is whether the app is installed, read in one place.
pub(crate) fn with_installed_apps(
    mut policy: tilo_action_policy::WorkspacePolicy,
    governance: &AutoCommit,
) -> tilo_action_policy::WorkspacePolicy {
    policy
        .grants
        .retain(|grant| !roles::is_app_records_grant(grant));
    let installed = workspace_apps().running(&policy.workspace, governance);
    let workspace = policy.workspace.clone();
    policy.grants.extend(
        installed
            .into_iter()
            .map(|(app, row)| roles::app_records_grant(&workspace, &app, &row.installed_by)),
    );
    policy
}

impl PeerGovernance<'_> {
    /// Every app version the workspace runs, by app id.
    #[must_use]
    pub fn app_versions(self) -> Vec<InstalledApp> {
        self.running().values().cloned().collect()
    }

    /// The version of `app` the workspace runs, if it chose one.
    #[must_use]
    pub fn app_version(self, app: &str) -> Option<InstalledApp> {
        self.running().get(app).cloned()
    }

    /// The apps the workspace runs, as [`WorkspaceApps::running`] reads them.
    fn running(self) -> Arc<BTreeMap<String, InstalledApp>> {
        let Some(doc) = self.peer.core.workspace_doc.as_ref() else {
            return Arc::default();
        };
        doc.running_apps(|| workspace_apps().running(&self.peer.core.workspace, doc))
    }

    /// The releases of `app` the workspace ran before the one it runs now, the most recent
    /// first, each once. What going back a version goes back to.
    #[must_use]
    pub fn earlier_app_versions(self, app: &str) -> Vec<AppRelease> {
        let Some(doc) = self.peer.core.workspace_doc.as_ref() else {
            return Vec::new();
        };
        // Keeping a version or letting it go is no other version.
        let unpinned = |mut release: AppRelease| {
            release.pinned = false;
            release
        };
        let current = self.app_version(app).map(|row| unpinned(row.release));
        let mut ran: Vec<AppRelease> = Vec::new();
        for release in workspace_apps().ran(&self.peer.core.workspace, doc, app) {
            let release = unpinned(release);
            ran.retain(|earlier| *earlier != release);
            ran.push(release);
        }
        ran.retain(|release| Some(release) != current.as_ref());
        ran.reverse();
        ran
    }
}

/// The row kept under `app` as the document stood at `heads`, if it read as a valid release then.
fn read_version_at(
    doc: &impl ReadDoc,
    versions: &automerge::ObjId,
    app: &str,
    heads: &[automerge::ChangeHash],
) -> Option<InstalledApp> {
    match doc.get_at(versions, app, heads).ok()?? {
        (automerge::Value::Scalar(value), _) => match value.as_ref() {
            automerge::ScalarValue::Bytes(bytes) => tilo_failure::ignored!(
                "a row that did not decode then named no version the workspace ran",
                serde_json::from_slice::<InstalledApp>(bytes)
                    .ok()
                    .filter(|row| row.release.app == app && row.release.validate().is_ok())
            ),
            _ => None,
        },
        _ => None,
    }
}

/// The row kept under `app`, if it reads as a valid release of that app: a
/// row naming another app than its key is not trusted.
fn read_version(
    doc: &impl ReadDoc,
    versions: &automerge::ObjId,
    app: &str,
) -> Option<InstalledApp> {
    match doc.get(versions, app).ok()?? {
        (automerge::Value::Scalar(value), _) => match value.as_ref() {
            automerge::ScalarValue::Bytes(bytes) => tilo_failure::ignored!(
                "a row that does not decode names no installed app",
                serde_json::from_slice::<InstalledApp>(bytes)
                    .ok()
                    .filter(|row| row.release.app == app && row.release.validate().is_ok())
            ),
            _ => None,
        },
        _ => None,
    }
}
