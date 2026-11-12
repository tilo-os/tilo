//! A device entering a workspace on its session
//! ([`tilo_relay_protocol::entering`]): through the door, which lets the
//! person in as its HTTP form did and issues the invitation, or with an
//! invitation of its own; the workspace admits the device, which proved it
//! holds its key on the session's connection, with that invitation.

use tilo_relay_protocol::directory::DoorJoin;
use tilo_relay_protocol::entering::{EnterRefusal, Entered, Entry, Through};
use tilo_relay_protocol::web::{MAX_DISPLAY_NAME_CHARS, kept};
use tilo_relay_protocol::workspaces::JoinReply;
use tilo_user_identity::DeviceCert;

use std::time::SystemTime;

use crate::handshake::{Doorway, EntryFuture};
use crate::workspaces::pages::Presented;
use crate::workspaces::{Carried, Workspaces};

/// A refusal of the join itself, after the door let the person in.
const fn refused(reason: String) -> EnterRefusal {
    EnterRefusal::Workspace(reason)
}

impl Doorway for Workspaces {
    fn enter<'a>(&'a self, cert: &'a DeviceCert, entry: Entry) -> EntryFuture<'a, Entered> {
        Box::pin(async move {
            let Entry {
                entering, access, ..
            } = entry;
            let device = cert.device_public_key;
            // A device asking again under an entry the workspace already
            // admitted (its answer was lost with its connection) is in.
            if let Ok(governed) = self.workspace(&entering.workspace).await {
                let again = {
                    let workspace = governed.lock().await;
                    (workspace.admitted_device(entering.request_id) == Some(device)).then(|| {
                        Self::admitted(&workspace, device, tilo_clock::unix_now_ns()).map_or_else(
                            |error| Entered::Refused(refused(error.to_string())),
                            entered,
                        )
                    })
                };
                if let Some(again) = again {
                    return self.with_desktop_scope(&entering.workspace, again).await;
                }
            }
            // A device that is a member already — a `tilo mcp` restarted, a
            // page opened again — enters with the workspace's own admission:
            // the door has nothing to decide for it, and an entry that asked
            // it anyway would issue an invitation into the workspace's
            // governance for someone already in, publish that to everyone
            // here and spend it, on every run.
            if entering.through == Through::Door
                && let Some(member) = self.member_already(cert, &entering.workspace).await
            {
                return self.with_desktop_scope(&entering.workspace, member).await;
            }
            let (invitation, token) = match entering.through {
                Through::Door => match self.door(cert, &entering.workspace, &entering.name).await {
                    Ok((invitation, token)) => (invitation, Some(token.into_bytes())),
                    Err(refused) => return Entered::Refused(refused),
                },
                Through::Invitation { invitation, token } => {
                    // The keys the entry asks for are the named workspace's group's: an
                    // invitation into another is not this entry's.
                    let names = tilo_peer_state::join_ledger::invitation_workspace(
                        &invitation,
                        tilo_clock::unix_now_ns(),
                    );
                    if names.as_deref().ok() != Some(entering.workspace.as_str()) {
                        return Entered::Refused(refused(
                            "the invitation is not for the workspace this device enters".to_owned(),
                        ));
                    }
                    (invitation, token)
                }
            };
            let entered = entered(
                self.redeem_on_connection(
                    Presented {
                        invitation,
                        cert: cert.clone(),
                        request_id: entering.request_id,
                    },
                    &entering.name,
                    token.as_deref(),
                )
                .await,
            );
            if !matches!(entered, Entered::Admitted { .. }) {
                return entered;
            }
            if let Some(access) = access {
                let keying = crate::timing::start();
                self.key_arrival(&entering.workspace, cert, access).await;
                crate::timing::event("entry.keys", keying, 0);
            }
            let scoping = crate::timing::start();
            let entered = self.with_desktop_scope(&entering.workspace, entered).await;
            crate::timing::event("entry.desktop_scope", scoping, 0);
            entered
        })
    }
}

impl Workspaces {
    /// Let `cert`'s person, named `name`, in through `workspace`'s door: the
    /// invitation it issued them and its one-time token, or why not.
    async fn door(
        &self,
        cert: &DeviceCert,
        workspace: &str,
        name: &str,
    ) -> Result<(Vec<u8>, String), EnterRefusal> {
        let join = DoorJoin {
            user_public_key: hex::encode(cert.user_public_key),
            display_name: kept(name, MAX_DISPLAY_NAME_CHARS),
        };
        let asking = crate::timing::start();
        let admitted = self.admit_listed(workspace, &join).await;
        crate::timing::event("entry.door", asking, 0);
        admitted
            .map(|(issued, token)| (issued.signed_invitation, token))
            .map_err(|refusal| EnterRefusal::Door {
                retry_after_secs: match refusal {
                    crate::workspaces::DoorRefusal::Full { retry_after_secs } => {
                        Some(retry_after_secs)
                    }
                    _ => None,
                },
                refused: refusal.answer_with(self.wait_list_door()),
            })
    }

    /// What `cert`'s device hears when it is a member of `workspace` already,
    /// and the door keeps nobody out of it: the workspace's own admission,
    /// with the standings its changes there are decided against brought up to
    /// date first. `None` for a device the workspace does not admit, and for
    /// one whose person the door refuses.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked until its standings are settled and the device's admission is read from it"
    )]
    async fn member_already(&self, cert: &DeviceCert, workspace: &str) -> Option<Entered> {
        let now = tilo_clock::unix_now_ns();
        let governed = self.workspace(workspace).await.ok()?;
        let mut governed = governed.lock().await;
        // The refusals the door would answer: a person banned or removed here
        // does not come back in by the admission it gave them before.
        let policy = governed
            .peer()
            .permissions(self.inner.host.cert.device_public_key, tilo_clock::wall())
            .ok()?
            .policy;
        if policy.is_blocked(&hex::encode(cert.user_public_key))
            || governed.door.refuses(cert.user_public_key, now)
        {
            return None;
        }
        // The workspace's own admission of this device, never another's: a
        // stranger holds no permissions here.
        governed
            .peer()
            .permissions(cert.device_public_key, tilo_clock::wall())
            .ok()?;
        // An entry cut off between committing an admission and settling it
        // leaves the device in governance and out of the standings this
        // workspace decides its changes against: settle before answering, as
        // a device that enters once more does.
        if !self
            .inner
            .host
            .access
            .projects_standing(governed.workspace_id(), cert.device_public_key)
            .await
        {
            self.settle(&mut governed).await.ok()?;
        }
        Some(entered(
            Self::admitted(&governed, cert.device_public_key, now).ok()?,
        ))
    }
}

impl Workspaces {
    /// Hand `workspace`'s channel participants the request `cert`'s device
    /// made in its entry, as they take every member's. The Welcome it asks
    /// for is kept for the device's subscription, whose replay writes it
    /// first ([`crate::peer::Arriving`]); the entry's answer waits neither
    /// for it nor for the request to be retained, so the participants commit
    /// it while the answer travels and the device subscribes. A request for
    /// another group than the workspace's own, by another device, or not
    /// signed by it is not handed on.
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
        if !self
            .inner
            .host
            .records
            .expect_arrival(workspace_id, device)
            .await
        {
            return;
        }
        let request = neighbour_wire::NeighbourMsg::MlsAccessRequest {
            workspace: workspace.to_owned(),
            request: Box::new(access),
            delivery: 0,
        };
        // Retaining the request is the participants' business, not the
        // answer's: the device is expected, so its replay waits for the
        // Welcome however late the request is taken.
        let workspaces = self.clone();
        let workspace = workspace.to_owned();
        tokio::spawn(async move {
            let taking = crate::timing::start();
            if let Err(error) = workspaces
                .take_channel_governance(&workspace, device, request, Carried::Unkept)
                .await
            {
                tracing::warn!(%workspace, %error, "an entry's channel request was not taken");
            }
            crate::timing::event("entry.keys.taken", taking, 0);
        });
    }

    /// `entered`, with the group that seals `workspace`'s shared desktop as
    /// its governance decides it now, and this host's signed address for the
    /// workspace, where a desktop takes it in.
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
        let (desktop_scope, host) = match self.workspace(workspace).await {
            Ok(governed) => {
                use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_RECORD};
                let route = collab_types::ChannelRoute::new(DESKTOP_APP, DESKTOP_RECORD);
                let governed = governed.lock().await;
                let scope = governed
                    .peer()
                    .governance()
                    .unit_scope(tilo_clock::wall(), &route)
                    .ok();
                let hint = self.transport_hint(&governed);
                drop(governed);
                let host = hint.ok().and_then(|hint| postcard::to_stdvec(&hint).ok());
                (scope, host)
            }
            Err(_) => (None, None),
        };
        Entered::Admitted {
            workspace: uuid,
            workspace_id,
            name,
            role,
            desktop_scope,
            host,
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
            host: None,
        },
        JoinReply::Refused(reason) => Entered::Refused(refused(reason)),
        JoinReply::Invitation { .. } => Entered::Refused(refused(
            "the workspace answered something other than an admission".to_owned(),
        )),
    }
}
