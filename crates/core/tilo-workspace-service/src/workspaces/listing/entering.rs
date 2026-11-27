//! A device entering a workspace on its session
//! ([`tilo_server_protocol::entering`]): through the entry, which lets the
//! person in as its HTTP form did and issues the invitation, or with an
//! invitation of its own; the workspace admits the device, which proved it
//! holds its key on the session's connection, with that invitation, by the
//! admission it signed itself: an entry that would let it in is answered
//! [`Entered::Sign`] first, and the device asks again with what it signed.

use tilo_server_protocol::directory::EntryJoin;
use tilo_server_protocol::entering::{EnterRefusal, Entered, Entering, Entry, Through};
use tilo_server_protocol::web::{MAX_DISPLAY_NAME_CHARS, kept};
use tilo_server_protocol::workspaces::JoinReply;
use tilo_user_identity::DeviceCert;

use super::Listed;
use crate::handshake::{Admit, EntryFuture};
use crate::workspace_access::person_device;
use crate::workspaces::mode::{Mode, PAUSED_RETRY_AFTER_SECS};
use crate::workspaces::pages::Presented;
use crate::workspaces::{Carried, Workspaces};

/// What a device whose person the workspace's member list lacks is told when it asks to
/// write its own row there.
pub(crate) const UNLISTED_PERSON_REFUSED: &str =
    "this workspace's member list doesn't have this device's person: they come in by an invitation";

/// Refuse `cert`'s device a row of its own in `hosted` unless the member list has its person,
/// as the workspace's owner or in a role.
///
/// # Errors
///
/// The member list lacks the device's person, or does not read.
pub(crate) fn refuse_unlisted_person(
    hosted: &crate::workspaces::HostedWorkspace,
    cert: &DeviceCert,
) -> Result<(), String> {
    let policy = hosted.peer().governance().member_policy()?;
    let person = hex::encode(cert.user_public_key);
    if policy.is_owner(&person) || policy.active_role(&person).is_some() {
        Ok(())
    } else {
        Err(UNLISTED_PERSON_REFUSED.to_owned())
    }
}

/// A refusal of the join itself, after the entry let the person in.
const fn refused(reason: String) -> EnterRefusal {
    EnterRefusal::Workspace(reason)
}

impl Admit for Workspaces {
    #[expect(
        clippy::too_many_lines,
        reason = "the entry's one answer: which refusals come first, and which path a device takes"
    )]
    fn enter<'a>(&'a self, cert: &'a DeviceCert, entry: Entry) -> EntryFuture<'a, Entered> {
        Box::pin(async move {
            let Entry {
                entering, access, ..
            } = entry;
            let device = cert.device_public_key;
            // A device this host placed for a Browser environment enters nothing, whichever
            // way it asks: not as a member's further device, not as one already listed, not by
            // an invitation.
            if let Err(refusal) = self
                .inner
                .host
                .environments
                .refuse_joining(&device, tilo_clock::unix_now_ns())
            {
                return Entered::Refused(refused(refusal.to_string()));
            }
            if self.removed_before_entry(cert, &entering).await {
                return Entered::Refused(refused(
                    tilo_peer_state::REMOVED_DEVICE_REFUSED.to_owned(),
                ));
            }
            // A device asking again under an entry the workspace already
            // admitted (its answer was lost with its connection) is in.
            if let Ok(hosted) = self.workspace(&entering.workspace).await {
                let again = {
                    let workspace = hosted.lock().await;
                    (workspace.admitted_device(entering.request_id) == Some(device)).then(|| {
                        Self::admitted(&workspace, device, tilo_clock::unix_now_ns()).map_or_else(
                            |error| Entered::Refused(refused(error.to_string())),
                            entered,
                        )
                    })
                };
                if let Some(again) = again {
                    return self.with_host(&entering.workspace, again).await;
                }
            }
            // A device that is a member already — a `tilo mcp` restarted, a
            // page opened again — enters with the workspace's own admission:
            // the entry has nothing to decide for it, and an entry that asked
            // it anyway would issue an invitation into the workspace's
            // governance for someone already in, publish that to everyone
            // here and spend it, on every run.
            if entering.through == Through::Entry
                && let Some(member) = self.member_already(cert, &entering.workspace).await
            {
                return self.with_host(&entering.workspace, member).await;
            }
            // A further device of someone the workspace has in it — a second
            // tab, a new computer of theirs — is theirs, not a newcomer's:
            // it comes in at the part its person holds, whatever the entry
            // says to strangers, with no invitation.
            let member_device = match entering.through {
                Through::Entry => {
                    self.member_device(cert, &entering.workspace, &entering.admission)
                        .await
                }
                Through::Invitation { .. } => None,
            };
            // The operator's kill switch: a paused host admits nobody new. A device of someone it
            // already has in it is not new, and enters as it does at any other time. The person is
            // told the wait a host at its bound asks for, in the mode's own words.
            if member_device.is_none()
                && let Some(refusal) = self.paused_entry_refusal()
            {
                return Entered::Refused(refusal);
            }
            let entered = match (member_device, entering.through) {
                (Some(entered), _) => entered,
                (None, Through::Entry) => {
                    self.entered_by_entry(
                        cert,
                        (
                            &entering.workspace,
                            &entering.name,
                            entering.request_id,
                            &entering.admission,
                        ),
                    )
                    .await
                }
                (None, Through::Invitation { invitation, token }) => {
                    // The keys the entry asks for are the named workspace's group's: an
                    // invitation into another is not this entry's.
                    let names = tilo_peer_state::join_log::invitation_workspace(
                        &invitation,
                        tilo_clock::unix_now_ns(),
                    );
                    if names.ok().map(|workspace| workspace.to_string()).as_deref()
                        != Some(entering.workspace.as_str())
                    {
                        return Entered::Refused(refused(
                            "the invitation is not for the workspace this device enters".to_owned(),
                        ));
                    }
                    if entering.admission.is_empty() {
                        let presented = Presented {
                            invitation,
                            cert: cert.clone(),
                            request_id: entering.request_id,
                        };
                        if let Err(refusal) = self.redeemable(&entering.workspace, &presented).await
                        {
                            return Entered::Refused(refused(refusal));
                        }
                        let through = Through::Invitation {
                            invitation: presented.invitation,
                            token,
                        };
                        return self.sign_on(&entering.workspace, through).await;
                    }
                    entered(
                        self.redeem_on_connection(
                            Presented {
                                invitation,
                                cert: cert.clone(),
                                request_id: entering.request_id,
                            },
                            entering.admission,
                            &entering.name,
                            token.as_deref(),
                        )
                        .await,
                    )
                }
            };
            if !matches!(entered, Entered::Admitted { .. }) {
                return entered;
            }
            if let Some(access) = access {
                self.key_arrival(&entering.workspace, cert, access).await;
            }
            self.with_host(&entering.workspace, entered).await
        })
    }
}

impl Workspaces {
    /// What a device that asks `workspace`'s entry hears: the workspace lets
    /// it sign its own admission under the entry, hands it an invitation the
    /// workspace holds for it, or takes the admission it signed and lets it
    /// in. No host signs anything for it.
    async fn entered_by_entry(
        &self,
        cert: &DeviceCert,
        (workspace, name, request_id, admission): (&str, &str, [u8; 32], &[Vec<u8>]),
    ) -> Entered {
        if admission.is_empty() {
            return match self.entry(cert, workspace, name).await {
                Ok(Listed::Open) => self.sign_on(workspace, Through::Entry).await,
                Ok(Listed::Invited(issued)) => {
                    let through = Through::Invitation {
                        invitation: issued.signed_invitation,
                        token: None,
                    };
                    self.sign_on(workspace, through).await
                }
                Err(refused) => Entered::Refused(refused),
            };
        }
        entered(
            self.admit_entered(cert, workspace, request_id, admission)
                .await,
        )
    }

    /// Whether `workspace` lets `presented`'s device redeem its invitation, as
    /// it stands now: the check the admission the device signs meets when it
    /// comes back ([`Presented::redeem`]), made before the device signs. An
    /// invitation another person holds, one spent under another request or
    /// withdrawn is the workspace's refusal, said once and final, never a
    /// failure of the device's own signing that it asks again after.
    async fn redeemable(&self, workspace: &str, presented: &Presented) -> Result<(), String> {
        let hosted = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let hosted = hosted.lock().await;
        presented
            .redeem(&hosted, self.device_cert(), tilo_clock::unix_now_ns())
            .map(drop)
            .map_err(|error| error.to_string())
    }

    /// What a device that may come into `workspace` is answered before it
    /// signs its own admission: the workspace's member list it writes it on,
    /// and how to come back with it (`through`).
    async fn sign_on(&self, workspace: &str, through: Through) -> Entered {
        use tilo_peer_state::join_log::GovernanceMessages as _;
        let governance = match self.workspace(workspace).await {
            Ok(hosted) => hosted.lock().await.peer().governance_messages(),
            Err(error) => Err(error.to_string()),
        };
        match governance {
            Ok(governance) => Entered::Sign {
                governance,
                through,
            },
            Err(reason) => Entered::Refused(refused(reason)),
        }
    }

    /// Let `cert`'s person, named `name`, in through `workspace`'s entry: the
    /// invitation it issued them and its one-time token, or why not.
    async fn entry(
        &self,
        cert: &DeviceCert,
        workspace: &str,
        name: &str,
    ) -> Result<super::Listed, EnterRefusal> {
        let wait_list = self.waiting_offer(workspace).await;
        // A key acting for someone the workspace has in it is no device of
        // theirs: the entry would hand it its person's part, and its
        // redemption would hold it unbounded. It comes in by an invitation,
        // which records it within its certificate's scope.
        if !person_device(cert)
            && self
                .member_role(workspace, cert.user_public_key)
                .await
                .is_some()
        {
            return Err(EnterRefusal::Entry {
                retry_after_secs: None,
                refused: crate::workspaces::EntryRefusal::Refused(
                    "an agent's key comes into a workspace by an invitation, within its certificate's scope, not through the entry"
                        .to_owned(),
                )
                .answer_with(wait_list.as_ref()),
            });
        }
        let join = EntryJoin {
            user_public_key: hex::encode(cert.user_public_key),
            display_name: kept(name, MAX_DISPLAY_NAME_CHARS),
        };
        let admitted = self.admit_listed(workspace, &join).await;
        admitted.map_err(|refusal| EnterRefusal::Entry {
            retry_after_secs: match refusal {
                crate::workspaces::EntryRefusal::Full { retry_after_secs } => {
                    Some(retry_after_secs)
                }
                _ => None,
            },
            refused: refusal.answer_with(wait_list.as_ref()),
        })
    }

    /// Whether `entering` is one the removed `cert` may not make: a device the
    /// workspace met and took off its member list comes back by a deliberate
    /// admission — the workspace handed to it again, a ticket or an invitation
    /// — never by presenting the certificate the workspace still holds. An
    /// invitation is that deliberate admission, so the entry it comes through
    /// decides for itself.
    async fn removed_before_entry(&self, cert: &DeviceCert, entering: &Entering) -> bool {
        entering.through == Through::Entry
            && match self.workspace(&entering.workspace).await {
                Ok(hosted) => {
                    let hosted = hosted.lock().await;
                    hosted.peer().governance().removed_from_member_list(cert)
                }
                Err(_) => false,
            }
    }

    /// The refusal a host that admits nobody new gives, in its mode's own
    /// words: a host paused or watch-only admits none. `None` while it admits
    /// joins.
    fn paused_entry_refusal(&self) -> Option<EnterRefusal> {
        let mode = self.mode();
        if mode.allows_joins() {
            return None;
        }
        let said = if mode == Mode::BackSoon {
            "the host is paused; back soon"
        } else {
            "the host is watch-only right now; come back soon"
        };
        Some(EnterRefusal::Entry {
            refused: tilo_server_protocol::directory::EntryRefused {
                code: tilo_server_protocol::directory::EntryRefusalCode::Full,
                error: said.to_owned(),
                ..Default::default()
            },
            retry_after_secs: Some(PAUSED_RETRY_AFTER_SECS),
        })
    }

    /// What `cert`'s device hears when it is a member of `workspace` already,
    /// and the entry keeps nobody out of it: the workspace's own admission,
    /// with what devices may do there, which its changes are decided against, brought up to
    /// date first. `None` for a device the workspace does not admit, and for
    /// one whose person the entry refuses.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked until its device_access are settled and the device's admission is read from it"
    )]
    async fn member_already(&self, cert: &DeviceCert, workspace: &str) -> Option<Entered> {
        let now = tilo_clock::unix_now_ns();
        let hosted = self.workspace(workspace).await.ok()?;
        let mut hosted = hosted.lock().await;
        // The refusals the entry would answer: a person banned or removed here
        // does not come back in by the admission it gave them before.
        // Read from the workspace's governance, not through this host's own
        // permissions: a server that keeps another person's workspace is no
        // member of it, and lets its members in all the same.
        let policy = hosted.peer().governance().member_policy().ok()?;
        if policy.is_blocked(&hex::encode(cert.user_public_key))
            || hosted.removals.refuses(cert.user_public_key, now)
        {
            return None;
        }
        // The workspace's own admission of this device, never another's: a
        // stranger holds no permissions here.
        hosted
            .peer()
            .permissions(cert.device_public_key, tilo_clock::wall())
            .ok()?;
        // An entry cut off between committing an admission and settling it
        // leaves the device in governance and out of the device_access this
        // workspace decides its changes against: settle before answering, as
        // a device that enters once more does.
        if !self
            .decides_for(hosted.workspace_id(), cert.device_public_key)
            .await
        {
            self.settle(&mut hosted).await.ok()?;
        }
        Some(entered(
            Self::admitted(&hosted, cert.device_public_key, now).ok()?,
        ))
    }

    /// Take `admission`, the row `cert`'s device signed for itself on the
    /// member list ([`tilo_peer_state::own_admission`]), into `hosted`, as
    /// every member takes it: judged under its author, the device; this host
    /// carries it and writes nothing of its own. Settled, so the device is
    /// admitted here from now on.
    ///
    /// The row reaches the workspace's other holders and members as the
    /// device's own change, carried before anything this host writes after
    /// it: this host's later changes build on it, and a holder that lacked it
    /// would ask for this host's whole state, in which every device's row
    /// arrives at once, and each device's row reads as written by the others.
    ///
    /// A device writes its own row only for a person the signed member list
    /// already has: its certificate comes from that person's root, and every
    /// receiver judges the row by the role the list gives them. A server
    /// holding the workspace for someone else carries a member's further
    /// device in this way, and refuses a device whose person the list lacks:
    /// who comes in is the member list's and its owners' to decide.
    ///
    /// # Errors
    ///
    /// The device's person is not on the member list, the row does not let
    /// the device in, or the workspace could not keep or carry it.
    pub(crate) async fn take_own_row(
        &self,
        hosted: &mut crate::workspaces::HostedWorkspace,
        cert: &DeviceCert,
        admission: &[Vec<u8>],
        now: u64,
    ) -> Result<(), String> {
        refuse_unlisted_person(hosted, cert)?;
        let key = self.inner.host.key.clone();
        let mut candidate = hosted.peer().clone();
        if !candidate.admit_own(
            cert,
            admission.to_vec(),
            crate::workspaces::system_time(now),
        )? {
            return Err("the device's own row does not let it in".to_owned());
        }
        let row = neighbour_wire::NeighbourMsg::SyncResp {
            changes: admission.to_vec(),
            workspace: candidate.workspace_id().to_owned(),
        };
        let state = hosted.collaboration().clone();
        hosted.keep(
            candidate,
            &key,
            crate::workspaces::system_time(now),
            |_, _| Ok(state),
        )?;
        let body = postcard::to_stdvec(&row).map_err(|error| error.to_string())?;
        let workspace = hosted.workspace_id();
        self.inner
            .records
            .carry(
                workspace,
                cert.device_public_key,
                &crate::workspace_documents::HostCommit { body },
            )
            .map_err(|error| format!("the device's own row was not carried: {error}"))?;
        // A member copy serves no link the row could go out on: it goes to the device serving
        // the workspace as the copy's other writes do. Carried alone, it reached that device only
        // with the catch-up of a link made after it, never on a link already up.
        if self.member_link(workspace).is_some() {
            self.forward_member_document(workspace, row);
        }
        self.settle(hosted).await
    }

    /// Take `admission`, the changes `cert`'s device signed for itself
    /// through `workspace`'s entry, as every member takes them: judged under
    /// their author, the device, while this host carries them and signs
    /// nothing of its own. The entry the guest named must still be the one
    /// the workspace records, so a close or an invitation-only change since
    /// it looked refuses it here; an admission already in the history is
    /// judged by the entry its own place held, never by this one.
    ///
    /// The host's answer: the workspace's admission, or why it refused.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked from reading the entry until its device is committed and settled"
    )]
    pub async fn admit_entered(
        &self,
        cert: &DeviceCert,
        workspace: &str,
        request_id: [u8; 32],
        admission: &[Vec<u8>],
    ) -> JoinReply {
        let refused = |reason: String| JoinReply::Refused(reason);
        if !crate::workspace_access::person_device(cert) {
            return refused(
                "an agent's key comes into a workspace by an invitation, within its certificate's scope, not through the entry"
                    .to_owned(),
            );
        }
        let now = tilo_clock::unix_now_ns();
        let Ok(state) = self.workspace(workspace).await else {
            return refused("this host holds no such workspace".to_owned());
        };
        let mut hosted = state.lock().await;
        if hosted.admitted_device(request_id) == Some(cert.device_public_key) {
            return Self::admitted(&hosted, cert.device_public_key, now)
                .unwrap_or_else(|error| refused(error.to_string()));
        }
        let entry = hosted.peer().governance().workspace_entry();
        let Some(role) = entry.open_role() else {
            return refused("this workspace's entry does not take people who ask".to_owned());
        };
        let Ok(digest) = tilo_peer_state::entry_digest(&entry) else {
            return refused("the workspace's entry does not read".to_owned());
        };
        if hosted
            .peer()
            .governance()
            .member_policy()
            .is_ok_and(|policy| policy.is_blocked(&hex::encode(cert.user_public_key)))
        {
            return refused("this person is banned from the workspace".to_owned());
        }
        if hosted.removals.refuses(cert.user_public_key, now)
            || hosted.peer().governance().removed_from_member_list(cert)
        {
            return refused(tilo_peer_state::REMOVED_DEVICE_REFUSED.to_owned());
        }
        let key = self.inner.host.key.clone();
        let mut candidate = hosted.peer().clone();
        if let Err(error) = candidate.admit_own(
            cert,
            admission.to_vec(),
            crate::workspaces::system_time(now),
        ) {
            return refused(error);
        }
        let mut collaboration = hosted.collaboration().clone();
        {
            let host = hosted.hosted(self.device_cert());
            if let Err(error) = collaboration.completed_entry(&host, cert, request_id, digest, role)
            {
                return refused(error.to_string());
            }
        }
        if let Err(error) = hosted.keep(
            candidate,
            &key,
            crate::workspaces::system_time(now),
            |_, _| Ok(collaboration),
        ) {
            return refused(error);
        }
        if let Err(error) = self.settle(&mut hosted).await {
            return refused(error);
        }
        Self::admitted(&hosted, cert.device_public_key, now)
            .unwrap_or_else(|error| refused(error.to_string()))
    }

    /// Admit `cert`'s device to `workspace` as a device of its person, who
    /// is a member there already, by `admission`, the row the device signed
    /// itself ([`tilo_peer_state::own_admission::sign_entry`]): the workspace
    /// lists the device under them, so it holds exactly the part its person
    /// holds, and the answer waits until that is retained, as a redemption's
    /// does. A device the workspace does not list yet, or under another
    /// certificate, that sent no row is answered [`Entered::Sign`]. Membership
    /// follows the person, not one device key: a browser tab is a device of
    /// its own, and every further one its person opens comes in this way.
    ///
    /// `None` when this is not for it, and the entry decides as it does for
    /// anyone: a key acting for the person rather than a device of theirs
    /// (an agent's grant), a person the workspace does not have in it, or
    /// one it banned or removed within the day.
    async fn member_device(
        &self,
        cert: &DeviceCert,
        workspace: &str,
        admission: &[Vec<u8>],
    ) -> Option<Entered> {
        if !person_device(cert) {
            return None;
        }
        let now = tilo_clock::unix_now_ns();
        if let Err(refusal) = self
            .inner
            .host
            .environments
            .refuse_joining(&cert.device_public_key, now)
        {
            return Some(Entered::Refused(refused(refusal.to_string())));
        }
        let hosted = self.workspace(workspace).await.ok()?;
        let reply = {
            let mut hosted = hosted.lock().await;
            let policy = hosted.peer().governance().member_policy().ok()?;
            let person = hex::encode(cert.user_public_key);
            // A workspace of the person's own names them its owner from the
            // moment it is made, so a device of theirs comes in before any
            // of theirs has entered it.
            if policy.is_blocked(&person)
                || hosted.removals.refuses(cert.user_public_key, now)
                || policy.active_role(&person).is_none()
            {
                return None;
            }
            // A device the workspace met and took off its member list comes back
            // by a deliberate admission, never by presenting the certificate
            // the workspace still holds: it is told so in words.
            if hosted.peer().governance().removed_from_member_list(cert) {
                return Some(Entered::Refused(refused(
                    tilo_peer_state::REMOVED_DEVICE_REFUSED.to_owned(),
                )));
            }
            let listed = hosted.peer().certificates().contains(cert);
            if !listed {
                if admission.is_empty() {
                    drop(hosted);
                    return Some(self.sign_on(workspace, Through::Entry).await);
                }
                if let Err(error) = self.take_own_row(&mut hosted, cert, admission, now).await {
                    return Some(Entered::Refused(refused(error)));
                }
                tracing::info!(
                    %workspace,
                    person = %person,
                    device = %hex::encode(cert.device_public_key),
                    outcome = "member device admitted",
                    "a member's further device came in through the entry by its own row"
                );
            }
            Self::admitted(&hosted, cert.device_public_key, now)
                .unwrap_or_else(|error| JoinReply::Refused(error.to_string()))
        };
        Some(entered(reply))
    }
}

impl Workspaces {
    /// Hand `workspace`'s channel key holders the request `cert`'s device
    /// made in its entry, as they take every member's. The Welcome it asks
    /// for is kept for the device's subscription, whose replay writes it
    /// first; the entry's answer waits neither
    /// for it nor for the request to be retained, so the key holders commit
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
            || access.workspace.to_string() != workspace
            || access.verify().is_err()
        {
            tracing::debug!(%workspace, "an entry's channel request was not the device's own for the workspace's group");
            return;
        }
        if self.workspace(workspace).await.is_err() {
            return;
        }
        let request = neighbour_wire::NeighbourMsg::MlsAccessRequest {
            workspace: workspace.to_owned(),
            request: Box::new(access),
            delivery: 0,
        };
        // Retaining the request is the key holders' business, not the
        // answer's: the device is expected, so its replay waits for the
        // Welcome however late the request is taken.
        let workspaces = self.clone();
        let workspace = workspace.to_owned();
        tokio::spawn(async move {
            if let Err(error) = workspaces
                .take_governance(&workspace, device, request, Carried::Unkept)
                .await
            {
                tracing::warn!(%workspace, %error, "an entry's channel request was not taken");
            }
        });
    }

    /// `entered`, with this host's signed address for the workspace, where a
    /// desktop takes it in.
    async fn with_host(&self, workspace: &str, entered: Entered) -> Entered {
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
        let host = match self.workspace(workspace).await {
            Ok(hosted) => {
                let hint = self.transport_hint(&*hosted.lock().await);
                hint.ok().and_then(|hint| postcard::to_stdvec(&hint).ok())
            }
            Err(_) => None,
        };
        Entered::Admitted {
            workspace: uuid,
            workspace_id,
            name,
            role,
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
            host: None,
        },
        JoinReply::Refused(reason) => Entered::Refused(refused(reason)),
        JoinReply::Invitation { .. } => Entered::Refused(refused(
            "the workspace answered something other than an admission".to_owned(),
        )),
    }
}

impl Workspaces {
    /// `cert`'s device asking for `entry` here, as a device does on its session: where the host
    /// answers that it may come in once it signs its own admission, `signer` (its key) signs it
    /// ([`tilo_peer_state::own_admission::sign_entry`]) and it asks again. The host's last
    /// answer.
    ///
    /// # Errors
    /// The key does not sign what the host answered.
    pub async fn enter_signed_by(
        &self,
        (signer, cert): (&dyn collab_types::signer::DeviceSigner, &DeviceCert),
        mut entry: Entry,
    ) -> Result<Entered, String> {
        let answered = Admit::enter(self, cert, entry.clone()).await;
        let Entered::Sign {
            governance,
            through,
        } = answered
        else {
            return Ok(answered);
        };
        let invitation = match &through {
            Through::Invitation { invitation, .. } => Some(invitation.as_slice()),
            Through::Entry => None,
        };
        let entering = &entry.entering;
        let admission = tilo_peer_state::own_admission::sign_entry(
            &entering.workspace,
            (&governance, &[]),
            (invitation, entering.request_id),
            Some(entering.name.as_str()),
            (cert, signer),
            tilo_clock::wall(),
        )
        .await?;
        entry.entering.through = through;
        entry.entering.admission = admission;
        Ok(Admit::enter(self, cert, entry).await)
    }

    /// Bring the key `cert` certifies for an agent of its person into `workspace`, named `name`,
    /// as every key of theirs comes into one: by an invitation this host issues its person, as
    /// it answers a key asking for the workspace it holds, which the key redeems with the
    /// admission it signs itself (`signer`). The workspace records it as that agent, within its
    /// certificate's scope. A key already listed there is let in as it is.
    ///
    /// # Errors
    /// The key's scope does not reach the workspace, this host does not invite its person
    /// there, the workspace does not let the key in, or the key does not sign.
    pub async fn enter_agent_key(
        &self,
        (signer, cert): (&dyn collab_types::signer::DeviceSigner, &DeviceCert),
        workspace: &str,
        name: &str,
    ) -> Result<(), String> {
        if !cert
            .capabilities
            .agent
            .as_ref()
            .is_some_and(|scope| scope.reaches(workspace))
        {
            return Err("the agent's key is not granted this workspace".to_owned());
        }
        // The key comes in at its person's own place, an Editor's at most, as a device of
        // theirs entering is invited: never above them, and for no one the workspace lacks. A
        // redemption by a person the list has adds the key and leaves their place as it is.
        let role = match self.member_role(workspace, cert.user_public_key).await {
            Some(
                role @ (tilo_peer_state::join::Role::Viewer
                | tilo_peer_state::join::Role::Commenter),
            ) => role,
            Some(_) => tilo_peer_state::join::Role::Editor,
            None => return Err("the agent's person is not in this workspace".to_owned()),
        };
        let invitation = self
            .issue(
                workspace,
                &self.inner.host.cert,
                crate::workspaces::IssueInvitationRequest {
                    request_id: rand::random(),
                    role,
                    recipient_user_public_key: Some(cert.user_public_key),
                    invitee_name: None,
                    inviter_name: None,
                    expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        let entry = Entry {
            entering: Entering {
                workspace: workspace.to_owned(),
                name: name.to_owned(),
                request_id: rand::random(),
                through: Through::Invitation {
                    invitation: invitation.signed_invitation,
                    token: None,
                },
                admission: Vec::new(),
            },
            access: None,
            proof: [0; 64],
        };
        match self.enter_signed_by((signer, cert), entry).await? {
            Entered::Admitted { .. } => Ok(()),
            refused => Err(format!(
                "the workspace did not let the agent's key in: {refused:?}"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::SigningKey;
    use tilo_server_protocol::directory::{EntryRefusalCode, Listing};
    use tilo_server_protocol::entering::{EnterRefusal, Entered, Entering, Entry, Through};
    use tilo_server_protocol::workspaces::RemoveMemberRequest;
    use tilo_server_protocol::workspaces::{JoinReply, Role};
    use tilo_user_identity::{AgentScope, DeviceCapabilities, DeviceCert, sign_device_cert};

    use crate::handshake::Admit;
    use crate::workspaces::fixtures::{hosting_device, signed_pre_admission_as, test_host};
    use crate::workspaces::mode::HostMode;
    use crate::workspaces::{PreAdmitRequest, Workspaces};

    /// A device `root` certifies with `capabilities`, as the person's own
    /// root signs each tab or computer of theirs.
    fn device_of(root: &SigningKey, capabilities: DeviceCapabilities) -> (SigningKey, DeviceCert) {
        let key = SigningKey::from_bytes(&rand::random());
        let now = tilo_clock::unix_now_ns();
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "tab".into(),
            capabilities,
            issued_at_ns: now.saturating_sub(1_000_000_000),
            expires_at_ns: Some(now.saturating_add(3_600_000_000_000)),
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, root).unwrap();
        (key, cert)
    }

    /// `cert`'s device asks to come into `workspace` through its entry, on a
    /// session whose proof the handshake checked.
    async fn through_the_entry(
        workspaces: &Workspaces,
        workspace: &str,
        (key, cert): (&SigningKey, &DeviceCert),
    ) -> Entered {
        workspaces
            .enter_signed(
                (key, cert),
                Entry {
                    entering: Entering {
                        workspace: workspace.to_owned(),
                        name: "Ada".into(),
                        request_id: rand::random(),
                        through: Through::Entry,
                        admission: Vec::new(),
                    },
                    access: None,
                    proof: [0; 64],
                },
            )
            .await
    }

    /// Make `root`'s person a member of `workspace` at `role` with a first
    /// device of theirs, by an invitation its owner pre-admitted them with.
    async fn member_at(
        workspaces: &Workspaces,
        owner: &DeviceCert,
        workspace: &str,
        root: &SigningKey,
        role: Role,
    ) {
        let (first_key, first) = device_of(root, DeviceCapabilities::member());
        let token: [u8; 16] = rand::random();
        let issued = workspaces
            .pre_admit(
                owner,
                PreAdmitRequest {
                    request_id: rand::random(),
                    grant: signed_pre_admission_as(workspace, &token, role),
                    recipient_user_public_key: Some(first.user_public_key),
                    inviter_name: None,
                },
            )
            .await
            .unwrap();
        let reply = workspaces
            .admit_proven(
                issued.signed_invitation,
                (first, &first_key),
                rand::random(),
                "Ada",
                Some(&token),
            )
            .await;
        assert!(
            matches!(reply, JoinReply::Admitted { role: admitted, .. } if admitted == role),
            "{reply:?}"
        );
    }

    async fn role_of(workspaces: &Workspaces, workspace: &str, person: [u8; 32]) -> Option<String> {
        workspaces
            .summary(workspace)
            .await
            .unwrap()
            .members
            .iter()
            .find(|member| member.root() == Some(person) && !member.removed)
            .map(|member| format!("{:?}", member.role))
    }

    /// A person a workspace has in it brings in every further device their
    /// root certifies — a second tab is a device of its own — through the
    /// entry of a workspace that takes strangers by invitation only, and of
    /// one that is not listed at all, at the part they hold there: a
    /// Moderator stays a Moderator, never the listing's default. A stranger
    /// is still turned away from both.
    #[tokio::test]
    async fn a_members_further_device_enters_through_the_entry_at_their_part() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = hosting_device();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let closed = workspaces.create("Lounge").await.unwrap().workspace;
        workspaces
            .publish(
                &closed,
                &owner,
                Listing {
                    description: String::new(),
                    default_role: Role::Viewer,
                },
            )
            .await
            .unwrap();
        workspaces
            .settle_entry(&closed, &owner, |entry| entry.invitation_only = true)
            .await
            .unwrap();
        let unlisted = workspaces.create("Team").await.unwrap().workspace;

        let ada = SigningKey::from_bytes(&rand::random());
        member_at(workspaces, &owner, &closed, &ada, Role::Moderator).await;
        member_at(workspaces, &owner, &unlisted, &ada, Role::Commenter).await;
        let before = role_of(workspaces, &closed, ada.verifying_key().to_bytes()).await;

        for (workspace, role) in [(&closed, Role::Moderator), (&unlisted, Role::Commenter)] {
            let (tab_key, tab) = device_of(&ada, DeviceCapabilities::member());
            let entered = through_the_entry(workspaces, workspace, (&tab_key, &tab)).await;
            assert!(
                matches!(entered, Entered::Admitted { role: admitted, .. } if admitted == role),
                "{workspace}: {entered:?}"
            );
            // In now, the device enters again as a member's.
            let again = through_the_entry(workspaces, workspace, (&tab_key, &tab)).await;
            assert!(
                matches!(again, Entered::Admitted { role: admitted, .. } if admitted == role),
                "{workspace} again: {again:?}"
            );
        }
        assert_eq!(
            role_of(workspaces, &closed, ada.verifying_key().to_bytes()).await,
            before,
            "the person's part is untouched"
        );

        for workspace in [&closed, &unlisted] {
            let (stranger_key, stranger) = device_of(
                &SigningKey::from_bytes(&rand::random()),
                DeviceCapabilities::member(),
            );
            let entered =
                through_the_entry(workspaces, workspace, (&stranger_key, &stranger)).await;
            assert!(
                matches!(entered, Entered::Refused(_)),
                "{workspace}: {entered:?}"
            );
        }
        server.shutdown().await;
    }

    /// The entry itself, asked for a person an invitation-only workspace has
    /// in it, answers a further device of theirs [`Entered::Sign`]: under the
    /// entry rather than an invitation, the row the device writes itself, which
    /// lets it in at the part its person holds without changing it.
    #[tokio::test]
    async fn an_invitation_only_entry_lets_a_member_in_at_their_part() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = hosting_device();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Lounge").await.unwrap().workspace;
        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: String::new(),
                    default_role: Role::Viewer,
                },
            )
            .await
            .unwrap();
        workspaces
            .settle_entry(&workspace, &owner, |entry| entry.invitation_only = true)
            .await
            .unwrap();
        let ada = SigningKey::from_bytes(&rand::random());
        member_at(workspaces, &owner, &workspace, &ada, Role::Moderator).await;
        let before = role_of(workspaces, &workspace, ada.verifying_key().to_bytes()).await;

        let (tab_key, tab) = device_of(&ada, DeviceCapabilities::member());
        let request_id = rand::random();
        let asked = Entry {
            entering: Entering {
                workspace: workspace.clone(),
                name: "Ada".into(),
                request_id,
                through: Through::Entry,
                admission: Vec::new(),
            },
            access: None,
            proof: [0; 64],
        };
        let Entered::Sign {
            governance,
            through,
        } = Admit::enter(workspaces, &tab, asked).await
        else {
            panic!("the entry lets a further device of a member sign its own row");
        };
        assert_eq!(
            through,
            Through::Entry,
            "a member's further device signs under the entry, not by an invitation"
        );
        let admission = tilo_peer_state::own_admission::sign_entry(
            &workspace,
            (&governance, &[]),
            (None, request_id),
            Some("Ada"),
            (&tab, &tab_key),
            tilo_clock::wall(),
        )
        .await
        .unwrap();
        let reply = Admit::enter(
            workspaces,
            &tab,
            Entry {
                entering: Entering {
                    workspace: workspace.clone(),
                    name: "Ada".into(),
                    request_id,
                    through,
                    admission,
                },
                access: None,
                proof: [0; 64],
            },
        )
        .await;
        assert!(
            matches!(
                reply,
                Entered::Admitted {
                    role: Role::Moderator,
                    ..
                }
            ),
            "{reply:?}"
        );
        assert_eq!(
            role_of(workspaces, &workspace, ada.verifying_key().to_bytes()).await,
            before
        );
        server.shutdown().await;
    }

    /// A person the workspace banned, or removed, brings no new device in
    /// through the entry; nor does a key acting for a member, an agent's grant,
    /// which comes in by its delegation and never as the person.
    #[tokio::test]
    async fn a_banned_or_removed_persons_device_and_a_members_agent_stay_out() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = hosting_device();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Team").await.unwrap().workspace;
        let banned = SigningKey::from_bytes(&rand::random());
        let removed = SigningKey::from_bytes(&rand::random());
        let ada = SigningKey::from_bytes(&rand::random());
        for root in [&banned, &removed, &ada] {
            member_at(workspaces, &owner, &workspace, root, Role::Editor).await;
        }
        workspaces
            .ban_member(
                &workspace,
                &owner,
                RemoveMemberRequest {
                    request_id: [7; 32],
                    user_public_key: banned.verifying_key().to_bytes(),
                },
            )
            .await
            .unwrap();
        workspaces
            .remove_member(
                &workspace,
                &owner,
                RemoveMemberRequest {
                    request_id: [8; 32],
                    user_public_key: removed.verifying_key().to_bytes(),
                },
            )
            .await
            .unwrap();
        for (who, root) in [("banned", &banned), ("removed", &removed)] {
            let (tab_key, tab) = device_of(root, DeviceCapabilities::member());
            let entered = through_the_entry(workspaces, &workspace, (&tab_key, &tab)).await;
            assert!(matches!(entered, Entered::Refused(_)), "{who}: {entered:?}");
        }

        let (agent_key, agent) = device_of(
            &ada,
            DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
        );
        let entered = through_the_entry(workspaces, &workspace, (&agent_key, &agent)).await;
        assert!(matches!(entered, Entered::Refused(_)), "{entered:?}");
        let held = workspaces.workspace(&workspace).await.unwrap();
        assert!(
            held.lock()
                .await
                .peer()
                .permissions(agent.device_public_key, tilo_clock::wall())
                .is_err(),
            "the agent holds nothing here"
        );
        server.shutdown().await;
    }

    /// While the operator's switch pauses the host, nobody new is admitted, and the entry answers
    /// the way a host at its bound does: the person comes back after a wait. A device of someone
    /// the workspace already has in it is not new and enters at their part; back to normal, the
    /// next newcomer walks in.
    #[tokio::test]
    async fn a_paused_host_admits_nobody_new_while_a_members_device_enters() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = hosting_device();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Lounge").await.unwrap().workspace;
        workspaces
            .publish(
                &workspace,
                &owner,
                Listing {
                    description: String::new(),
                    default_role: Role::Viewer,
                },
            )
            .await
            .unwrap();
        let ada = SigningKey::from_bytes(&rand::random());
        member_at(workspaces, &owner, &workspace, &ada, Role::Editor).await;

        // The operator pauses the host: a stranger is turned away with the wait a host at its
        // bound asks for.
        workspaces.hold_mode(HostMode::held(
            dir.path().to_owned(),
            crate::workspaces::mode::Mode::SpectatorOnly,
        ));
        let (stranger_key, stranger) = device_of(
            &SigningKey::from_bytes(&rand::random()),
            DeviceCapabilities::member(),
        );
        let refused = through_the_entry(workspaces, &workspace, (&stranger_key, &stranger)).await;
        let Entered::Refused(EnterRefusal::Entry {
            refused: answer,
            retry_after_secs,
        }) = refused
        else {
            panic!("a paused host admits nobody new: {refused:?}");
        };
        assert_eq!(answer.code, EntryRefusalCode::Full);
        assert_eq!(
            retry_after_secs,
            Some(crate::workspaces::mode::PAUSED_RETRY_AFTER_SECS)
        );
        assert!(
            answer.error.contains("watch-only"),
            "a watch-only host says so: {}",
            answer.error
        );

        // Paused entirely, the host says it is paused, not merely watch-only.
        workspaces.hold_mode(HostMode::held(
            dir.path().to_owned(),
            crate::workspaces::mode::Mode::BackSoon,
        ));
        let Entered::Refused(EnterRefusal::Entry {
            refused: answer, ..
        }) = through_the_entry(workspaces, &workspace, (&stranger_key, &stranger)).await
        else {
            panic!("a back-soon host admits nobody new");
        };
        assert!(
            answer.error.contains("paused"),
            "a back-soon host says so: {}",
            answer.error
        );

        // A device of a member is not new: it enters at the part its person holds.
        let (tab_key, tab) = device_of(&ada, DeviceCapabilities::member());
        assert!(
            matches!(
                through_the_entry(workspaces, &workspace, (&tab_key, &tab)).await,
                Entered::Admitted {
                    role: Role::Editor,
                    ..
                }
            ),
            "a member's further device enters while the host is paused"
        );

        // Back to normal, the next newcomer walks in.
        workspaces.hold_mode(HostMode::held(
            dir.path().to_owned(),
            crate::workspaces::mode::Mode::Normal,
        ));
        assert!(
            matches!(
                through_the_entry(workspaces, &workspace, (&stranger_key, &stranger)).await,
                Entered::Admitted { .. }
            ),
            "an unpaused host admits the stranger"
        );
        server.shutdown().await;
    }

    /// An agent's key comes into its workspace at its person's own place and never above it: a
    /// Viewer's agent edits nothing, and the key of someone the workspace does not have is
    /// refused.
    #[tokio::test]
    async fn an_agents_key_enters_at_its_persons_place_and_never_for_a_stranger() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = hosting_device();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Team").await.unwrap().workspace;
        let id = tilo_server_protocol::WorkspaceId::parse(&workspace).unwrap();
        let scope = || {
            DeviceCapabilities::agent_grant(AgentScope::agent_in(
                *id.as_uuid().as_bytes(),
                "scribe",
            ))
        };
        let viewer = SigningKey::from_bytes(&rand::random());
        member_at(workspaces, &owner, &workspace, &viewer, Role::Viewer).await;
        let (agent_key, agent) = device_of(&viewer, scope());
        workspaces
            .enter_agent_key((&agent_key, &agent), &workspace, "Scribe")
            .await
            .expect("the viewer's agent comes in");
        let now = tilo_clock::wall();
        let hosted = workspaces.workspace(&workspace).await.unwrap();
        let hosted = hosted.lock().await;
        let role = hosted.peer().role(&agent.device_public_key, now).unwrap();
        let access = hosted
            .peer()
            .permissions(agent.device_public_key, now)
            .unwrap();
        drop(hosted);
        assert_eq!(
            role,
            collab_types::Role::Viewer,
            "the agent holds its person's place"
        );
        assert!(
            !access.allows(
                &tilo_action_policy::TreePath::workspace(&workspace),
                tilo_action_policy::ActionKind::Undoable,
                now
            ),
            "a viewer's agent edits nothing"
        );
        assert!(
            role_of(workspaces, &workspace, viewer.verifying_key().to_bytes())
                .await
                .is_some_and(|role| role.contains("Viewer")),
            "the person keeps their place"
        );
        let stranger = SigningKey::from_bytes(&rand::random());
        let (stranger_key, strangers) = device_of(&stranger, scope());
        assert_eq!(
            workspaces
                .enter_agent_key((&stranger_key, &strangers), &workspace, "Scribe")
                .await,
            Err("the agent's person is not in this workspace".to_owned())
        );
        server.shutdown().await;
    }
}
