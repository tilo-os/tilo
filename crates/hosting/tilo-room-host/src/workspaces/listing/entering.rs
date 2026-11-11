//! A device entering a workspace through its door on its session
//! ([`tilo_relay_protocol::entering`]): the same door and join steps its HTTP
//! form took, answered on the session's handshake.

use tilo_relay_protocol::directory::DoorJoin;
use tilo_relay_protocol::entering::{EnterRefusal, Entered, Entering};
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
                    refused: refusal.answer(),
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
        challenge: Vec<u8>,
        token: String,
        signature: [u8; 64],
        name: &'a str,
    ) -> EntryFuture<'a, Entered> {
        Box::pin(async move {
            entered(
                self.page_join(JoinStep::ProvePreAdmitted {
                    challenge,
                    signature,
                    name: name.to_owned(),
                    token: token.into_bytes(),
                })
                .await,
            )
        })
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
        },
        JoinReply::Refused(reason) => Entered::Refused(refused(reason)),
        _ => Entered::Refused(refused(
            "the workspace answered something other than an admission".to_owned(),
        )),
    }
}
