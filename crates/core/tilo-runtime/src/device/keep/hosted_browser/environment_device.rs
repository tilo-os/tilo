//! What a session's device in a workspace's VM asks of this host
//! ([`tilo_action_services::environment_device`]): the media keys of its own
//! streams, and its own windows shared with the workspace.
//!
//! The device holds none of the workspace's keys and writes nothing of the
//! workspace's governance (`tilo_workspace_service::environment`). This host certified
//! it and holds the workspace's keys, so this host answers it, and only it:
//!
//! - its media keys are derived from the workspace's media channel at its
//!   current epoch, for that one sender and only the streams it publishes
//!   ([`tilo_live::frame::Stream::OWN`]), and sealed to its device key, so
//!   whatever carries the answer between here and the VM reads none of it;
//! - a window it shares is written as this host's own person's share
//!   ([`tilo_workspace_service::workspaces::Workspaces::set_scoped_role`]),
//!   after checking the window's id was minted under the asking device's own
//!   salt: a confined device shares its own windows and nobody else's.
//!
//! What a Browser window asks for a field it fills from a vault item is answered beside these,
//! where the fill was handed to it ([`super::turn_fills`]).

use tilo_action_services::environment_device::{
    self, END_WINDOW_SHARE, MEDIA_KEYS, MediaKeys, SHARE_WINDOW, WindowShare,
};
use tilo_actions::ActionOutput;
use tilo_actions::remote::{
    ActionCallError, Caller, InvocationReceipt, InvokeRequest, ReceiptStatus, RemoteActionMessage,
    pending,
};
use tilo_desktop_model::ids::{device_salt_of_key, peer_salt_of};
use tilo_desktop_model::window_share::window_node;
use tilo_lock::MutexRecover as _;
use tilo_user_identity::DeviceCert;

use super::HostedBrowsers;
use tilo_server_protocol::WorkspaceId;

/// What one ask needs once it leaves the lock: who asked, in which workspace,
/// and how this host reaches that workspace's keys and governance.
struct Asked {
    device: DeviceCert,
    workspace: String,
    /// The workspaces this host serves, when it serves any: where their
    /// keys and governance are.
    workspaces: Option<tilo_workspace_service::workspaces::Workspaces>,
}

impl HostedBrowsers {
    /// `request` from `caller`, when it is one of the asks of an environment's
    /// device: answered here, for the device of `workspace_id`'s Browser or
    /// Linux apps session and nobody else. `None` for any other request.
    pub(super) fn environment_asked(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Option<tilo_workspace_service::app_serving::Routing> {
        let action = environment_device::action_of(&request.target)?.to_owned();
        // The text of a vault item for a field its window fills is decided where the fill was
        // handed over, and answered for the Browser's own device alone.
        if action == environment_device::USE_ITEM {
            return self.item_asked(workspace_id, caller, request);
        }
        let message = RemoteActionMessage::Invoke(request.clone());
        let refused = |error: ActionCallError| super::answered_with(&message, error);
        let Some(asked) = self.asking_device(workspace_id, caller) else {
            return refused(ActionCallError::NotPermitted);
        };
        let invocation = request.invocation;
        let argument = request.argument.clone();
        let (answer, answered) = pending();
        let runtime = tokio::runtime::Handle::try_current().ok()?;
        runtime.spawn(async move {
            let status = match asked.answer(&action, &argument).await {
                Ok(value) => ReceiptStatus::Completed {
                    output: ActionOutput {
                        value: Some(value),
                        resources: Vec::new(),
                    },
                },
                Err(error) => ReceiptStatus::Rejected { error },
            };
            answer.complete(RemoteActionMessage::Receipt(InvocationReceipt {
                invocation,
                status,
            }));
        });
        Some(tilo_workspace_service::app_serving::Routing::Answer(
            answered,
        ))
    }

    /// The device `caller` is, when it is `workspace_id`'s VM device, as the person's devices
    /// record lists and confines it to that workspace, with what answering it needs.
    fn asking_device(&self, workspace_id: WorkspaceId, caller: &Caller) -> Option<Asked> {
        let device = *caller.device_public_key()?;
        let guard = self.authority.lock_recover();
        let (certificate, workspaces) = {
            let authority = guard.as_ref()?;
            let scope = tilo_workspace_service::environment::environment_of(
                &authority.environments,
                &device,
            )?
            .placed()?;
            if scope.workspace != workspace_id {
                return None;
            }
            (
                authority.environments.certificate(&device)?,
                authority
                    .workspaces
                    .as_ref()
                    .and_then(tilo_workspace_service::workspaces::WeakWorkspaces::upgrade),
            )
        };
        drop(guard);
        Some(Asked {
            device: certificate,
            workspace: workspace_id.to_string(),
            workspaces,
        })
    }
}

impl Asked {
    /// What `action` answers for this device, or why it is refused.
    async fn answer(
        &self,
        action: &str,
        argument: &tilo_actions::remote::InvokeArgument,
    ) -> Result<serde_json::Value, ActionCallError> {
        match action {
            MEDIA_KEYS => self.media_keys().await,
            SHARE_WINDOW | END_WINDOW_SHARE => {
                let window: WindowShare = argument_of(argument)?;
                self.share(&window, action == SHARE_WINDOW).await
            }
            _ => Err(ActionCallError::NotPermitted),
        }
    }

    /// The media keys of this device's own streams at the media channel's
    /// current epoch, sealed to it.
    async fn media_keys(&self) -> Result<serde_json::Value, ActionCallError> {
        let keys = self
            .workspaces
            .as_ref()
            .and_then(tilo_workspace_service::workspaces::Workspaces::device_keys)
            .ok_or_else(|| ActionCallError::Unavailable {
                reason: "this host holds no workspace's keys".to_owned(),
            })?;
        lend(
            keys.as_ref(),
            &self.workspace,
            self.device.device_public_key,
        )
        .await
    }

    /// Share `window` with the workspace's members, or end its share, once it
    /// is a window this device minted.
    async fn share(
        &self,
        window: &WindowShare,
        share: bool,
    ) -> Result<serde_json::Value, ActionCallError> {
        if !owns_window(&self.device.device_public_key, window.tab) {
            return Err(ActionCallError::NotPermitted);
        }
        let refused = |error: tilo_workspace_service::workspaces::CollaborationError| {
            ActionCallError::Refused {
                reason: error.to_string(),
            }
        };
        let workspaces = self
            .workspaces
            .as_ref()
            .ok_or_else(|| ActionCallError::Unavailable {
                reason: "this host serves no workspace".to_owned(),
            })?;
        let node = window_node(&self.workspace, &window.app, window.tab);
        let everyone = tilo_action_policy::Grantee::role(tilo_action_policy::Role::Viewer);
        let role = share.then_some(tilo_action_policy::Role::Viewer);
        let grants = workspaces
            .set_scoped_role(&self.workspace, &self.device, (everyone, &node), role)
            .await
            .map_err(refused)?;
        Ok(serde_json::json!({ "grants": u32::from(grants) }))
    }
}

/// The media keys of `device`'s own streams in `workspace`, as `keys` (this
/// host's workspace keys) derive them at the current epoch, sealed to
/// `device`. The host opens the workspace's group when nothing sealed opened
/// it yet, as a sealed write does: a workspace whose first call comes before
/// its first sealed record still has keys to lend.
async fn lend(
    keys: &dyn tilo_workspace_service::workspaces::GroupKeys,
    workspace: &str,
    device: [u8; 32],
) -> Result<serde_json::Value, ActionCallError> {
    let unavailable = |reason: String| ActionCallError::Unavailable { reason };
    let content = keys
        .group_keys(workspace, collab_types::EVERY_MEMBER, true)
        .await
        .ok_or_else(|| unavailable("this host holds no keys of the workspace's media".to_owned()))?
        .map_err(|error| {
            unavailable(format!(
                "the workspace's media keys are not here yet: {error}"
            ))
        })?;
    let streams = tilo_live::frame::Stream::OWN.map(tilo_live::frame::Stream::tag);
    let sealed = content
        .sender_media_keys(content.epoch(), device, &streams)
        .and_then(|lent| lent.seal_to(device))
        .map_err(|error| unavailable(format!("the media keys could not be lent: {error}")))?;
    serde_json::to_value(MediaKeys {
        sealed: hex::encode(&*sealed),
    })
    .map_err(|error| unavailable(error.to_string()))
}

/// Whether the window `tab` was minted by the device `device`: its id carries
/// the salt that device's key derives, as every id a device mints does.
fn owns_window(device: &[u8; 32], tab: u64) -> bool {
    peer_salt_of(tab) == device_salt_of_key(device)
}

/// `argument` as the JSON object `T` is.
fn argument_of<T: serde::de::DeserializeOwned>(
    argument: &tilo_actions::remote::InvokeArgument,
) -> Result<T, ActionCallError> {
    let invalid = |reason: String| ActionCallError::InvalidRequest { reason };
    let tilo_actions::remote::InvokeArgument::Json(map) = argument else {
        return Err(invalid("the window is named in JSON".to_owned()));
    };
    serde_json::from_value(serde_json::Value::Object(map.clone()))
        .map_err(|error| invalid(format!("not a window: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    use key_group::{ContentKeys, KeyGroup, MemberKey};
    use tilo_live::frame::Stream;
    use tilo_workspace_service::workspaces::{GroupKeys, GroupKeysAnswerFuture};

    const WORKSPACE: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";

    /// A host holding the workspace's keys, and no other's.
    struct Holding(ContentKeys);

    impl GroupKeys for Holding {
        fn group_keys<'a>(
            &'a self,
            workspace: &'a str,
            _: &'a str,
            _: bool,
        ) -> GroupKeysAnswerFuture<'a> {
            let answer = (workspace == WORKSPACE).then(|| Ok(self.0.clone()));
            Box::pin(std::future::ready(answer))
        }
    }

    fn media() -> ContentKeys {
        let workspace = collab_types::WorkspaceId::parse(WORKSPACE).unwrap();
        KeyGroup::create(workspace, MemberKey::generate("host").unwrap())
            .unwrap()
            .content_keys()
            .unwrap()
    }

    fn opened(
        answer: serde_json::Value,
        device: &ed25519_dalek::SigningKey,
    ) -> key_group::SenderMediaKeys {
        let lent: MediaKeys = serde_json::from_value(answer).unwrap();
        let sender = device.verifying_key().to_bytes();
        key_group::SenderMediaKeys::open_with(device, sender, &hex::decode(lent.sealed).unwrap())
            .expect("the device it was lent to opens it")
    }

    #[tokio::test]
    async fn a_device_is_lent_its_own_streams_keys_and_nothing_else() {
        let content = media();
        let device = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
        let key = device.verifying_key().to_bytes();
        let answer = lend(&Holding(content.clone()), WORKSPACE, key)
            .await
            .unwrap();
        let lent = opened(answer.clone(), &device);
        assert_eq!(lent.epoch(), content.epoch());
        assert_eq!(lent.sender(), key);
        for stream in Stream::OWN {
            assert_eq!(
                lent.media_key(content.epoch(), key, stream.tag())
                    .unwrap()
                    .expose_secret(),
                content
                    .media_key(content.epoch(), key, stream.tag())
                    .unwrap()
                    .expose_secret(),
                "{stream:?} is lent as the workspace's members derive it"
            );
        }
        for stream in [
            Stream::Microphone,
            Stream::Camera,
            Stream::Screen,
            Stream::Activity,
        ] {
            assert!(
                lent.media_key(content.epoch(), key, stream.tag()).is_err(),
                "{stream:?} is no stream of its own to publish"
            );
        }
        assert!(
            lent.media_key(content.epoch(), [9; 32], Stream::Presence.tag())
                .is_err(),
            "no other sender's key is lent with it"
        );
        let other = ed25519_dalek::SigningKey::from_bytes(&[6; 32]);
        let lent: MediaKeys = serde_json::from_value(answer).unwrap();
        assert!(
            key_group::SenderMediaKeys::open_with(&other, key, &hex::decode(lent.sealed).unwrap())
                .is_err(),
            "what carries the answer to the VM reads none of it"
        );
    }

    #[tokio::test]
    async fn a_host_without_the_workspaces_media_lends_nothing() {
        let device = [5; 32];
        let refused = lend(&Holding(media()), "another-workspace", device).await;
        assert!(
            matches!(refused, Err(ActionCallError::Unavailable { .. })),
            "{refused:?}"
        );
    }

    #[test]
    fn a_window_is_its_minting_devices_own() {
        let device = [7; 32];
        let mut ids = tilo_desktop_model::ids::IdMinter::new(device_salt_of_key(&device));
        let tab = ids.mint();
        assert!(owns_window(&device, tab));
        assert!(!owns_window(&[8; 32], tab));
    }
}
