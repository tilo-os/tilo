//! What a session's device in a workspace's VM asks of this host
//! ([`tilo_actions::remote::environment_device`]): the media keys of its own
//! streams, and its own windows shared with the workspace.
//!
//! The device holds none of the workspace's keys and writes nothing of the
//! workspace's governance (`tilo_room_host::environment`). This host certified
//! it and holds the workspace's keys, so this host answers it, and only it:
//!
//! - its media keys are derived from the workspace's media channel at its
//!   current epoch, for that one sender and only the streams it publishes
//!   ([`tilo_live::frame::Stream::OWN`]), and sealed to its device key, so
//!   whatever carries the answer between here and the VM reads none of it;
//! - a window it shares is written as the workspace's governor writes a web
//!   page's share ([`tilo_room_host::workspaces::Workspaces::share_resource`]),
//!   after checking the window's id was minted under the asking device's own
//!   salt: a confined device shares its own windows and nobody else's.

use tilo_action_policy::share::ShareAccess;
use tilo_actions::ActionOutput;
use tilo_actions::remote::environment_device::{
    self, END_WINDOW_SHARE, MEDIA_KEYS, MediaKeys, SHARE_WINDOW, WindowShare,
};
use tilo_actions::remote::{
    ActionHostError, Caller, InvocationReceipt, InvokeRequest, ReceiptStatus, RemoteActionMessage,
    pending,
};
use tilo_desktop_model::ids::{device_salt_of_key, peer_salt_of};
use tilo_desktop_model::window_share::{window_node, window_share_id};
use tilo_lock::MutexRecover as _;
use tilo_user_identity::DeviceCert;

use super::HostedBrowsers;
use crate::WorkspaceId;

/// What one ask needs once it leaves the lock: who asked, in which workspace,
/// and how this host reaches that workspace's keys and governance.
struct Asked {
    device: DeviceCert,
    workspace: String,
    /// The workspaces this host governs, when it governs any: where their
    /// keys and governance are.
    workspaces: Option<tilo_room_host::workspaces::Workspaces>,
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
    ) -> Option<crate::app_serving::Routing> {
        let action = environment_device::action_of(&request.target)?.to_owned();
        let message = RemoteActionMessage::Invoke(request.clone());
        let refused = |error: ActionHostError| super::answered_with(&message, error);
        let Some(asked) = self.asking_device(workspace_id, caller) else {
            return refused(ActionHostError::NotPermitted);
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
        Some(crate::app_serving::Routing::Answer(answered))
    }

    /// The device `caller` is, when it is the device this host placed for
    /// `workspace_id`'s Browser or Linux apps, with what answering it needs.
    fn asking_device(&self, workspace_id: WorkspaceId, caller: &Caller) -> Option<Asked> {
        let device = *caller.device_public_key()?;
        let placed = |sessions: &std::sync::Mutex<std::collections::BTreeMap<WorkspaceId, super::Open>>| {
            sessions
                .lock_recover()
                .get(&workspace_id)
                .filter(|up| up.device.device_public_key == device)
                .map(|up| (up.device.clone(), up.workspace.clone()))
        };
        let (device, workspace) = placed(&self.open).or_else(|| placed(&self.apps))?;
        let workspaces = self.authority.lock_recover().as_ref()?.workspaces.clone();
        Some(Asked {
            device,
            workspace,
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
    ) -> Result<serde_json::Value, ActionHostError> {
        match action {
            MEDIA_KEYS => self.media_keys().await,
            SHARE_WINDOW | END_WINDOW_SHARE => {
                let window: WindowShare = argument_of(argument)?;
                self.share(&window, action == SHARE_WINDOW).await
            }
            _ => Err(ActionHostError::NotPermitted),
        }
    }

    /// The media keys of this device's own streams at the media channel's
    /// current epoch, sealed to it.
    async fn media_keys(&self) -> Result<serde_json::Value, ActionHostError> {
        let keys = self
            .workspaces
            .as_ref()
            .and_then(tilo_room_host::workspaces::Workspaces::device_keys)
            .ok_or_else(|| ActionHostError::Unavailable {
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
    ) -> Result<serde_json::Value, ActionHostError> {
        if !owns_window(&self.device.device_public_key, window.tab) {
            return Err(ActionHostError::NotPermitted);
        }
        let id = window_share_id(window.tab);
        let refused =
            |error: tilo_room_host::workspaces::CollaborationError| ActionHostError::Refused {
                reason: error.to_string(),
            };
        let workspaces = self
            .workspaces
            .as_ref()
            .ok_or_else(|| ActionHostError::Unavailable {
                reason: "this host governs no workspace".to_owned(),
            })?;
        let grants = if share {
            let node = window_node(&self.workspace, &window.app, window.tab);
            workspaces
                .share_resource(&self.workspace, &self.device, &id, &node, ShareAccess::View)
                .await
                .map_err(refused)?
        } else {
            workspaces
                .unshare_resource(&self.workspace, &self.device, &id)
                .await
                .map_err(refused)?
        };
        Ok(serde_json::json!({ "grants": grants }))
    }
}

/// The media keys of `device`'s own streams in `workspace`, as `keys` (this
/// host's channel keys) derive them at the media channel's current epoch,
/// sealed to `device`.
async fn lend(
    keys: &dyn tilo_room_host::workspaces::ChannelKeys,
    workspace: &str,
    device: [u8; 32],
) -> Result<serde_json::Value, ActionHostError> {
    let unavailable = |reason: String| ActionHostError::Unavailable { reason };
    let route = collab_types::ChannelRoute::new(
        collab_types::scope::MEDIA_APP,
        collab_types::scope::WORKSPACE_RECORD,
    );
    let content = keys
        .channel_keys(workspace, &route, false)
        .await
        .ok_or_else(|| unavailable("this host takes no part in the workspace's media".to_owned()))?
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
) -> Result<T, ActionHostError> {
    let invalid = |reason: String| ActionHostError::InvalidRequest { reason };
    let tilo_actions::remote::InvokeArgument::Json(map) = argument else {
        return Err(invalid("the window is named in JSON".to_owned()));
    };
    serde_json::from_value(serde_json::Value::Object(map.clone()))
        .map_err(|error| invalid(format!("not a window: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    use channel_crypto::{ChannelContentKeys, ChannelGroup, MemberKey};
    use tilo_live::frame::Stream;
    use tilo_room_host::workspaces::{ChannelKeys, ChannelKeysAnswerFuture};

    const WORKSPACE: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";

    /// A host holding the workspace's media channel, and no other.
    struct Holding(ChannelContentKeys);

    impl ChannelKeys for Holding {
        fn channel_keys<'a>(
            &'a self,
            workspace: &'a str,
            route: &'a collab_types::ChannelRoute,
            _: bool,
        ) -> ChannelKeysAnswerFuture<'a> {
            let media = collab_types::ChannelRoute::new(
                collab_types::scope::MEDIA_APP,
                collab_types::scope::WORKSPACE_RECORD,
            );
            let answer = (workspace == WORKSPACE && *route == media).then(|| Ok(self.0.clone()));
            Box::pin(std::future::ready(answer))
        }
    }

    fn media() -> ChannelContentKeys {
        let channel = collab_types::scope::Scope::workspace(WORKSPACE).channel_id();
        ChannelGroup::create(channel, MemberKey::generate("host").unwrap())
            .unwrap()
            .content_keys()
            .unwrap()
    }

    fn opened(
        answer: serde_json::Value,
        device: &ed25519_dalek::SigningKey,
    ) -> channel_crypto::SenderMediaKeys {
        let lent: MediaKeys = serde_json::from_value(answer).unwrap();
        let sender = device.verifying_key().to_bytes();
        channel_crypto::SenderMediaKeys::open_with(
            device,
            sender,
            &hex::decode(lent.sealed).unwrap(),
        )
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
            channel_crypto::SenderMediaKeys::open_with(
                &other,
                key,
                &hex::decode(lent.sealed).unwrap()
            )
            .is_err(),
            "what carries the answer to the VM reads none of it"
        );
    }

    #[tokio::test]
    async fn a_host_without_the_workspaces_media_lends_nothing() {
        let device = [5; 32];
        let refused = lend(&Holding(media()), "another-workspace", device).await;
        assert!(
            matches!(refused, Err(ActionHostError::Unavailable { .. })),
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
