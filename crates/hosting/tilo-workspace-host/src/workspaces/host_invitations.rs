//! Invitations the host makes for itself: the ones whoever runs it asks for
//! (a link to hand someone, an invitation for a wait-list address), signed by
//! the host's own device and naming no person as who sent them, so the
//! person who opens one reads the workspace's name, not a stand-in member.
//!
//! An owner invitation also makes whoever accepts it an owner of the
//! workspace, as [`Workspaces::declare_owner`] does: the way a host that has
//! no owner yet gets one, without anyone knowing the person's key first. The
//! invitations still waiting to be accepted are kept in `owner-invitations.json`
//! beside the workspaces, so one handed out survives a restart.

use std::collections::BTreeMap;
use std::path::PathBuf;

use tilo_lock::MutexRecover as _;
use tilo_peer_state::join::Role;
use tilo_server_session::workspaces::{IssueInvitationRequest, IssuedInvitation};
use tilo_user_identity::DeviceCert;

use super::{CollaborationError, Workspaces};

/// Where the owner invitations not accepted yet are kept, under the host's
/// workspace directory.
const FILE: &str = "owner-invitations.json";

/// What the host is asked to invite someone as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostInvitation {
    /// The asker's name for this request: asked again with the same id, the
    /// host answers the invitation it made the first time.
    pub request_id: [u8; 32],
    pub role: Role,
    /// Whoever accepts it is an administrator as well (an Editor's role).
    pub administrator: bool,
    /// Whoever accepts it is an owner as well: an administrator whom the
    /// workspace is kept for.
    pub owner: bool,
    /// Who it is for, as the inviter names them (an address, a name).
    pub invitee_name: Option<String>,
    pub expires_at_ns: u64,
    /// The wait-list entry the invitation is issued for, where the person is
    /// an address the entry kept: the entry records the invitation, so the
    /// entry mails this link to it, and it leaves the list once someone joins
    /// with it.
    pub wait_list_entry: Option<[u8; 32]>,
}

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
    /// Invite someone into `workspace` as the host itself: signed by its own
    /// device, from nobody in particular, so the invitation reads as the
    /// workspace's. An owner invitation makes whoever accepts it an owner too.
    ///
    /// # Errors
    ///
    /// The workspace is not governed here, the role is not one an
    /// administrator joins as, or the invitation could not be written.
    pub async fn invite_as_host(
        &self,
        workspace: &str,
        invitation: HostInvitation,
    ) -> Result<IssuedInvitation, CollaborationError> {
        let administrator = invitation.administrator || invitation.owner;
        let issued = self
            .issue_at(
                workspace,
                &self.inner.host.cert.clone(),
                IssueInvitationRequest {
                    request_id: invitation.request_id,
                    role: if administrator {
                        Role::Editor
                    } else {
                        invitation.role
                    },
                    recipient_user_public_key: None,
                    invitee_name: invitation.invitee_name,
                    expires_at_ns: invitation.expires_at_ns,
                    inviter_name: None,
                },
                administrator,
                invitation.wait_list_entry,
            )
            .await?;
        if invitation.owner {
            self.inner
                .owner_invitations
                .add(issued.invite_id, workspace)?;
        }
        Ok(issued)
    }

    /// Issue an owner invitation into `workspace` as `device`'s person, who
    /// must be able to make owners themselves: whoever accepts it is an owner
    /// from then on, as the host's own owner invitation makes them. For the
    /// wait-list entry `wait_list_entry` when it names one.
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

    /// Whether anyone holds `workspace` as its owner: someone the workspace is
    /// kept for, besides the host that runs it.
    pub async fn has_owner(&self, workspace: &str) -> bool {
        self.policy_of(workspace)
            .await
            .is_some_and(|policy| policy.keepers().next().is_some())
    }

    /// Whether the person `user` owns `workspace`: someone it is kept for.
    pub async fn is_owner(&self, workspace: &str, user: [u8; 32]) -> bool {
        self.policy_of(workspace)
            .await
            .is_some_and(|policy| policy.is_keeper(&hex::encode(user)))
    }

    /// Declare the person `user` an owner of `workspace`: its governance
    /// holds them by their root as an administrator whom the workspace is kept
    /// for, so its entry lets them in even when it takes people by invitation,
    /// and a key of theirs lists the workspace, enters it and invites before
    /// any device of theirs has joined. `name`, when given, is how the
    /// workspace names them until they rename themselves.
    ///
    /// # Errors
    ///
    /// The workspace is not governed here or its governance could not be
    /// changed.
    pub async fn declare_owner(
        &self,
        workspace: &str,
        user: [u8; 32],
        name: Option<&str>,
    ) -> Result<(), CollaborationError> {
        let host = self.inner.host.cert.clone();
        let now = tilo_clock::unix_now_ns();
        self.set_member_administrator(workspace, &host, decision(b"owner", &user, now), user, name)
            .await?;
        self.set_member_keeper(workspace, &host, decision(b"keeper", &user, now), user)
            .await?;
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

/// A fresh administration request id per decision: the same person can be
/// promoted, demoted and promoted again over the workspace's life.
fn decision(kind: &[u8], key: &[u8; 32], now_ns: u64) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kind);
    hasher.update(key);
    hasher.update(&now_ns.to_le_bytes());
    *hasher.finalize().as_bytes()
}

#[cfg(test)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the server handle a test binds is kept until the test ends"
)]
mod tests {
    use super::*;
    use crate::workspaces::listing::entry_tests::walk_in_as;
    use crate::workspaces::tests::{governor, guest_device, test_host};
    use tilo_server_protocol::directory::{EntryJoin, Listing};
    use tilo_server_protocol::workspaces::{
        JoinReply, Workspace, WorkspacesReply, WorkspacesRequest,
    };

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

    fn asked(role: Role, administrator: bool, owner: bool) -> HostInvitation {
        HostInvitation {
            request_id: rand::random(),
            role,
            administrator,
            owner,
            invitee_name: None,
            expires_at_ns: tilo_clock::unix_now_ns().saturating_add(DAY_NS),
            wait_list_entry: None,
        }
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

    async fn is_administrator(workspaces: &Workspaces, workspace: &str, person: [u8; 32]) -> bool {
        workspaces
            .policy_of(workspace)
            .await
            .is_some_and(|policy| policy.is_administrator(&hex::encode(person)))
    }

    /// `device`'s person as a page's workspace lists them.
    fn row<'a>(page: &'a Workspace, device: &DeviceCert) -> &'a collab_types::Member {
        let person = hex::encode(device.user_public_key);
        page.members
            .iter()
            .find(|row| row.id.person_id() == Some(person.as_str()))
            .unwrap_or_else(|| panic!("{device:?} has a row in {page:?}"))
    }

    /// The host invites as itself: the invitation names no person who sent
    /// it, so the card reads as the workspace's, and the host is no member
    /// the card counts. Whoever accepts it is in with its role.
    #[tokio::test]
    async fn a_host_made_invitation_is_from_the_workspace_and_lets_a_newcomer_in() {
        let dir = tempfile::tempdir().unwrap();
        let host = governor();
        let server = site(dir.path(), &host).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Lounge").await.unwrap().workspace;

        for (admitted, (role, administrator)) in [
            (Role::Editor, false),
            (Role::Viewer, false),
            (Role::Editor, true),
        ]
        .into_iter()
        .enumerate()
        {
            let issued = workspaces
                .invite_as_host(&workspace, asked(role, administrator, false))
                .await
                .unwrap();
            let JoinReply::Invitation {
                inviter,
                members,
                role: offered,
                ..
            } = workspaces
                .inspect_invitation(&issued.signed_invitation)
                .await
            else {
                panic!("the host honours its own invitation");
            };
            assert_eq!(inviter, None, "a host-made invitation is from no person");
            assert_eq!(
                members, admitted,
                "the host is none of the people it counts"
            );
            assert_eq!(offered, role);
            let (person, reply) = accept(workspaces, issued.signed_invitation).await;
            assert!(matches!(reply, JoinReply::Admitted { .. }), "{reply:?}");
            assert_eq!(
                is_administrator(workspaces, &workspace, person).await,
                administrator
            );
        }
        assert!(
            !workspaces.has_owner(&workspace).await,
            "no invitation made an owner"
        );
        server.shutdown().await;
    }

    /// The owner link: an owner invitation, opened by someone whose key the
    /// host never knew, makes them an administrator the workspace is kept for
    /// as they come in, also after a restart; once someone did, the workspace
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
            .invite_as_host(&workspace, asked(Role::Viewer, false, true))
            .await
            .unwrap();
        server.shutdown().await;

        let server = site(dir.path(), &host).await;
        let workspaces = server.workspaces();
        let JoinReply::Invitation { inviter, role, .. } = workspaces
            .inspect_invitation(&issued.signed_invitation)
            .await
        else {
            panic!("the host honours its owner invitation");
        };
        assert_eq!(
            (inviter, role),
            (None, Role::Editor),
            "an owner joins as an editor"
        );

        let (owner, reply) = accept(workspaces, issued.signed_invitation.clone()).await;
        assert!(matches!(reply, JoinReply::Admitted { .. }), "{reply:?}");
        assert!(
            workspaces.has_owner(&workspace).await,
            "the workspace has its owner"
        );
        assert!(is_administrator(workspaces, &workspace, owner).await);
        let policy = workspaces.policy_of(&workspace).await.unwrap();
        assert_eq!(
            policy.keepers().collect::<Vec<_>>(),
            vec![hex::encode(owner)]
        );
        // Who the workspace lists as its owner (`tilo members ls`): the
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
            .invite_as_host(&lounge, asked(Role::Editor, false, true))
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
            .invite_as_host(&lounge, asked(Role::Editor, false, false))
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

        // And the workspace lets them change a person's role.
        let changed = workspaces
            .page_workspaces(
                &owner,
                WorkspacesRequest::SetRole {
                    workspace: lounge,
                    request_id: rand::random(),
                    user_public_key: member.user_public_key,
                    role: Role::Viewer,
                },
            )
            .await;
        let WorkspacesReply::Workspace(changed) = changed else {
            panic!("the owner may change a role: {changed:?}");
        };
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
            .invite_as_host(&elsewhere, asked(Role::Editor, false, false))
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
}
