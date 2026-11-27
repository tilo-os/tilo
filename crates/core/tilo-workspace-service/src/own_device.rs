//! A device's own workspace service: the service a server runs, opened in the process of a
//! device that holds its workspaces itself (a desktop, a runtime). It signs as the device, whose
//! key and certificate its embedder holds, so the records it writes, the links it serves and the
//! content it answers all name the one key the device is certified as.
//!
//! The service holds the workspaces the device founded and the member copies of the ones it
//! joined, writing as the device in them, and takes part in their key groups through the
//! device's key service: it asks that service for a group's keys, and hands what its members
//! send for the groups to a sink of the embedder's, so the service and the device never found
//! two groups under one leaf.

mod key_service;

pub use key_service::KeyService;

use std::path::PathBuf;
use std::sync::Arc;

use collab_types::signer::DeviceSigner;
use tilo_user_identity::DeviceCert;
use tilo_workspace_keys::KeyProvider;

use crate::service::{ServiceConfig, ServiceError, ServiceIdentity, WorkspaceService};
use crate::workspaces::{
    AskingBehind, DeviceKeyGroups, GroupKeys, GroupKeysAnswerFuture, KeySink, KeysChangedFuture,
    KeysFuture, MemberDevice,
};

pub use crate::workspaces::sealed_documents::OPEN_WAIT;

/// The device a service is opened for: the certificate its person issued it, and what signs as
/// it. What the service writes in a workspace this device takes part in is signed as it.
pub struct Device<'a> {
    pub cert: &'a DeviceCert,
    pub signer: &'a Arc<dyn DeviceSigner>,
    /// The device itself, when its service acts as it with material beyond its key and
    /// certificate: a server, whose revocations it carries.
    pub itself: Option<ServiceIdentity>,
}

/// Open the workspace service `device` keeps under `store`, on `endpoint`, its groups kept by
/// `groups`. Each workspace it holds is decided by that workspace's signed member list, never by
/// the device holding it. `configure` adjusts the service's configuration beyond
/// what every device's service shares.
///
/// The service's member copies act as `device`, and that is set here rather than left to each
/// embedder: a device that holds a member copy syncs it with the device serving it, and a
/// service opened without its device keeps the copy and never syncs it. Every device's service
/// is opened through here, so none of them can diverge, a server among them: it keeps a
/// workspace of its person's that another device serves as a member copy too.
///
/// The service signs as the device: one device has one key, so its links, the records it writes
/// and what it answers name the key its certificate does.
///
/// # Errors
/// The device's key is not held in the clear, or the service's store cannot be opened.
pub async fn open(
    store: PathBuf,
    device: Device<'_>,
    endpoint: iroh::Endpoint,
    groups: DeviceKeyGroups,
    configure: impl FnOnce(ServiceConfig) -> ServiceConfig,
) -> Result<WorkspaceService, ServiceError> {
    let identity = if let Some(itself) = device.itself {
        itself
    } else {
        let key = device.signer.device_signing_key().ok_or_else(|| {
            ServiceError::Identity(
                "this device's workspace service signs as its device, whose key it does not \
                 hold in the clear"
                    .into(),
            )
        })?;
        ServiceIdentity::as_device(device.cert, key)
    };
    // A person's own device takes nobody else's work, unless `configure` says otherwise, and
    // its member copies act as it.
    let config = configure(ServiceConfig {
        member: Some(Arc::new(MemberDevice {
            cert: device.cert.clone(),
            signer: Arc::clone(device.signer),
        })),
        app_files: app_files(),
        ..ServiceConfig::new(store, tilo_server_protocol::WorkSetting::none())
    });
    WorkspaceService::open(config, identity, endpoint, Some(groups)).await
}

/// Where a device keeps its workspace service's store, below the device's directory.
const SERVICE: &str = "service";
/// Where a device's key service keeps its sealed group state, below the device's directory.
const GROUPS: &str = "groups";

/// A device's workspace service and the key service beside it, as [`open_in`] opened them.
pub struct Opened {
    pub service: WorkspaceService,
    pub keys: KeyService,
}

/// The key sealing a device's group state: derived from the device key, which the device keeps
/// beside it.
#[must_use]
pub fn vault_key(key: &ed25519_dalek::SigningKey) -> key_group::VaultKey {
    key_group::VaultKey::new(blake3::derive_key(
        "tilo device workspace key vault v1",
        &key.to_bytes(),
    ))
}

/// Open `device`'s workspace service and the key service beside it in the device's directory
/// `dir`, on `endpoint`, its group state sealed with `vault` ([`vault_key`]) and what the groups'
/// members seal checked against `seal_checks`. Every device keeps them in this one layout, a
/// server among them; `configure` is as [`open`] takes it.
///
/// # Errors
/// The key service's state or the service's store cannot be opened.
pub async fn open_in(
    dir: &std::path::Path,
    device: Device<'_>,
    vault: key_group::VaultKey,
    endpoint: iroh::Endpoint,
    seal_checks: Arc<dyn tilo_workspace_keys::SealCheckRegistry>,
    configure: impl FnOnce(ServiceConfig) -> ServiceConfig,
) -> Result<Opened, ServiceError> {
    let keys = KeyService::open(
        "tilo-device-groups",
        &dir.join(GROUPS),
        vault,
        Arc::clone(device.signer),
        seal_checks,
    )?;
    let service = open(
        dir.join(SERVICE),
        device,
        endpoint,
        keys.groups(),
        |config| {
            // The revocation log sits beside the identity in the device's own directory, where
            // the daemon reads it back on its next start; the service's state is the
            // directory below.
            configure(ServiceConfig {
                revocation_log_root: dir.to_path_buf(),
                ..config
            })
        },
    )
    .await?;
    Ok(Opened { service, keys })
}

/// Where the device's native apps kept files of their own, which its workspaces take in.
#[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
fn app_files() -> Vec<PathBuf> {
    crate::workspaces::profile_import::homes()
}

/// A device that runs no apps kept no app files.
#[cfg(not(all(feature = "apps", not(target_arch = "wasm32"))))]
fn app_files() -> Vec<PathBuf> {
    Vec::new()
}

/// Keys a workspace's host handed a session that takes no part in the workspace's group, by
/// workspace: what it reads every member's records with.
pub type HandedKeys = Box<dyn Fn(&str) -> Option<key_group::ContentKeys> + Send + Sync>;

/// The device's key service as its workspace service asks it, answering first from `handed`,
/// with `sink` taking what the service's members send for the groups, and `behind` asking the
/// person's other devices for the frames of a group this device holds behind them.
#[must_use]
pub fn key_groups(
    keys: KeyProvider,
    handed: Option<HandedKeys>,
    sink: Option<Arc<dyn KeySink>>,
    behind: Option<AskingBehind>,
) -> DeviceKeyGroups {
    DeviceKeyGroups {
        keys: Arc::new(DeviceGroupKeys {
            keys,
            handed,
            behind,
        }),
        sink,
    }
}

/// The device's keys, as its workspace service asks for a workspace's: those the workspace's
/// host handed this device, else its key service's.
struct DeviceGroupKeys {
    keys: KeyProvider,
    handed: Option<HandedKeys>,
    /// Where the person's other devices are asked for a group's frames this device is behind.
    behind: Option<AskingBehind>,
}

impl GroupKeys for DeviceGroupKeys {
    fn named_readers(
        &self,
        workspace: collab_types::WorkspaceId,
        document: &str,
    ) -> Option<String> {
        self.keys.get()?.named_readers(workspace, document)
    }

    fn group_keys<'a>(
        &'a self,
        workspace: &'a str,
        readers: &'a str,
        create: bool,
    ) -> GroupKeysAnswerFuture<'a> {
        Box::pin(async move {
            if readers == collab_types::EVERY_MEMBER
                && let Some(keys) = self.handed.as_ref().and_then(|handed| handed(workspace))
            {
                return Some(Ok(keys));
            }
            if self.keys.get().is_none() {
                return Some(Err("this device's key service is not running".into()));
            }
            self.keys
                .group_keys(workspace, readers, create, OPEN_WAIT)
                .await
        })
    }

    fn keys_changed<'a>(&'a self, workspace: &'a str, after: u64) -> KeysChangedFuture<'a> {
        Box::pin(async move {
            let workspace = tilo_desktop_model::WorkspaceId::parse(workspace)?;
            Some(self.keys.workspace_changed(workspace, after).await)
        })
    }

    fn behind(&self, group: collab_types::WorkspaceId, held: u64) {
        if let Some(behind) = &self.behind {
            behind(group, held);
        }
    }

    fn access_request(
        &self,
        workspace: collab_types::WorkspaceId,
    ) -> KeysFuture<'_, Option<collab_types::MlsAccessRequestV1>> {
        Box::pin(async move {
            self.keys
                .get()?
                .ask_on_entry(workspace, tilo_clock::now(), tilo_clock::unix_now())
                .await
        })
    }

    fn admit(
        &self,
        workspace: collab_types::WorkspaceId,
        request: collab_types::MlsAccessRequestV1,
    ) -> KeysFuture<'_, Result<Vec<neighbour_wire::NeighbourMsg>, String>> {
        Box::pin(async move {
            // The group exists before anyone is committed into it: the device that founded the
            // workspace opens it on its first use. Only the key service's own part in the group
            // commits another in: keys a host handed this device admit no one.
            match self
                .keys
                .group_keys(
                    &workspace.to_string(),
                    collab_types::EVERY_MEMBER,
                    true,
                    OPEN_WAIT,
                )
                .await
            {
                Some(Ok(_)) => {}
                Some(Err(error)) => return Err(error),
                None => return Err("this device takes no part in the workspace's keys".into()),
            }
            self.keys
                .get()
                .ok_or("this device's key service is not running")?
                .admit(workspace, request, tilo_clock::unix_now())
                .await
        })
    }
}
