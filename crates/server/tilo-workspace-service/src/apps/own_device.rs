//! A device's own workspace service: the service a server runs, opened in the process of a
//! device that holds its workspaces itself (a desktop, a runtime), under a key of the service's
//! own that the device certifies, so the device's engine and the service never number changes
//! from one count.
//!
//! The service holds the workspaces the device governs and the member copies of the ones it
//! joined, writing as the device in them, and takes part in their key groups through the
//! device's key service: it asks that service for a group's keys, and hands what its members
//! send for the groups to a sink of the embedder's, so the service and the device never found
//! two groups under one leaf.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use collab_types::signer::DeviceSigner;
use tilo_user_identity::DeviceCert;
use tilo_workspace_keys::KeyProvider;

use crate::service::{ServiceConfig, ServiceError, ServiceIdentity, WorkspaceService};
use crate::workspaces::{
    DeviceKeyGroups, GroupKeys, GroupKeysAnswerFuture, KeySink, MemberDevice,
};

/// How long the service waits for the device's key service to open a group: far more than the
/// wait before the device founds the group itself
/// ([`tilo_workspace_keys::founding_wait`]) and the work a first write does beside it, so a
/// fresh workspace's first write is not refused for keys that are about to arrive.
pub const OPEN_WAIT: Duration = Duration::from_secs(30);
const _: () = assert!(OPEN_WAIT.as_secs() > tilo_workspace_keys::founding_wait().as_secs());

/// The device a service is opened for: the certificate its person issued it, and what signs as
/// it. What the service writes in a workspace this device takes part in is signed as it.
pub struct Device<'a> {
    pub cert: &'a DeviceCert,
    pub signer: &'a Arc<dyn DeviceSigner>,
}

/// Open the workspace service `device` keeps under `store`, on `endpoint`, its groups kept by
/// `groups`: one that governs the workspaces it founds where `governs`, else one that holds only
/// the workspaces the device joined. `configure` adjusts the service's configuration beyond
/// what every device's service shares.
///
/// # Errors
/// The device cannot certify the service's own key, or the service's store cannot be opened.
pub async fn open(
    store: PathBuf,
    device: Device<'_>,
    endpoint: iroh::Endpoint,
    groups: DeviceKeyGroups,
    governs: bool,
    configure: impl FnOnce(ServiceConfig) -> ServiceConfig,
) -> Result<WorkspaceService, ServiceError> {
    let identity = ServiceIdentity::beside_device(&store, device.cert, &**device.signer).await?;
    let config = configure(ServiceConfig {
        member: Some(Arc::new(MemberDevice {
            cert: device.cert.clone(),
            signer: Arc::clone(device.signer),
        })),
        ..ServiceConfig::new(store, tilo_server_protocol::WorkSetting::none())
    });
    if governs {
        WorkspaceService::open(config, identity, endpoint, Some(groups)).await
    } else {
        WorkspaceService::open_joining(config, identity, endpoint, Some(groups)).await
    }
}

/// Keys a workspace's host handed this device for what every member reads, by workspace: a
/// member copy opens with them before the device's key service takes part in the group.
pub type HandedKeys = Box<dyn Fn(&str) -> Option<key_group::ContentKeys> + Send + Sync>;

/// The device's key service as its workspace service asks it, answering first from `handed`,
/// with `sink` taking what the service's members send for the groups.
#[must_use]
pub fn key_groups(
    keys: KeyProvider,
    handed: Option<HandedKeys>,
    sink: Option<Arc<dyn KeySink>>,
) -> DeviceKeyGroups {
    DeviceKeyGroups {
        keys: Arc::new(DeviceGroupKeys { keys, handed }),
        sink,
    }
}

/// The device's keys, as its workspace service asks for a workspace's: those the workspace's
/// host handed this device, else its key service's.
struct DeviceGroupKeys {
    keys: KeyProvider,
    handed: Option<HandedKeys>,
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
}
