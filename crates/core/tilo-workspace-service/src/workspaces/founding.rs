//! A workspace's first history: the seed it starts from, and the record that
//! keeps it here.
//!
//! Every workspace is founded once, by the device that makes it, and names that
//! device as its founder. Every other device takes part through enrolment.
//! The person's own Servers workspace is no exception: its id follows from
//! their root so their devices find it, but only one device founds it. A
//! device that founds it again founds another history under the same id, and
//! a host that holds the first one refuses it.

use std::path::Path;

use ed25519_dalek::SigningKey;
use tilo_peer_state::workspace_membership::{AppRelease, WorkspaceMembershipCommand};
use tilo_peer_state::{PackageRegistry, StatePeer};
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::{DeviceCert, RevocationList};
use web_time::SystemTime;

use super::{
    DOCUMENT_VERSION, DeviceKeys, GovernedWorkspace, RECEIPTS, WorkspaceDocumentEntry, appended,
    collaboration, peer_parts, select_exact_admission,
};

impl GovernedWorkspace {
    pub(super) fn create(
        dir: &Path,
        name: &str,
        authority: &DeviceCert,
        key: &SigningKey,
        device: Option<&DeviceKeys>,
        starts: Starts<'_>,
        now: SystemTime,
    ) -> Result<Self, String> {
        Self::create_in(
            dir,
            &uuid::Uuid::new_v4().to_string(),
            name,
            (authority, key),
            device,
            starts,
            now,
        )
    }

    /// A new governed workspace under the identity `workspace`: a fresh one, or the
    /// workspace a workspace already is when it moves here from the device that
    /// made it (a workspace is its workspace, and keeps it when its governance
    /// moves). `starts` names the registries it fetches apps from, recorded as
    /// `workspace/registries#registry.add` records one, and then the versions it
    /// starts running, as `workspace/apps#install` records one; a workspace
    /// moving here is given neither, and keeps the registries and apps it has.
    pub(super) fn create_in(
        dir: &Path,
        workspace: &str,
        name: &str,
        (authority, key): (&DeviceCert, &SigningKey),
        device: Option<&DeviceKeys>,
        starts: Starts<'_>,
        now: SystemTime,
    ) -> Result<Self, String> {
        let name = tilo_peer_state::workspace_membership::valid_founding_name(name)?;
        // A workspace is founded by a device, which signs its founding and seals its records.
        // A host with no device of its own writes no record and founds nothing.
        let Some(device) = device else {
            return Err(
                tilo_server_protocol::workspaces::AdminRefusal::NoDeviceToFound.to_reason(),
            );
        };
        let workspace = uuid::Uuid::parse_str(workspace)
            .map_err(|_| "a workspace is a UUID".to_owned())?
            .hyphenated()
            .to_string();
        if dir.join(format!("{workspace}.workspace")).exists() {
            return Err("this host has that workspace already".into());
        }
        let roster = vec![authority.clone()];
        let mut peer = StatePeer::new(
            &workspace,
            authority.clone(),
            roster.clone(),
            RevocationList::new(),
            now,
        )?;
        select_exact_admission(&mut peer)?;
        let mut seed = tilo_peer_state::workspace_seed(&workspace, authority, &roster, name, now)?;
        // The workspace's records are sealed channels this device opens on
        // first use: a seed record would go out in the clear before its
        // channel exists, and every member who joins after the channel
        // opened refuses that cleartext. The shared desktop starts with
        // the first layout written to it, sealed, as in a workspace that
        // never had one (`prepare_desktop_document`).
        seed.documents.clear();
        peer.install_seed(seed, key, now)?;
        let (registries, defaults) = starts;
        record_registries(&mut peer, authority, key, registries, now)?;
        record_defaults(&mut peer, authority, key, defaults, now)?;
        // The founding is the founding device's own change, carried as it signed it.
        let founding = peer.workspace_message();
        let path = dir.join(format!("{workspace}.workspace"));
        let mut parts = peer_parts::PeerParts::open(&path)?;
        let record = WorkspaceDocumentEntry {
            version: DOCUMENT_VERSION,
            workspace_id: WorkspaceId::parse(&workspace)
                .ok_or_else(|| "a workspace is named by its id".to_string())?,
            workspace: workspace.clone(),
            owner: authority.clone(),
            peer: parts.stage(&peer)?,
            collaboration: collaboration::CollaborationState::default(),
        };
        let (receipts, _) = appended::Appended::open(&path, RECEIPTS, 0)?;
        let mut this = Self::assemble(record, peer, path, parts, receipts, Some(device))?;
        this.unsent.push(founding);
        this.persist()?;
        Ok(this)
    }
}

/// What a new workspace starts with: the registries it fetches apps from, and the app releases
/// it runs, the releases those registries offer.
pub(super) type Starts<'a> = (&'a [PackageRegistry], &'a [AppRelease]);

/// Record each of `registries` as one the new workspace fetches apps from, as the
/// `workspace/registries#registry.add` action records one, authored by the founder.
fn record_registries(
    peer: &mut StatePeer,
    authority: &DeviceCert,
    key: &SigningKey,
    registries: &[PackageRegistry],
    now: SystemTime,
) -> Result<(), String> {
    for registry in registries {
        let command = WorkspaceMembershipCommand::AddRegistry(registry.clone());
        peer.workspace_mut()
            .apply_workspace_membership_command(authority, &command, key, now)
            .map_err(|error| {
                format!(
                    "the registry {} could not be recorded: {error}",
                    registry.name
                )
            })?;
    }
    Ok(())
}

/// Record each of `defaults` as a version the new workspace runs, as the `workspace/apps#install`
/// action records one: the same release validation, the same grant check, and the same authorship
/// — the founder, whose device made the workspace and signs for it.
fn record_defaults(
    peer: &mut StatePeer,
    authority: &DeviceCert,
    key: &SigningKey,
    defaults: &[AppRelease],
    now: SystemTime,
) -> Result<(), String> {
    for release in defaults {
        let command = WorkspaceMembershipCommand::InstallApp(release.clone());
        peer.workspace_mut()
            .apply_workspace_membership_command(authority, &command, key, now)
            .map_err(|error| {
                format!(
                    "the default app {} could not be recorded: {error}",
                    release.app
                )
            })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspaces::fixtures::governor;

    /// A release as `create_in` records one, with a distinct surface and package per app.
    fn release(app: &str) -> AppRelease {
        AppRelease {
            app: app.into(),
            version: "1.0.0".into(),
            surface: hex::encode(blake3::hash(app.as_bytes()).as_bytes()),
            package: hex::encode(blake3::hash(format!("{app}:package").as_bytes()).as_bytes()),
            publisher: hex::encode(blake3::hash(b"publisher").as_bytes()),
            file: None,
            dev: false,
            pinned: false,
            payload: tilo_peer_state::workspace_membership::AppPayload::Tapp,
        }
    }

    #[test]
    fn a_new_workspace_records_the_apps_it_starts_with_as_its_founders() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let defaults = vec![release("so.tilo.files"), release("so.tilo.writer")];
        let workspace = GovernedWorkspace::create(
            dir.path(),
            "Team",
            &cert,
            &key,
            Some(&crate::workspaces::fixtures::founding_device()),
            (&[], &defaults),
            SystemTime::now(),
        )
        .unwrap();
        let running = workspace.peer().governance().app_versions();
        // The founder's own device recorded them, as `workspace/apps#install` records one.
        assert_eq!(
            running
                .iter()
                .map(|installed| installed.release.clone())
                .collect::<Vec<_>>(),
            defaults
        );
        for installed in &running {
            assert_eq!(installed.installed_by, hex::encode(cert.user_public_key));
        }
    }

    #[test]
    fn a_new_workspace_records_the_registries_it_fetches_apps_from_as_its_founders() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let registries = vec![PackageRegistry {
            name: "Tilo main".into(),
            base_url: "https://apps.tilo.so".into(),
            owner_pubkey_hex: hex::encode([7u8; 32]),
            enabled: true,
            channel: "stable".into(),
        }];
        let defaults = vec![release("so.tilo.files")];
        let workspace = GovernedWorkspace::create(
            dir.path(),
            "Team",
            &cert,
            &key,
            Some(&crate::workspaces::fixtures::founding_device()),
            (&registries, &defaults),
            SystemTime::now(),
        )
        .unwrap();
        assert_eq!(
            workspace.peer().governance().package_registries(),
            registries
        );
        assert_eq!(workspace.peer().governance().app_versions().len(), 1);
    }

    #[test]
    fn the_versions_a_workspace_ran_before_read_from_its_history_most_recent_first() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let version = |number: &str| AppRelease {
            version: number.into(),
            package: hex::encode(blake3::hash(number.as_bytes()).as_bytes()),
            ..release("so.tilo.files")
        };
        // Each recorded in turn replaces the one before, as an update does.
        let ran = vec![
            version("1.0.0"),
            version("1.1.0"),
            version("1.0.0"),
            version("2.0.0"),
        ];
        let workspace = GovernedWorkspace::create(
            dir.path(),
            "Team",
            &cert,
            &key,
            Some(&crate::workspaces::fixtures::founding_device()),
            (&[], &ran),
            SystemTime::now(),
        )
        .unwrap();
        let peer = workspace.peer();
        let governance = peer.governance();
        assert_eq!(
            governance
                .app_version("so.tilo.files")
                .unwrap()
                .release
                .version,
            "2.0.0"
        );
        assert_eq!(
            governance.earlier_app_versions("so.tilo.files"),
            vec![version("1.0.0"), version("1.1.0")],
            "the most recent first, each once, never the version it runs"
        );
        assert!(governance.earlier_app_versions("so.tilo.writer").is_empty());
        drop(peer);
    }

    #[test]
    fn a_workspace_that_keeps_the_apps_it_has_records_no_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let workspace = GovernedWorkspace::create(
            dir.path(),
            "Placed",
            &cert,
            &key,
            Some(&crate::workspaces::fixtures::founding_device()),
            (&[], &[]),
            SystemTime::now(),
        )
        .unwrap();
        assert!(workspace.peer().governance().app_versions().is_empty());
    }

    #[test]
    fn a_default_the_install_action_would_refuse_fails_the_creation() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let mut malformed = release("so.tilo.broken");
        malformed.surface = "not a hash".into();
        let made = GovernedWorkspace::create(
            dir.path(),
            "Team",
            &cert,
            &key,
            Some(&crate::workspaces::fixtures::founding_device()),
            (&[], &[malformed]),
            SystemTime::now(),
        );
        assert!(
            made.is_err(),
            "an app release the action refuses is refused"
        );
    }
}
