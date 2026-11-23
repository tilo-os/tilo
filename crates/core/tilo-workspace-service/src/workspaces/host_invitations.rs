//! Owner invitations: a link that makes whoever accepts it an owner of the
//! workspace, as [`Workspaces::declare_owner`] does: the way a workspace that
//! has no owner yet gets one, without anyone knowing the person's key first.
//! The invitations still waiting to be accepted are kept in
//! `owner-invitations.json` beside the workspaces, so one handed out survives
//! a restart.

use std::collections::BTreeMap;
use std::path::PathBuf;

use tilo_lock::MutexRecover as _;
use tilo_server_protocol::workspaces::{IssueInvitationRequest, IssuedInvitation};
use tilo_user_identity::DeviceCert;

use super::{CollaborationError, Workspaces};

/// Where the owner invitations not accepted yet are kept, under the host's
/// workspace directory.
const FILE: &str = "owner-invitations.json";

/// The owner invitations handed out and not accepted yet: the workspace
/// each is into, by invitation id (hex).
pub(super) struct OwnerInvitations {
    path: PathBuf,
    open: std::sync::Mutex<BTreeMap<String, String>>,
}

impl OwnerInvitations {
    pub(super) fn open(dir: &std::path::Path) -> Self {
        let path = dir.join(FILE);
        // None handed out yet is no file; one that does not read leaves the
        // links it held unable to make an owner, which the operator hears.
        let open = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                tracing::warn!(%error, path = %path.display(), "the owner invitations do not read: their links make nobody an owner");
                BTreeMap::new()
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "the owner invitations could not be read: their links make nobody an owner");
                BTreeMap::new()
            }
        };
        Self {
            path,
            open: std::sync::Mutex::new(open),
        }
    }

    fn keep(&self, open: &BTreeMap<String, String>) -> Result<(), CollaborationError> {
        let bytes =
            serde_json::to_vec(open).map_err(|error| CollaborationError::new(error.to_string()))?;
        tilo_files::native::atomic::write(&self.path, &bytes)
            .map_err(|error| CollaborationError::new(error.to_string()))
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "the file is written under the lock, so two writers keep it in the order they changed it"
    )]
    fn add(&self, invite_id: [u8; 32], workspace: &str) -> Result<(), CollaborationError> {
        let mut open = self.open.lock_recover();
        if open.get(&hex::encode(invite_id)).map(String::as_str) == Some(workspace) {
            return Ok(());
        }
        open.insert(hex::encode(invite_id), workspace.to_owned());
        self.keep(&open)
    }

    /// Whether `invite_id` is an owner invitation into `workspace`.
    fn holds(&self, invite_id: [u8; 32], workspace: &str) -> bool {
        self.open
            .lock_recover()
            .get(&hex::encode(invite_id))
            .is_some_and(|into| into == workspace)
    }

    fn remove(&self, invite_id: [u8; 32]) {
        let mut open = self.open.lock_recover();
        if open.remove(&hex::encode(invite_id)).is_some()
            && let Err(error) = self.keep(&open)
        {
            tracing::warn!(%error, "the accepted owner invitation stays on record");
        }
    }
}

impl Workspaces {
    /// Issue an owner invitation into `workspace` as `device`'s person, who
    /// must be able to make owners themselves: whoever accepts it is an owner
    /// from then on, as [`Self::declare_owner`] makes them. For the wait-list
    /// entry `wait_list_entry` when it names one.
    ///
    /// # Errors
    ///
    /// The person may not make owners, or the invitation was not issued.
    pub(super) async fn issue_owner_invitation(
        &self,
        workspace: &str,
        device: &DeviceCert,
        request: IssueInvitationRequest,
        wait_list_entry: Option<[u8; 32]>,
    ) -> Result<IssuedInvitation, CollaborationError> {
        {
            let governed = self.workspace(workspace).await?;
            let governed = governed.lock().await;
            self.authorize(
                device,
                Some(&governed),
                super::authority::WorkspaceOp::MakeOwner,
                tilo_clock::unix_now_ns(),
            )?;
        }
        let issued = self
            .issue_for(workspace, device, request, true, wait_list_entry)
            .await?;
        self.inner
            .owner_invitations
            .add(issued.invite_id, workspace)?;
        Ok(issued)
    }

    /// Whether a person owns `workspace`: its governance names someone else
    /// than the host that runs it as its owner.
    pub async fn has_owner(&self, workspace: &str) -> bool {
        let host = hex::encode(self.inner.host.cert.user_public_key);
        self.policy_of(workspace)
            .await
            .is_some_and(|policy| policy.owner().is_some_and(|owner| owner != host))
    }

    /// Whether the person `user` owns `workspace`, as its governance says.
    pub async fn is_owner(&self, workspace: &str, user: [u8; 32]) -> bool {
        self.policy_of(workspace)
            .await
            .is_some_and(|policy| policy.is_owner(&hex::encode(user)))
    }

    /// Hand `workspace`, which this host owns, to the person `user`: its
    /// governance names them its owner from then on, so its entry lets them
    /// in even when it takes people by invitation, and a key of theirs lists
    /// the workspace, enters it and invites before any device of theirs has
    /// joined. This host stays in it as an administrator the owner may
    /// withdraw. `name`, when given, is how the workspace names them until
    /// they rename themselves.
    ///
    /// # Errors
    ///
    /// The workspace is not governed here, this host no longer owns it, or its
    /// governance could not be changed.
    pub async fn declare_owner(
        &self,
        workspace: &str,
        user: [u8; 32],
        name: Option<&str>,
    ) -> Result<(), CollaborationError> {
        let governed = self.workspace(workspace).await?;
        let mut governed = governed.lock().await;
        self.give_ownership(&mut governed, user, name)
            .await
            .map_err(CollaborationError::new)?;
        Ok(())
    }

    /// Whether `invite_id` is an owner invitation into `workspace`.
    pub(super) fn owner_invitation(&self, invite_id: [u8; 32], workspace: &str) -> bool {
        self.inner.owner_invitations.holds(invite_id, workspace)
    }

    /// After the person `user` was admitted to `workspace` with the
    /// invitation `invitation` (its signed bytes): an owner invitation makes
    /// them an owner as well.
    pub(super) async fn accepted(&self, workspace: &str, invitation: &[u8], user: [u8; 32]) {
        let invite_id = *blake3::hash(invitation).as_bytes();
        if !self.inner.owner_invitations.holds(invite_id, workspace) {
            return;
        }
        match self.declare_owner(workspace, user, None).await {
            Ok(()) => {
                self.inner.owner_invitations.remove(invite_id);
                tracing::info!(
                    workspace,
                    person = %hex::encode(user),
                    "the owner invitation made its holder an owner"
                );
            }
            Err(error) => tracing::warn!(
                workspace,
                %error,
                "the owner invitation was accepted, but its holder was not made an owner"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspaces::listing::entry_tests::walk_in_as;
    use crate::workspaces::tests::{device_cert, governor, governor_with, guest_device, test_host};
    use ed25519_dalek::SigningKey;
    use tilo_peer_state::join::Role;
    use tilo_server_protocol::directory::{EntryJoin, Listing};
    use tilo_server_protocol::workspaces::{
        JoinReply, Workspace, WorkspacesReply, WorkspacesRequest,
    };
    use tilo_user_identity::{DeviceCapabilities, RevocationList};

    const DAY_NS: u64 = 24 * 3600 * 1_000_000_000;

    /// A host as tilo.sh runs one: it keeps workspaces of people's own, and
    /// is nobody in them itself.
    async fn site(
        dir: &std::path::Path,
        (key, cert): &(ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert),
    ) -> crate::workspaces::fixtures::TestHost {
        let server = test_host(dir, key, cert).await;
        assert!(
            server
                .workspaces()
                .offer_own_workspaces(crate::workspaces::OwnWorkspaceOffer::free("tilo.test"))
                .unwrap()
        );
        server
    }

    /// An invitation asked into a workspace, naming `person` when it is asked
    /// for them, as the workspace's own requests ask one.
    fn asked(request_id: [u8; 32], role: Role, person: Option<[u8; 32]>) -> IssueInvitationRequest {
        IssueInvitationRequest {
            request_id,
            role,
            recipient_user_public_key: person,
            invitee_name: None,
            expires_at_ns: tilo_clock::unix_now_ns().saturating_add(DAY_NS),
            inviter_name: None,
        }
    }

    /// An invitation asked of the workspace, as the invite action asks one.
    fn invitation(role: Role) -> IssueInvitationRequest {
        asked(rand::random(), role, None)
    }

    /// `device` walks into `workspace` with an invitation asked for its
    /// person, as a key of theirs does, going by the device's own name.
    async fn enters(
        workspaces: &Workspaces,
        workspace: &str,
        device: &DeviceCert,
        request_id: [u8; 32],
    ) -> JoinReply {
        let issued = workspaces
            .issue(
                workspace,
                workspaces.serving_certificate(),
                asked(request_id, Role::Editor, Some(device.user_public_key)),
            )
            .await
            .unwrap();
        workspaces
            .admit_proven(
                issued.signed_invitation,
                device.clone(),
                rand::random(),
                &device.device_label,
                None,
            )
            .await
    }

    /// `device`'s person asks for an owner link into `workspace`, as the
    /// command line does.
    async fn owner_link_asked_for_by(
        workspaces: &Workspaces,
        workspace: &str,
        device: &DeviceCert,
        request_id: [u8; 32],
    ) -> IssuedInvitation {
        workspaces
            .issue_as(
                workspace,
                device,
                asked(request_id, Role::Editor, None),
                true,
            )
            .await
            .unwrap()
    }

    /// What the link `signed` says to whoever opens it, its signature
    /// checked.
    fn opens(signed: &[u8]) -> tilo_peer_state::join::VerifiedJoin {
        tilo_peer_state::join::decode_and_verify_invitation(
            signed,
            None,
            &RevocationList::new(),
            tilo_clock::unix_now_ns(),
        )
        .unwrap()
    }

    /// A browser that never held an identity makes one and accepts: a fresh
    /// person's device, redeeming the link's invitation.
    async fn accept(workspaces: &Workspaces, invitation: Vec<u8>) -> ([u8; 32], JoinReply) {
        let (_, newcomer) = guest_device();
        let reply = workspaces
            .admit_proven(invitation, newcomer.clone(), rand::random(), "Ada", None)
            .await;
        (newcomer.user_public_key, reply)
    }

    /// `device`'s person as a page's workspace lists them.
    fn row<'a>(page: &'a Workspace, device: &DeviceCert) -> &'a collab_types::Member {
        let person = hex::encode(device.user_public_key);
        page.members
            .iter()
            .find(|row| row.id.person_id() == Some(person.as_str()))
            .unwrap_or_else(|| panic!("{device:?} has a row in {page:?}"))
    }

    /// The owner link: an owner invitation, opened by someone whose key the
    /// host never knew, hands them the workspace as they come in, also after
    /// a restart; once someone did, the workspace
    /// has its owner, and the link is spent.
    #[tokio::test]
    async fn an_owner_invitation_makes_whoever_accepts_it_the_owner() {
        let dir = tempfile::tempdir().unwrap();
        let host = governor();
        let server = site(dir.path(), &host).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Lounge").await.unwrap().workspace;
        assert!(!workspaces.has_owner(&workspace).await);

        let issued = workspaces
            .issue_owner_invitation(&workspace, &host.1, invitation(Role::Editor), None)
            .await
            .unwrap();
        server.shutdown().await;

        let server = site(dir.path(), &host).await;
        let workspaces = server.workspaces();
        let JoinReply::Invitation {
            inviter,
            members,
            role,
            administrator,
            owner,
            ..
        } = workspaces
            .inspect_invitation(&issued.signed_invitation)
            .await
        else {
            panic!("the host honours its owner invitation");
        };
        assert_eq!(
            (inviter, role),
            (None, Role::Editor),
            "the link is from nobody in particular, and an owner joins as an editor"
        );
        assert_eq!(members, 0, "the host is none of the people it counts");
        assert!(
            administrator && owner,
            "the invitation says what it makes whoever opens it"
        );

        let (owner, reply) = accept(workspaces, issued.signed_invitation.clone()).await;
        assert!(matches!(reply, JoinReply::Admitted { .. }), "{reply:?}");
        assert!(
            workspaces.has_owner(&workspace).await,
            "the workspace has its owner"
        );
        assert!(workspaces.is_owner(&workspace, owner).await);
        let policy = workspaces.policy_of(&workspace).await.unwrap();
        assert_eq!(policy.owner(), Some(hex::encode(owner).as_str()));
        assert!(
            policy.is_administrator(&hex::encode(host.1.user_public_key)),
            "the host it was handed from serves it as an administrator"
        );
        // Who the workspace lists as its owner (`tilo do workspace/members#list`): the
        // person the link made one.
        let members = workspaces.members(&workspace).await.unwrap();
        let row = members
            .iter()
            .find(|row| row["member"]["person"] == hex::encode(owner))
            .unwrap_or_else(|| panic!("the owner has a row in {members:?}"));
        assert_eq!(row["owner"], true, "{members:?}");
        assert_eq!(row["administrator"], true, "{members:?}");
        let (_, again) = accept(workspaces, issued.signed_invitation).await;
        assert!(
            matches!(again, JoinReply::Refused(_)),
            "one owner link, one owner: {again:?}"
        );
        server.shutdown().await;
    }

    /// An owner invitation a person's key asks for names that person on the
    /// card that opens it, as the workspace shows them, even when it was
    /// signed before the workspace knew a name for them: the card reads a
    /// person, never "someone".
    #[tokio::test]
    async fn an_owner_invitation_a_persons_key_asks_for_names_them() {
        let dir = tempfile::tempdir().unwrap();
        let holder = governor();
        let server = site(dir.path(), &holder).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Tilo").await.unwrap().workspace;

        // The person the workspace is kept for, under a root of their own,
        // before any device of theirs joined: no name known yet.
        let root = SigningKey::from_bytes(&[7; 32]);
        let person = root.verifying_key().to_bytes();
        workspaces
            .declare_owner(&workspace, person, None)
            .await
            .unwrap();

        // A key of theirs enters the workspace, then asks for an owner link,
        // as the command line does.
        let (_, key) = device_cert(
            &root,
            "Quinten's key",
            DeviceCapabilities::server_key_grant(),
        );
        let admitted = enters(workspaces, &workspace, &key, [2; 32]).await;
        assert!(
            matches!(admitted, JoinReply::Admitted { .. }),
            "{admitted:?}"
        );
        let issued = owner_link_asked_for_by(workspaces, &workspace, &key, [3; 32]).await;
        let join = opens(&issued.signed_invitation);
        assert!(join.administrator(), "an owner link makes an owner");
        let from = join.inviter().expect("the invitation says who asked");
        assert_eq!(from.user_public_key, person);
        assert_eq!(from.name, "", "the workspace knew no name for them yet");

        // A device of theirs joins, as their page does, going by their name.
        let (_, page) = device_cert(&root, "Quinten", DeviceCapabilities::member());
        let admitted = enters(workspaces, &workspace, &page, [6; 32]).await;
        assert!(
            matches!(admitted, JoinReply::Admitted { .. }),
            "{admitted:?}"
        );

        // The card that opens the link names them: the workspace's name for
        // them now, not the empty one the invitation was signed with.
        let JoinReply::Invitation { inviter, .. } = workspaces
            .inspect_invitation(&issued.signed_invitation)
            .await
        else {
            panic!("the invitation is still honoured");
        };
        let from = inviter.expect("the card says who asked");
        assert_eq!(from.user_public_key, person);
        assert_eq!(from.name, "Quinten", "the name the workspace shows them by");
        server.shutdown().await;
    }

    /// A workspace kept under a person's own root, as one a site keeps for
    /// them is: another device of that same person asking for an invitation
    /// still asks as a person, so the card says who asked. Only the device
    /// that issues it for the workspace itself asks for nobody.
    #[tokio::test]
    async fn an_invitation_a_second_device_of_the_workspaces_person_asks_for_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let holder = governor();
        let server = site(dir.path(), &holder).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Tilo").await.unwrap().workspace;
        let (_, sibling) = governor_with(DeviceCapabilities::server_key_grant());
        assert_eq!(sibling.user_public_key, holder.1.user_public_key);
        assert_ne!(sibling.device_public_key, holder.1.device_public_key);

        let issued = owner_link_asked_for_by(workspaces, &workspace, &sibling, [4; 32]).await;
        assert!(
            opens(&issued.signed_invitation).inviter().is_some(),
            "a device of the person still asks as a person"
        );
        server.shutdown().await;
    }

    /// The owner link's opener is the workspace's owner on its pages: the
    /// People tab shows them as Owner, reads its own rows as theirs, and the
    /// workspace lets them change who is in it.
    #[tokio::test]
    async fn an_owner_links_opener_reads_as_owner_and_governs_peoples_roles() {
        let dir = tempfile::tempdir().unwrap();
        let host = governor();
        let server = site(dir.path(), &host).await;
        let workspaces = server.workspaces();
        let lounge = workspaces.create("Lounge").await.unwrap().workspace;

        let owner_link = workspaces
            .issue_owner_invitation(&lounge, &host.1, invitation(Role::Editor), None)
            .await
            .unwrap();
        let (_, owner) = guest_device();
        let admitted = workspaces
            .admit_proven(
                owner_link.signed_invitation,
                owner.clone(),
                rand::random(),
                "Ada",
                None,
            )
            .await;
        assert!(
            matches!(admitted, JoinReply::Admitted { .. }),
            "{admitted:?}"
        );
        let invite = workspaces
            .issue(&lounge, &owner, invitation(Role::Editor))
            .await
            .unwrap();
        let (_, member) = guest_device();
        let admitted = workspaces
            .admit_proven(
                invite.signed_invitation,
                member.clone(),
                rand::random(),
                "Grace",
                None,
            )
            .await;
        assert!(
            matches!(admitted, JoinReply::Admitted { .. }),
            "{admitted:?}"
        );
        // The owner's own page: their row is theirs, and reads as the owner's.
        let WorkspacesReply::Workspaces(listed) = workspaces
            .page_workspaces(&owner, WorkspacesRequest::List)
            .await
        else {
            panic!("the page lists its workspaces");
        };
        let page = listed
            .into_iter()
            .find(|page| page.workspace == lounge)
            .expect("the Lounge is listed");
        assert_eq!(
            row(&page, &owner).role,
            Some(Role::Owner),
            "the owner link's opener reads as the owner: {page:?}"
        );
        assert!(row(&page, &owner).is_self, "their own row is theirs");
        assert!(!row(&page, &member).is_self, "nobody else's row is");
        assert_eq!(row(&page, &member).role, Some(Role::Editor));
        // Anyone else's page reads the owner as the owner, and their own row as theirs.
        let WorkspacesReply::Workspaces(listed) = workspaces
            .page_workspaces(&member, WorkspacesRequest::List)
            .await
        else {
            panic!("the page lists its workspaces");
        };
        let page = listed
            .into_iter()
            .find(|page| page.workspace == lounge)
            .expect("the Lounge is listed");
        assert_eq!(row(&page, &owner).role, Some(Role::Owner));
        assert!(!row(&page, &owner).is_self);
        assert!(row(&page, &member).is_self);

        // And the workspace lets them change a person's role, which the page reads.
        workspaces
            .set_member_role(
                &lounge,
                &owner,
                tilo_server_protocol::workspaces::SetMemberRoleRequest {
                    request_id: rand::random(),
                    user_public_key: member.user_public_key,
                    role: Role::Viewer,
                },
            )
            .await
            .expect("the owner may change a role");
        let WorkspacesReply::Workspaces(listed) = workspaces
            .page_workspaces(&owner, WorkspacesRequest::List)
            .await
        else {
            panic!("the owner's page lists its workspaces");
        };
        let changed = listed
            .into_iter()
            .find(|page| page.workspace == lounge)
            .expect("the Lounge is listed");
        assert_eq!(row(&changed, &member).role, Some(Role::Viewer));
        server.shutdown().await;
    }

    /// Someone invited into any workspace on the host is past its home's
    /// invite-only entry: whoever invited them let them in, and the home is
    /// where everyone on the host is.
    #[tokio::test]
    async fn an_invitation_anywhere_on_the_host_opens_the_invite_only_home() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let server = test_host(dir.path(), &key, &cert).await;
        let workspaces = server.workspaces();
        assert!(
            workspaces
                .offer_own_workspaces(crate::workspaces::OwnWorkspaceOffer::free("tilo.test"))
                .unwrap()
        );
        let home = workspaces.create("Lounge").await.unwrap().workspace;
        workspaces
            .publish(
                &home,
                &cert,
                Listing {
                    description: "Everyone".into(),
                    default_role: Role::Editor,
                },
            )
            .await
            .unwrap();
        workspaces
            .list_invitation_only(&home, &cert, true)
            .await
            .unwrap();
        let elsewhere = workspaces.create("Acme").await.unwrap().workspace;
        let issued = workspaces
            .issue(&elsewhere, &cert, invitation(Role::Editor))
            .await
            .unwrap();
        let (_, newcomer) = guest_device();
        let join = EntryJoin {
            user_public_key: hex::encode(newcomer.user_public_key),
            display_name: "Ada".into(),
        };
        assert!(
            walk_in_as(workspaces, &home, &join, &newcomer)
                .await
                .is_err(),
            "a stranger is turned away from the home"
        );
        let reply = workspaces
            .admit_proven(
                issued.signed_invitation,
                newcomer.clone(),
                rand::random(),
                "Ada",
                None,
            )
            .await;
        assert!(matches!(reply, JoinReply::Admitted { .. }), "{reply:?}");
        let reply = walk_in_as(workspaces, &home, &join, &newcomer)
            .await
            .unwrap();
        assert!(
            matches!(reply, JoinReply::Admitted { .. }),
            "in Acme, so in the Lounge: {reply:?}"
        );
        server.shutdown().await;
    }

    /// A person who walked in through a published entry is declared an owner
    /// while their page is open: the page's own question whether they may
    /// invite others is answered yes from then on, of the device they joined
    /// with, not of a device they would have to join again with.
    #[tokio::test]
    async fn a_person_declared_an_owner_may_invite_from_the_device_they_walked_in_with() {
        let dir = tempfile::tempdir().unwrap();
        let governing = governor();
        let site = site(dir.path(), &governing).await;
        let workspaces = site.workspaces();
        let workspace = workspaces.create("Lounge").await.unwrap().workspace;
        workspaces
            .publish(
                &workspace,
                &governing.1,
                Listing {
                    description: "The Tilo community".into(),
                    default_role: Role::Editor,
                },
            )
            .await
            .unwrap();
        let (_, newcomer) = guest_device();
        let join = EntryJoin {
            user_public_key: hex::encode(newcomer.user_public_key),
            display_name: "Ada".into(),
        };
        let reply = walk_in_as(workspaces, &workspace, &join, &newcomer)
            .await
            .unwrap();
        assert!(matches!(reply, JoinReply::Admitted { .. }), "{reply:?}");
        let may_invite = || WorkspacesRequest::Permits {
            workspace: workspace.clone(),
            node: tilo_action_policy::roles::governance_action(
                &workspace,
                tilo_action_policy::roles::governance::INVITE,
            )
            .to_string(),
        };
        assert_eq!(
            workspaces.page_workspaces(&newcomer, may_invite()).await,
            WorkspacesReply::Permits(false),
            "an editor invites nobody"
        );
        workspaces
            .declare_owner(&workspace, newcomer.user_public_key, None)
            .await
            .unwrap();
        assert_eq!(
            workspaces.page_workspaces(&newcomer, may_invite()).await,
            WorkspacesReply::Permits(true),
            "the owner's own device may invite at once"
        );
        site.shutdown().await;
    }
}
