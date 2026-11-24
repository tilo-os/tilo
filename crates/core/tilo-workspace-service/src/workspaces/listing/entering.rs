//! A device entering a workspace on its session
//! ([`tilo_server_protocol::entering`]): through the entry, which lets the
//! person in as its HTTP form did and issues the invitation, or with an
//! invitation of its own; the workspace admits the device, which proved it
//! holds its key on the session's connection, with that invitation.

use tilo_server_protocol::directory::EntryJoin;
use tilo_server_protocol::entering::{EnterRefusal, Entered, Entering, Entry, Through};
use tilo_server_protocol::web::{MAX_DISPLAY_NAME_CHARS, kept};
use tilo_server_protocol::workspaces::JoinReply;
use tilo_user_identity::DeviceCert;

use crate::handshake::{Admit, EntryFuture};
use crate::workspace_access::person_device;
use crate::workspaces::mode::{Mode, PAUSED_RETRY_AFTER_SECS};
use crate::workspaces::pages::Presented;
use crate::workspaces::{Carried, Workspaces};

/// A refusal of the join itself, after the entry let the person in.
const fn refused(reason: String) -> EnterRefusal {
    EnterRefusal::Workspace(reason)
}

impl Admit for Workspaces {
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
                Through::Entry => self.member_device(cert, &entering.workspace).await,
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
            let reply = match (member_device, entering.through) {
                (Some(reply), _) => reply,
                (None, Through::Entry) => {
                    match self.entry(cert, &entering.workspace, &entering.name).await {
                        Ok((invitation, token)) => {
                            self.redeem_on_connection(
                                Presented {
                                    invitation,
                                    cert: cert.clone(),
                                    request_id: entering.request_id,
                                },
                                &entering.name,
                                Some(token.as_bytes()),
                            )
                            .await
                        }
                        Err(refused) => return Entered::Refused(refused),
                    }
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
                    self.redeem_on_connection(
                        Presented {
                            invitation,
                            cert: cert.clone(),
                            request_id: entering.request_id,
                        },
                        &entering.name,
                        token.as_deref(),
                    )
                    .await
                }
            };
            let entered = entered(reply);
            if !matches!(entered, Entered::Admitted { .. }) {
                return entered;
            }
            if let Some(access) = access {
                let keying = crate::timing::start();
                self.key_arrival(&entering.workspace, cert, access).await;
                crate::timing::event("entry.keys", keying, 0);
            }
            self.with_host(&entering.workspace, entered).await
        })
    }
}

impl Workspaces {
    /// Let `cert`'s person, named `name`, in through `workspace`'s entry: the
    /// invitation it issued them and its one-time token, or why not.
    async fn entry(
        &self,
        cert: &DeviceCert,
        workspace: &str,
        name: &str,
    ) -> Result<(Vec<u8>, String), EnterRefusal> {
        // A key acting for someone the workspace has in it is no device of
        // theirs: the entry would hand it its person's part, and its
        // redemption would hold it unbounded. It comes in by the delegation
        // that bounds it.
        if !person_device(cert)
            && self
                .member_role(workspace, cert.user_public_key)
                .await
                .is_some()
        {
            return Err(EnterRefusal::Entry {
                retry_after_secs: None,
                refused: crate::workspaces::EntryRefusal::Refused(
                    "an agent comes into a workspace its person is in by its delegation, not through the entry"
                        .to_owned(),
                )
                .answer_with(self.wait_list_page()),
            });
        }
        let join = EntryJoin {
            user_public_key: hex::encode(cert.user_public_key),
            display_name: kept(name, MAX_DISPLAY_NAME_CHARS),
        };
        let asking = crate::timing::start();
        let admitted = self.admit_listed(workspace, &join).await;
        crate::timing::event("entry.entry", asking, 0);
        admitted
            .map(|(issued, token)| (issued.signed_invitation, token))
            .map_err(|refusal| EnterRefusal::Entry {
                retry_after_secs: match refusal {
                    crate::workspaces::EntryRefusal::Full { retry_after_secs } => {
                        Some(retry_after_secs)
                    }
                    _ => None,
                },
                refused: refusal.answer_with(self.wait_list_page()),
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
                Ok(governed) => {
                    let governed = governed.lock().await;
                    governed.peer().governance().removed_from_member_list(cert)
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
        let governed = self.workspace(workspace).await.ok()?;
        let mut governed = governed.lock().await;
        // The refusals the entry would answer: a person banned or removed here
        // does not come back in by the admission it gave them before.
        let policy = governed
            .peer()
            .permissions(self.inner.host.cert.device_public_key, tilo_clock::wall())
            .ok()?
            .policy;
        if policy.is_blocked(&hex::encode(cert.user_public_key))
            || governed.removals.refuses(cert.user_public_key, now)
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
        // leaves the device in governance and out of the device_access this
        // workspace decides its changes against: settle before answering, as
        // a device that enters once more does.
        if !self
            .decides_for(governed.workspace_id(), cert.device_public_key)
            .await
        {
            self.settle(&mut governed).await.ok()?;
        }
        Some(entered(
            Self::admitted(&governed, cert.device_public_key, now).ok()?,
        ))
    }

    /// Enrol `cert`'s device in `workspace`, where its person is a member, as
    /// [`Self::member_device`] admits a further device of theirs: what a join
    /// on the session asks for when the workspace's governance has the person
    /// and not yet this device ([`crate::workspace_access::Decision::AdmitAndEnrol`]).
    /// Whoever asked decides again afterwards.
    pub(crate) async fn enrol_member_device(&self, cert: &DeviceCert, workspace: &str) {
        if let Some(refused @ JoinReply::Refused(_)) = self.member_device(cert, workspace).await {
            tracing::debug!(%workspace, ?refused, "a member's further device was not enrolled");
        }
    }

    /// Admit `cert`'s device to `workspace` as a device of its person, who
    /// is a member there already: governance records the device under them
    /// ([`tilo_peer_state::PeerGovernanceMut::admit_member_device`]), so it
    /// holds exactly the part its person holds, and the answer waits until
    /// that governance is retained, as a redemption's does. Membership
    /// follows the person, not one device key: a browser tab is a device of
    /// its own, and every further one its person opens comes in this way.
    ///
    /// `None` when this is not for it, and the entry decides as it does for
    /// anyone: a key acting for the person rather than a device of theirs
    /// (an agent's grant), a person the workspace does not have in it, or
    /// one it banned or removed within the day.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked until the device is committed, settled and its admission read"
    )]
    async fn member_device(&self, cert: &DeviceCert, workspace: &str) -> Option<JoinReply> {
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
            return Some(JoinReply::Refused(refusal.to_string()));
        }
        let governed = self.workspace(workspace).await.ok()?;
        let reply = {
            let mut governed = governed.lock().await;
            let policy = governed
                .peer()
                .permissions(self.inner.host.cert.device_public_key, tilo_clock::wall())
                .ok()?
                .policy;
            let person = hex::encode(cert.user_public_key);
            // A workspace of the person's own names them its owner from the
            // moment it is made, so a device of theirs comes in before any
            // of theirs has entered it.
            if policy.is_blocked(&person)
                || governed.removals.refuses(cert.user_public_key, now)
                || policy.active_role(&person).is_none()
            {
                return None;
            }
            // A device the workspace met and took off its member list comes back
            // by a deliberate admission, never by presenting the certificate
            // the workspace still holds: it is told so in words.
            if governed.peer().governance().removed_from_member_list(cert) {
                return Some(JoinReply::Refused(
                    tilo_peer_state::REMOVED_DEVICE_REFUSED.to_owned(),
                ));
            }
            let key = self.inner.host.key.clone();
            let mut candidate = governed.peer().clone();
            match candidate.governance_mut().admit_member_device(
                cert,
                &key,
                crate::workspaces::system_time(now),
            ) {
                // The signed member list has the device already, and the host's
                // own list of the workspace may not have caught up with it yet:
                // settling is what lists the device here.
                Ok(false) => {
                    if let Err(error) = self.settle(&mut governed).await {
                        return Some(JoinReply::Refused(error));
                    }
                }
                Ok(true) => {
                    let state = governed.collaboration().clone();
                    if let Err(error) = governed.commit(
                        candidate,
                        &key,
                        crate::workspaces::system_time(now),
                        |_, _| Ok(state),
                    ) {
                        return Some(JoinReply::Refused(error));
                    }
                    if let Err(error) = self.settle(&mut governed).await {
                        return Some(JoinReply::Refused(error));
                    }
                    tracing::info!(
                        %workspace,
                        person = %person,
                        device = %hex::encode(cert.device_public_key),
                        outcome = "member device admitted",
                        "a member's further device came in through the entry as theirs"
                    );
                }
                Err(error) => return Some(JoinReply::Refused(error)),
            }
            Self::admitted(&governed, cert.device_public_key, now)
                .unwrap_or_else(|error| JoinReply::Refused(error.to_string()))
        };
        Some(reply)
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
            let taking = crate::timing::start();
            if let Err(error) = workspaces
                .take_governance(&workspace, device, request, Carried::Unkept)
                .await
            {
                tracing::warn!(%workspace, %error, "an entry's channel request was not taken");
            }
            crate::timing::event("entry.keys.taken", taking, 0);
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
            Ok(governed) => {
                let hint = self.transport_hint(&*governed.lock().await);
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

#[cfg(test)]
mod tests {
    use ed25519_dalek::SigningKey;
    use tilo_server_protocol::directory::{EntryJoin, EntryRefusalCode, Listing};
    use tilo_server_protocol::entering::{EnterRefusal, Entered, Entering, Entry, Through};
    use tilo_server_protocol::workspaces::RemoveMemberRequest;
    use tilo_server_protocol::workspaces::{JoinReply, Role};
    use tilo_user_identity::{AgentScope, DeviceCapabilities, DeviceCert, sign_device_cert};

    use crate::handshake::Admit;
    use crate::workspaces::fixtures::{governor, signed_pre_admission_as, test_host};
    use crate::workspaces::mode::HostMode;
    use crate::workspaces::{PreAdmitRequest, Workspaces};

    /// A device `root` certifies with `capabilities`, as the person's own
    /// root signs each tab or computer of theirs.
    fn device_of(root: &SigningKey, capabilities: DeviceCapabilities) -> DeviceCert {
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
        cert
    }

    /// `cert`'s device asks to come into `workspace` through its entry, on a
    /// session whose proof the handshake checked.
    async fn through_the_entry(
        workspaces: &Workspaces,
        workspace: &str,
        cert: &DeviceCert,
    ) -> Entered {
        Admit::enter(
            workspaces,
            cert,
            Entry {
                entering: Entering {
                    workspace: workspace.to_owned(),
                    name: "Ada".into(),
                    request_id: rand::random(),
                    through: Through::Entry,
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
        let first = device_of(root, DeviceCapabilities::member());
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
                first,
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
        let (key, owner) = governor();
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
            .list_invitation_only(&closed, &owner, true)
            .await
            .unwrap();
        let unlisted = workspaces.create("Team").await.unwrap().workspace;

        let ada = SigningKey::from_bytes(&rand::random());
        member_at(workspaces, &owner, &closed, &ada, Role::Moderator).await;
        member_at(workspaces, &owner, &unlisted, &ada, Role::Commenter).await;
        let before = role_of(workspaces, &closed, ada.verifying_key().to_bytes()).await;

        for (workspace, role) in [(&closed, Role::Moderator), (&unlisted, Role::Commenter)] {
            let tab = device_of(&ada, DeviceCapabilities::member());
            let entered = through_the_entry(workspaces, workspace, &tab).await;
            assert!(
                matches!(entered, Entered::Admitted { role: admitted, .. } if admitted == role),
                "{workspace}: {entered:?}"
            );
            // In now, the device enters again as a member's.
            let again = through_the_entry(workspaces, workspace, &tab).await;
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
            let stranger = device_of(
                &SigningKey::from_bytes(&rand::random()),
                DeviceCapabilities::member(),
            );
            let entered = through_the_entry(workspaces, workspace, &stranger).await;
            assert!(
                matches!(entered, Entered::Refused(_)),
                "{workspace}: {entered:?}"
            );
        }
        server.shutdown().await;
    }

    /// The entry itself, asked for a person an invitation-only workspace has
    /// in it, lets them in as its doc says: with an invitation at the part
    /// they hold, which a further device of theirs redeems without changing
    /// it.
    #[tokio::test]
    async fn an_invitation_only_entry_lets_a_member_in_at_their_part() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
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
            .list_invitation_only(&workspace, &owner, true)
            .await
            .unwrap();
        let ada = SigningKey::from_bytes(&rand::random());
        member_at(workspaces, &owner, &workspace, &ada, Role::Moderator).await;
        let before = role_of(workspaces, &workspace, ada.verifying_key().to_bytes()).await;

        let join = EntryJoin {
            user_public_key: hex::encode(ada.verifying_key().to_bytes()),
            display_name: "Ada".into(),
        };
        let (issued, token) = workspaces.admit_listed(&workspace, &join).await.unwrap();
        let tab = device_of(&ada, DeviceCapabilities::member());
        let reply = workspaces
            .admit_proven(
                issued.signed_invitation,
                tab,
                rand::random(),
                "Ada",
                Some(token.as_bytes()),
            )
            .await;
        assert!(
            matches!(
                reply,
                JoinReply::Admitted {
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
        let (key, owner) = governor();
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
            let tab = device_of(root, DeviceCapabilities::member());
            let entered = through_the_entry(workspaces, &workspace, &tab).await;
            assert!(matches!(entered, Entered::Refused(_)), "{who}: {entered:?}");
        }

        let agent = device_of(
            &ada,
            DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
        );
        let entered = through_the_entry(workspaces, &workspace, &agent).await;
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
        let (key, owner) = governor();
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
        let stranger = device_of(
            &SigningKey::from_bytes(&rand::random()),
            DeviceCapabilities::member(),
        );
        let refused = through_the_entry(workspaces, &workspace, &stranger).await;
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
        }) = through_the_entry(workspaces, &workspace, &stranger).await
        else {
            panic!("a back-soon host admits nobody new");
        };
        assert!(
            answer.error.contains("paused"),
            "a back-soon host says so: {}",
            answer.error
        );

        // A device of a member is not new: it enters at the part its person holds.
        let tab = device_of(&ada, DeviceCapabilities::member());
        assert!(
            matches!(
                through_the_entry(workspaces, &workspace, &tab).await,
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
                through_the_entry(workspaces, &workspace, &stranger).await,
                Entered::Admitted { .. }
            ),
            "an unpaused host admits the stranger"
        );
        server.shutdown().await;
    }
}
