//! Durable installation of a personal device's enrollment on a native device.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use tilo_device_identity::IdentityPaths;
use tilo_identity::{device_fleet::DeviceFleet, person_files::as_person};
use tilo_server_protocol::ProcessLease;
use tilo_user_identity::{DeviceCert, verify_device_cert_at};

use crate::{
    ResponsePayload, fleet_store, now_ns, personal_devices_of, read_bounded_json,
    remember_personal_devices, restore_owner_from_parent, share_directory_like_parent,
};

#[cfg(unix)]
mod offline;
#[cfg(unix)]
pub use offline::{OfflineInstall, install_from_local_identity};

/// A native personal enrollment's device layout and account. The roots are
/// chosen by the local caller. When acting for another account, supply its
/// uid explicitly so writes into the person's directory run as that account.
pub struct PersonalInstall<'a> {
    identity_root: &'a Path,
    identity_dir: &'a Path,
    person_uid: Option<u32>,
    // An image is a private staging tree which will be moved onto a fresh
    // system. Its selector is portable, but its live registry paths are not.
    image_source: Option<[u8; 32]>,
}

impl<'a> PersonalInstall<'a> {
    #[must_use]
    pub fn new(identity_root: &'a Path, identity_dir: &'a Path, person_uid: Option<u32>) -> Self {
        Self {
            identity_root,
            identity_dir,
            person_uid,
            image_source: None,
        }
    }

    /// Stage the verified personal answer, then confirm it before publishing.
    /// The caller must supply the verified answer for this device and
    /// authenticate the approver's Finalized packet in `confirm`. A caller
    /// installing a locally verified out-of-band certificate has already
    /// obtained approval and can return success directly.
    ///
    /// # Errors
    /// Returns an error for an invalid projection, failed confirmation,
    /// expired requester lease, or an I/O failure. Once the durable commit
    /// marker is ready, [`Self::recover`] can finish a partial publication.
    pub fn install(
        &self,
        payload: &ResponsePayload,
        requester_lease: Option<ProcessLease>,
        confirm: impl FnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<PathBuf> {
        self.install_with_checkpoint(payload, requester_lease, confirm, |_| Ok(()))
    }

    fn install_with_checkpoint(
        &self,
        payload: &ResponsePayload,
        requester_lease: Option<ProcessLease>,
        confirm: impl FnOnce() -> anyhow::Result<()>,
        checkpoint: impl FnMut(PersonalCommitCheckpoint) -> anyhow::Result<()>,
    ) -> anyhow::Result<PathBuf> {
        self.install_projection(
            &PersonalProjection {
                cert: &payload.cert,
                identity: payload
                    .public_identity_json
                    .as_deref()
                    .context("approval omitted the public identity")?,
                fleet: payload
                    .fleet_json
                    .as_deref()
                    .context("approval omitted the fleet")?,
                peers: payload
                    .home_peers_json
                    .as_deref()
                    .context("approval omitted home peers")?,
            },
            requester_lease,
            confirm,
            checkpoint,
        )
    }

    fn install_projection(
        &self,
        projection: &PersonalProjection<'_>,
        requester_lease: Option<ProcessLease>,
        confirm: impl FnOnce() -> anyhow::Result<()>,
        checkpoint: impl FnMut(PersonalCommitCheckpoint) -> anyhow::Result<()>,
    ) -> anyhow::Result<PathBuf> {
        let paths = IdentityPaths::under(self.identity_root);
        paths.ensure_directories()?;
        let (marker_path, marker) = stage_personal_enrollment(self, projection, requester_lease)?;
        confirm()?;
        // A successful confirmation and the original live requester make
        // this transaction eligible for recovery, even after that lease ends.
        let marker = authorize_personal_commit(&marker_path, marker)?;
        self.commit(&paths, &marker_path, &marker, requester_lease, checkpoint)
    }

    /// Resume confirmed transactions belonging to this account and identity
    /// directory, using the current requester's lease for publication.
    ///
    /// # Errors
    /// Returns an error for invalid staging, an expired requester lease, or
    /// an I/O failure.
    pub fn recover(&self, requester_lease: Option<ProcessLease>) -> anyhow::Result<()> {
        let directory = enrollment_commit_dir(self.identity_root);
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let paths = IdentityPaths::under(self.identity_root);
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            // Any member of the device-trust group can leave a file here; only
            // this account's own markers are its enrollments.
            #[cfg(unix)]
            if std::os::unix::fs::MetadataExt::uid(&entry.metadata()?)
                != rustix::process::geteuid().as_raw()
            {
                continue;
            }
            let marker: PersonalEnrollmentMarker =
                serde_json::from_slice(&std::fs::read(entry.path())?)
                    .context("personal enrollment recovery marker is invalid")?;
            if marker.identity_dir == self.identity_dir && marker.commit_ready {
                self.commit(&paths, &entry.path(), &marker, requester_lease, |_| Ok(()))?;
            }
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "keep the durable publication order and its lease checks together"
    )]
    fn commit(
        &self,
        paths: &IdentityPaths,
        marker_path: &Path,
        marker: &PersonalEnrollmentMarker,
        requester_lease: Option<ProcessLease>,
        mut checkpoint: impl FnMut(PersonalCommitCheckpoint) -> anyhow::Result<()>,
    ) -> anyhow::Result<PathBuf> {
        if marker.schema_version != PERSONAL_ENROLLMENT_SCHEMA
            || !marker.commit_ready
            || marker.identity_dir != self.identity_dir
        {
            bail!("personal enrollment marker is invalid");
        }
        let stage_dir = private_stage_dir(self.identity_root, &marker.stage_name)?;
        let cert_bytes = std::fs::read(stage_dir.join("default-cert.bin"))?;
        let cert: DeviceCert = tilo_user_identity::canonical::from_canonical_bytes(&cert_bytes)?;
        if cert.user_public_key != marker.user_public_key
            || cert.device_public_key != marker.device_public_key
            || verify_device_cert_at(&cert, now_ns()?).is_err()
            || std::fs::read(stage_dir.join("user-pubkey.bin"))? != marker.user_public_key
        {
            bail!("staged personal enrollment no longer validates");
        }
        // Check the lease before even creating the user identity directory.
        let mut before_publish = |step| {
            validate_requester(requester_lease)?;
            checkpoint(step)
        };
        before_publish(PersonalCommitCheckpoint::Record)?;
        // The identity directory is the person's: a root-run (headless) join
        // writes there as the person, never as root.
        let record = identity_record_path(self.identity_dir, marker.user_public_key);
        let record_json = std::fs::read(stage_dir.join("record.json"))?;
        as_person(self.person_uid, self.identity_dir, || {
            std::fs::create_dir_all(self.identity_dir)?;
            tilo_files::native::atomic::write_with_mode(&record, &record_json, 0o600)
        })?;
        before_publish(PersonalCommitCheckpoint::Fleet)?;
        let fleet = self.install_staged_fleet(marker, &stage_dir)?;
        before_publish(PersonalCommitCheckpoint::HomePeers)?;
        let staged_peers = tilo_server_protocol::checked_home_peers(&std::fs::read(
            stage_dir.join("home-peers.json"),
        )?)
        .context("the staged home peers are not a registry this build accepts")?;
        tilo_server_protocol::write_home_peers(
            &tilo_server_protocol::home_peer_registry_path(self.identity_root),
            &staged_peers,
        )?;
        before_publish(PersonalCommitCheckpoint::Registry)?;
        if self.image_source.is_none() {
            let registry_json = std::fs::read(stage_dir.join("registry.json"))?;
            as_person(self.person_uid, self.identity_dir, || {
                tilo_files::native::atomic::write_with_mode(
                    &self.identity_dir.join("registry.json"),
                    &registry_json,
                    0o600,
                )
            })?;
        }
        before_publish(PersonalCommitCheckpoint::UserPublicKey)?;
        // The public key and certificates are public, whatever the umask this
        // commit runs under: the session reads them to verify the device, and
        // reaches the per-identity ones through a directory shared like its parent.
        let public = tilo_device_identity::persist::PUBLIC_FILE_MODE;
        tilo_files::native::atomic::write_with_mode(
            &paths.user_pubkey,
            &marker.user_public_key,
            public,
        )?;
        before_publish(PersonalCommitCheckpoint::PerUserCert)?;
        let per_user_cert = paths.device_cert_for_user(&marker.user_public_key);
        share_directory_like_parent(
            per_user_cert
                .parent()
                .context("per-user certificate has no directory")?,
        )?;
        tilo_files::native::atomic::write_with_mode(
            &per_user_cert,
            &std::fs::read(stage_dir.join("per-user-cert.bin"))?,
            public,
        )?;
        // This device and the approver become each other's personal devices. The
        // approver's certificate comes from the root-signed fleet staged with this
        // enrollment, which the fleet just adopted, and must verify under the
        // identity this device joins. A device the home-peer registry names is a
        // home peer, reached through that registry, not one of the person's
        // personal devices.
        before_publish(PersonalCommitCheckpoint::PersonalPeers)?;
        let personal_devices = match self.image_source {
            // The imager pairs the target directly with the setup machine.
            Some(source) => vec![source],
            None => personal_devices_of(
                marker.device_public_key,
                marker.user_public_key,
                &fleet,
                &staged_peers,
                now_ns()?,
            ),
        };
        let personal_peers = remember_personal_devices(self.identity_root, &personal_devices)?;
        // Publish this last: every projection file has been durably installed.
        before_publish(PersonalCommitCheckpoint::DefaultCert)?;
        tilo_files::native::atomic::write_with_mode(&paths.device_cert, &cert_bytes, public)?;
        // The joined identity is the one this device now acts for: select it, or
        // the next login resolves the absent default and asks to create one. The
        // first-use UI reads a selected joined identity as a finished join, so it
        // is selected only once every file of the join is in place. Only the
        // directory's owner can select (the selector is theirs alone); a root-run
        // join for someone else (`--person-uid`) leaves it to them.
        if self.person_uid.is_none() {
            let document_id = record
                .file_stem()
                .and_then(|stem| stem.to_str())
                .context("joined identity record has no runtime id")?;
            as_person(self.person_uid, self.identity_dir, || {
                tilo_identity::store::set_active_identity(self.identity_dir, document_id)
            })
            .context("select the joined identity")?;
        }
        // A root-run (headless) enrollment wrote these, under the identity root,
        // as root. Restore each one's directory owner and group before the marker
        // is cleared, so recovery repeats this step if it is interrupted. Parents
        // come before their children.
        for path in [
            tilo_server_protocol::home_peer_registry_path(self.identity_root),
            personal_peers,
            paths.user_pubkey.clone(),
            per_user_cert
                .parent()
                .map(Path::to_path_buf)
                .context("per-user certificate has no directory")?,
            per_user_cert,
            paths.device_cert.clone(),
        ] {
            restore_owner_from_parent(&path)?;
        }
        std::fs::remove_file(marker_path)?;
        if let Some(parent) = marker_path.parent()
            && let Ok(directory) = std::fs::File::open(parent)
        {
            directory.sync_all()?;
        }
        let _ = std::fs::remove_dir_all(stage_dir);
        Ok(record)
    }
}

/// Check the originating first-use requester's lifetime when one is required.
///
/// # Errors
/// Returns an error when the leased process is gone or has been replaced.
pub fn validate_requester(lease: Option<ProcessLease>) -> anyhow::Result<()> {
    if let Some(lease) = lease {
        lease
            .validate()
            .context("the first-use requester exited; nearby pairing was revoked")?;
    }
    Ok(())
}

/// The files of a personal enrollment live in a private, durable staging
/// directory until the approving device has sent `Finalized`.  A marker is
/// deliberately retained while the staged files are being published: a boot
/// or retry can finish the exact same transaction instead of guessing which
/// of a set of independently-atomic renames reached disk.
#[derive(Serialize, Deserialize)]
struct PersonalEnrollmentMarker {
    schema_version: u32,
    identity_dir: PathBuf,
    stage_name: String,
    user_public_key: [u8; 32],
    device_public_key: [u8; 32],
    /// The exact compositor lifetime that received and confirmed this
    /// enrollment. Before `commit_ready`, no other requester may authorize it.
    requester_lease: Option<ProcessLease>,
    /// Set only after the recipient authenticates the approver's Finalized
    /// packet. Prepared-but-unconfirmed transactions must never activate on
    /// a later timeout retry.
    commit_ready: bool,
}

const PERSONAL_ENROLLMENT_SCHEMA: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PersonalCommitCheckpoint {
    Record,
    Fleet,
    HomePeers,
    Registry,
    UserPublicKey,
    PerUserCert,
    PersonalPeers,
    DefaultCert,
}

fn enrollment_commit_dir(identity_root: &Path) -> PathBuf {
    identity_root.join("enrollment-commits")
}

fn enrollment_stage_dir(identity_root: &Path) -> PathBuf {
    identity_root.join("enrollment-staging")
}

fn personal_marker_path(identity_root: &Path, stage_name: &str) -> PathBuf {
    enrollment_commit_dir(identity_root).join(format!("{stage_name}.json"))
}

fn random_stage_name() -> anyhow::Result<String> {
    let mut token = [0_u8; 16];
    getrandom::fill(&mut token)
        .map_err(|error| anyhow::anyhow!("generate a private enrollment staging name: {error}"))?;
    Ok(format!("personal-{}", hex::encode(token)))
}

/// Create an enrollment's staging directory in the shared staging area,
/// which every member of the device-trust group may write: under a name
/// from `name` that nothing holds yet, as this account's private 0700
/// directory. `mkdir` follows no symlink and fails on any name already
/// taken, by a planted link or anything else; a taken name is skipped for
/// the next one `name` gives.
fn create_stage_dir(
    identity_root: &Path,
    mut name: impl FnMut() -> anyhow::Result<String>,
) -> anyhow::Result<(String, PathBuf)> {
    let parent = enrollment_stage_dir(identity_root);
    std::fs::create_dir_all(&parent)?;
    for _ in 0..8 {
        let stage_name = name()?;
        let stage_dir = parent.join(&stage_name);
        let mut directory = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut directory, 0o700);
        match directory.create(&stage_dir) {
            Ok(()) => return Ok((stage_name, stage_dir)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    bail!("no free enrollment staging name")
}

/// The staging directory a marker names, if it is still the private
/// directory this account created: not a symlink, owned by this account and
/// closed to everyone else.
fn private_stage_dir(identity_root: &Path, stage_name: &str) -> anyhow::Result<PathBuf> {
    if stage_name.is_empty()
        || stage_name == ".."
        || stage_name.contains('/')
        || stage_name.contains('\\')
    {
        bail!("personal enrollment marker is invalid");
    }
    let stage_dir = enrollment_stage_dir(identity_root).join(stage_name);
    let metadata = std::fs::symlink_metadata(&stage_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if !metadata.is_dir()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
        {
            bail!("personal enrollment staging is not this account's private directory");
        }
    }
    #[cfg(not(unix))]
    if !metadata.is_dir() {
        bail!("personal enrollment staging is not a directory");
    }
    Ok(stage_dir)
}

struct PersonalProjection<'a> {
    cert: &'a DeviceCert,
    identity: &'a [u8],
    fleet: &'a [u8],
    peers: &'a [u8],
}

fn identity_record_path(identity_dir: &Path, user: [u8; 32]) -> PathBuf {
    let prefix = hex::encode(user).chars().take(55).collect::<String>();
    identity_dir.join(format!("identity-{prefix}.json"))
}

fn validate_public_identity(bytes: &[u8], user: [u8; 32]) -> anyhow::Result<()> {
    let identity: serde_json::Value = serde_json::from_slice(bytes)?;
    let threshold = identity
        .get("threshold_identity")
        .and_then(serde_json::Value::as_object)
        .context("approval returned an invalid public identity")?;
    if threshold.get("group_public_key") != Some(&serde_json::json!(user))
        || threshold
            .get("secret_bytes")
            .is_some_and(|secret| secret != &serde_json::json!([]))
        || threshold.get("single_device") != Some(&serde_json::json!(false))
    {
        bail!("approval returned an identity containing private or mismatched key material");
    }
    Ok(())
}

fn checked_personal_projection(
    install: &PersonalInstall<'_>,
    projection: &PersonalProjection<'_>,
) -> anyhow::Result<(DeviceFleet, Vec<tilo_server_protocol::HomePeerRegistration>)> {
    let user = projection.cert.user_public_key;
    let identity_id = hex::encode(user);
    let fleet = install
        .checked_fleet(projection.fleet, user, projection.cert.device_public_key)
        .context("approval returned an invalid device fleet")?;
    validate_public_identity(projection.identity, user)?;

    let incoming = tilo_server_protocol::checked_home_peers(projection.peers)
        .context("the approval's home peers are not a registry this build accepts")?;
    if incoming
        .iter()
        .any(|entry| entry.user_public_key != identity_id)
    {
        bail!("approval returned an invalid home-peer registry");
    }

    let mut existing = tilo_server_protocol::read_home_peers(
        &tilo_server_protocol::home_peer_registry_path(install.identity_root),
    )
    .context("the existing home-peer registry is not one this build accepts")?;

    // A response projects routes for one identity. Do not use it as a global
    // snapshot and erase other identities' independently verified routes.
    existing.retain(|entry| entry.user_public_key != identity_id);
    existing.extend(incoming);
    if existing.len() > 128 {
        bail!("merged home-peer registry is full");
    }
    existing.sort_by(|left, right| {
        left.user_public_key
            .cmp(&right.user_public_key)
            .then_with(|| left.device_node_id.cmp(&right.device_node_id))
    });
    Ok((fleet, existing))
}

fn projected_registry(identity_dir: &Path, user: [u8; 32]) -> anyhow::Result<Vec<u8>> {
    let identity_id = hex::encode(user);
    let registry_path = identity_dir.join("registry.json");
    let mut registry: serde_json::Value =
        read_bounded_json(&registry_path, "existing identity registry")?.unwrap_or_else(|| {
            serde_json::json!({
                "schema_version": 1,
                "default_identity": identity_id,
                "identities": {},
                "workspace_identities": {}
            })
        });
    let valid_registry = registry.as_object().is_some_and(|root| {
        root.get("identities")
            .is_some_and(serde_json::Value::is_object)
            && root
                .get("workspace_identities")
                .is_some_and(serde_json::Value::is_object)
    });
    if !valid_registry {
        bail!("existing identity registry has an invalid schema");
    }

    let resource_path = identity_record_path(identity_dir, user);
    let registry_root = registry
        .as_object_mut()
        .context("existing identity registry has an invalid schema")?;
    registry_root.insert(
        "default_identity".into(),
        serde_json::Value::String(identity_id.clone()),
    );
    registry_root
        .get_mut("identities")
        .and_then(serde_json::Value::as_object_mut)
        .context("existing identity registry has an invalid identities map")?
        .insert(
            identity_id.clone(),
            serde_json::json!({
                "id": identity_id,
                "display_name": "My Tilo identity",
                // The registry the runtime reads names an identity's file
                // `record_path`, and refuses a field it does not know: one
                // written another way holds identities it cannot start from.
                "record_path": resource_path,
                "user_public_key": user
            }),
        );

    Ok(serde_json::to_vec_pretty(&registry)?)
}

fn stage_personal_enrollment(
    install: &PersonalInstall<'_>,
    projection: &PersonalProjection<'_>,
    requester_lease: Option<ProcessLease>,
) -> anyhow::Result<(PathBuf, PersonalEnrollmentMarker)> {
    let (fleet, peers) = checked_personal_projection(install, projection)?;
    let registry = if install.image_source.is_none() {
        Some(projected_registry(
            install.identity_dir,
            projection.cert.user_public_key,
        )?)
    } else {
        None
    };
    let identity_root = install.identity_root;
    let (stage_name, stage_dir) = create_stage_dir(identity_root, random_stage_name)?;
    let cert = tilo_user_identity::canonical::canonical_bytes(projection.cert)?;
    tilo_files::native::atomic::write_with_mode(
        &stage_dir.join("record.json"),
        projection.identity,
        0o600,
    )?;
    if let Some(registry) = registry {
        tilo_files::native::atomic::write_with_mode(
            &stage_dir.join("registry.json"),
            &registry,
            0o600,
        )?;
    }
    tilo_files::native::atomic::write(
        &stage_dir.join("fleet.json"),
        &serde_json::to_vec_pretty(&fleet)?,
    )?;
    tilo_files::native::atomic::write_with_mode(
        &stage_dir.join("home-peers.json"),
        &serde_json::to_vec_pretty(&peers)?,
        0o644,
    )?;
    tilo_files::native::atomic::write(
        &stage_dir.join("user-pubkey.bin"),
        &projection.cert.user_public_key,
    )?;
    tilo_files::native::atomic::write(&stage_dir.join("per-user-cert.bin"), &cert)?;
    tilo_files::native::atomic::write(&stage_dir.join("default-cert.bin"), &cert)?;
    let marker_path = personal_marker_path(identity_root, &stage_name);
    let marker = PersonalEnrollmentMarker {
        schema_version: PERSONAL_ENROLLMENT_SCHEMA,
        identity_dir: install.identity_dir.to_path_buf(),
        stage_name,
        user_public_key: projection.cert.user_public_key,
        device_public_key: projection.cert.device_public_key,
        requester_lease,
        commit_ready: false,
    };
    std::fs::create_dir_all(enrollment_commit_dir(identity_root))?;
    tilo_files::native::atomic::write_with_mode(
        &marker_path,
        &serde_json::to_vec_pretty(&marker)?,
        0o600,
    )?;
    Ok((marker_path, marker))
}

fn authorize_personal_commit(
    marker_path: &Path,
    mut marker: PersonalEnrollmentMarker,
) -> anyhow::Result<PersonalEnrollmentMarker> {
    validate_requester(marker.requester_lease)?;
    marker.commit_ready = true;
    tilo_files::native::atomic::write_with_mode(
        marker_path,
        &serde_json::to_vec_pretty(&marker)?,
        0o600,
    )?;
    Ok(marker)
}

impl PersonalInstall<'_> {
    fn checked_fleet(
        &self,
        bytes: &[u8],
        user: [u8; 32],
        device: [u8; 32],
    ) -> anyhow::Result<DeviceFleet> {
        let identity_id = hex::encode(user);
        if self.image_source.is_none() {
            return Ok(
                fleet_store(self.identity_root, self.identity_dir, self.person_uid)
                    .accept_transferred(&identity_id, bytes, device)?,
            );
        }

        // A fresh image's fleet came from the local canonical store, not a
        // packet. Keep its full contents even if they exceed the wire limit.
        // The store verifies the document when adopting it below.
        let fleet: DeviceFleet = serde_json::from_slice(bytes)?;
        if fleet.identity_id != identity_id
            || fleet
                .certificates
                .iter()
                .any(|cert| cert.user_public_key != user)
            || fleet
                .revocations
                .iter()
                .any(|entry| entry.user_public_key != user)
            || !fleet
                .certificates
                .iter()
                .any(|cert| cert.device_public_key == device)
        {
            bail!("the local fleet does not match the enrolled identity");
        }
        Ok(fleet)
    }

    /// Adopt the approved fleet as this device's own record of the person's devices.
    fn install_staged_fleet(
        &self,
        marker: &PersonalEnrollmentMarker,
        stage_dir: &Path,
    ) -> anyhow::Result<DeviceFleet> {
        let fleet = self
            .checked_fleet(
                &std::fs::read(stage_dir.join("fleet.json"))?,
                marker.user_public_key,
                marker.device_public_key,
            )
            .context("staged device fleet")?;
        fleet_store(self.identity_root, self.identity_dir, self.person_uid)
            .adopt_transferred(&fleet)
            .context("install the joined identity's device fleet")?;
        Ok(fleet)
    }
}

#[cfg(all(test, unix))]
mod tests;
