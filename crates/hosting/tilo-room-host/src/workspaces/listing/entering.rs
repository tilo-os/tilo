//! A device entering a workspace through its door on its session
//! ([`tilo_relay_protocol::entering`]): the same door and join steps its HTTP
//! form took, answered on the session's handshake.

use tilo_relay_protocol::directory::DoorJoin;
use tilo_relay_protocol::entering::{EnterProof, EnterRefusal, Entered, Entering};
use tilo_relay_protocol::web::{MAX_DISPLAY_NAME_CHARS, kept};
use tilo_relay_protocol::workspaces::{JoinReply, JoinStep};
use tilo_user_identity::DeviceCert;

use crate::handshake::{Asked, Doorway, EntryFuture};
use crate::workspaces::Workspaces;

/// A refusal of the join itself, after the door let the person in.
const fn refused(reason: String) -> EnterRefusal {
    EnterRefusal::Workspace(reason)
}

impl Doorway for Workspaces {
    fn ask<'a>(
        &'a self,
        cert: &'a DeviceCert,
        entering: &'a Entering,
    ) -> EntryFuture<'a, Result<Asked, EnterRefusal>> {
        Box::pin(async move {
            let device = cert.device_public_key;
            // A device asking again under an entry the workspace already
            // admitted (its answer was lost with its connection) is in.
            if let Ok(governed) = self.workspace(&entering.workspace).await {
                let workspace = governed.lock().await;
                if workspace.admitted_device(entering.request_id) == Some(device) {
                    let now = tilo_clock::unix_now_ns();
                    return Self::admitted(&workspace, device, now)
                        .map(|reply| Asked::In(entered(reply)))
                        .map_err(|error| refused(error.to_string()));
                }
            }
            let join = DoorJoin {
                user_public_key: hex::encode(cert.user_public_key),
                display_name: kept(&entering.name, MAX_DISPLAY_NAME_CHARS),
            };
            let (issued, token) = self
                .admit_listed(&entering.workspace, &join)
                .await
                .map_err(|refusal| EnterRefusal::Door {
                    retry_after_secs: match refusal {
                        crate::workspaces::DoorRefusal::Full { retry_after_secs } => {
                            Some(retry_after_secs)
                        }
                        _ => None,
                    },
                    refused: refusal.answer_with(self.wait_list_door()),
                })?;
            match self
                .page_join(JoinStep::Ask {
                    invitation: issued.signed_invitation,
                    device_cert: cert.clone(),
                    request_id: entering.request_id,
                })
                .await
            {
                JoinReply::Challenge { challenge } => Ok(Asked::Challenge { challenge, token }),
                JoinReply::Refused(reason) => Err(refused(reason)),
                _ => Err(refused("the workspace answered no challenge".to_owned())),
            }
        })
    }

    fn prove<'a>(
        &'a self,
        cert: &'a DeviceCert,
        entering: &'a Entering,
        challenge: Vec<u8>,
        token: String,
        proof: EnterProof,
    ) -> EntryFuture<'a, Entered> {
        Box::pin(async move {
            let entered = entered(
                self.page_join(JoinStep::ProvePreAdmitted {
                    challenge,
                    signature: proof.signature,
                    name: entering.name.clone(),
                    token: token.into_bytes(),
                })
                .await,
            );
            if !matches!(entered, Entered::Admitted { .. }) {
                return entered;
            }
            if let Some(access) = proof.access {
                self.key_arrival(&entering.workspace, cert, access).await;
            }
            self.with_desktop_scope(&entering.workspace, entered).await
        })
    }
}

/// How long an admitted device's answer waits for the Welcome its request
/// asked for: the participants commit it within tens of milliseconds, and a
/// device whose Welcome is later asks again as a member, as every member does.
const KEYS_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

impl Workspaces {
    /// Hand `workspace`'s channel participants the request `cert`'s device
    /// made in its entry, as they take every member's, and wait (at most
    /// [`KEYS_WAIT`]) for the Welcome it asks for to be durable, kept for the
    /// device's subscription to write first. A request for another group than
    /// the workspace's own, by another device, or not signed by it is not
    /// handed on.
    async fn key_arrival(
        &self,
        workspace: &str,
        cert: &DeviceCert,
        access: collab_types::MlsAccessRequestV1,
    ) {
        let device = cert.device_public_key;
        if access.device.as_bytes() != &device
            || access.channel != collab_types::workspace_scope(workspace)
            || access.verify().is_err()
        {
            tracing::debug!(%workspace, "an entry's channel request was not the device's own for the workspace's group");
            return;
        }
        let Ok(workspace_id) = tilo_relay_protocol::room_workspace_id(workspace) else {
            return;
        };
        let keyed = self
            .inner
            .host
            .records
            .expect_arrival(workspace_id, device)
            .await;
        let request = neighbour_wire::NeighbourMsg::MlsAccessRequest {
            workspace: workspace.to_owned(),
            request: Box::new(access),
            delivery: 0,
        };
        if let Err(error) = self
            .take_channel_governance(workspace, device, request)
            .await
        {
            tracing::warn!(%workspace, %error, "an entry's channel request was not taken");
            return;
        }
        if let Some(keyed) = keyed {
            let _ = tokio::time::timeout(KEYS_WAIT, keyed.notified()).await;
        }
    }

    /// `entered`, with the group that seals `workspace`'s shared desktop as
    /// its governance decides it now.
    async fn with_desktop_scope(&self, workspace: &str, entered: Entered) -> Entered {
        let Entered::Admitted {
            workspace: uuid,
            workspace_id,
            name,
            role,
            ..
        } = entered
        else {
            return entered;
        };
        let desktop_scope = match self.workspace(workspace).await {
            Ok(governed) => {
                use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_RECORD};
                let route = collab_types::ChannelRoute::new(DESKTOP_APP, DESKTOP_RECORD);
                let governed = governed.lock().await;
                governed
                    .peer()
                    .governance()
                    .unit_scope(std::time::SystemTime::now(), &route)
                    .ok()
            }
            Err(_) => None,
        };
        Entered::Admitted {
            workspace: uuid,
            workspace_id,
            name,
            role,
            desktop_scope,
        }
    }
}

/// The workspace's answer to a proved join, as the session tells it.
fn entered(reply: JoinReply) -> Entered {
    match reply {
        JoinReply::Admitted {
            workspace,
            workspace_id,
            name,
            role,
        } => Entered::Admitted {
            workspace,
            workspace_id,
            name,
            role,
            desktop_scope: None,
        },
        JoinReply::Refused(reason) => Entered::Refused(refused(reason)),
        _ => Entered::Refused(refused(
            "the workspace answered something other than an admission".to_owned(),
        )),
    }
}
