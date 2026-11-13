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

use automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};
use collab_types::signer::DeviceSigner;
use tilo_action_policy::roles::governance;
use tilo_user_identity::DeviceCert;

use super::facades::PeerGovernance;
use super::{StatePeer, SystemTime, err, now_ns};
use crate::workspace_membership::{AppRelease, WorkspaceMembershipCommand};

/// The governed map that keeps the workspace's app versions, by app id.
const APP_VERSIONS: &str = "app_versions_v1";

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
        if self
            .governance()
            .app_version(&release.app)
            .is_some_and(|installed| &installed.release == release)
        {
            return Ok(false);
        }
        let row = InstalledApp {
            release: release.clone(),
            installed_by: member,
            installed_at_ns: now_ns(now),
        };
        self.put_app_version(&release.app, Some(&row), key, now)
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
        self.put_app_version(app, None, key, now)
    }

    /// The person behind `caller`, when the workspace lets them install apps.
    fn check_app_governance(&self, caller: &DeviceCert, now: SystemTime) -> Result<String, String> {
        let member = self.membership(caller.device_public_key, now)?;
        let bound = tilo_action_policy::roles::foreign_device_bound();
        member
            .policy
            .check_action(
                &member.authority(bound),
                governance::INSTALL_APP,
                &tilo_action_policy::Context::at_time(now),
                false,
            )
            .map_err(|refusal| refusal.to_string())?;
        Ok(member.person)
    }

    /// Write or remove the version of `app` in one governed commit.
    fn put_app_version(
        &mut self,
        app: &str,
        row: Option<&InstalledApp>,
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
        let versions = match doc.get(ROOT, APP_VERSIONS).map_err(err)? {
            Some((_, map)) => map,
            None => doc
                .put_object(ROOT, APP_VERSIONS, ObjType::Map)
                .map_err(err)?,
        };
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
        doc.commit();
        self.write_governance(doc, &before, key, now)
    }

    /// The surface the workspace runs `app` with, when it chose a version.
    pub(super) fn workspace_surface(&self, app: &str) -> Option<[u8; 32]> {
        self.governance().app_version(app)?.release.surface_hash()
    }
}

impl PeerGovernance<'_> {
    /// Every app version the workspace runs, by app id.
    #[must_use]
    pub fn app_versions(self) -> Vec<InstalledApp> {
        let Some(doc) = self.peer.core.workspace_doc.as_ref() else {
            return Vec::new();
        };
        let Ok(Some((_, versions))) = doc.get(ROOT, APP_VERSIONS) else {
            return Vec::new();
        };
        let mut out: Vec<InstalledApp> = doc
            .keys(&versions)
            .filter_map(|app| read_version(&**doc, &versions, &app))
            .collect();
        out.sort_by(|a, b| a.release.app.cmp(&b.release.app));
        out
    }

    /// The version of `app` the workspace runs, if it chose one.
    #[must_use]
    pub fn app_version(self, app: &str) -> Option<InstalledApp> {
        let doc = self.peer.core.workspace_doc.as_ref()?;
        let (_, versions) = doc.get(ROOT, APP_VERSIONS).ok()??;
        read_version(&**doc, &versions, app)
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
