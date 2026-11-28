//! An owner invitation is an invitation whose grant names the owner's place
//! ([`tilo_peer_state::join::InviteGrant::makes_owner`]): a device of the workspace's owner
//! signs it, and whoever redeems it takes the workspace over as they join. This is where the
//! workspace's owner is read, and where it is handed over.

use super::{CollaborationError, Workspaces};

impl Workspaces {
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
    /// The workspace is not hosted here, this host no longer owns it, or its
    /// governance could not be changed.
    pub async fn declare_owner(
        &self,
        workspace: &str,
        user: [u8; 32],
        name: Option<&str>,
    ) -> Result<(), CollaborationError> {
        let hosted = self.workspace(workspace).await?;
        let mut hosted = hosted.lock().await;
        self.give_ownership(&mut hosted, user, name)
            .await
            .map_err(CollaborationError::new)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::workspaces::listing::entry_tests::walk_in_as;
    use crate::workspaces::tests::{device_cert, guest_device, hosting_device, test_host};
    use ed25519_dalek::SigningKey;
    use tilo_peer_state::join::Role;
    use tilo_server_protocol::directory::EntryJoin;
    use tilo_server_protocol::workspaces::{
        IssueInvitationRequest, JoinReply, Workspace, WorkspacesReply, WorkspacesRequest,
    };
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList};

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

    /// `device`'s person as a page's workspace lists them.
    fn row<'a>(page: &'a Workspace, device: &DeviceCert) -> &'a collab_types::Member {
        let person = hex::encode(device.user_public_key);
        page.members
            .iter()
            .find(|row| row.id.person_id() == Some(person.as_str()))
            .unwrap_or_else(|| panic!("{device:?} has a row in {page:?}"))
    }

    /// The owner's place is given by an owner invitation: making a person whose first device has
    /// not joined the owner records it there, the owner it has keeps it and still acts, the member
    /// list shows the person by the place they will take, and their first device's join hands the
    /// workspace over, the host it came from staying as an administrator. A person's own device
    /// asking for an invitation names them on the card, and the owner reads as one on its pages.
    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "one hand-over, its invitation, the devices that ask for one, and its pages"
    )]
    async fn an_owner_invitation_makes_whoever_accepts_it_the_owner() {
        let dir = tempfile::tempdir().unwrap();
        let host = hosting_device();
        let server = site(dir.path(), &host).await;
        let workspaces = server.workspaces();
        let workspace = workspaces.create("Lounge").await.unwrap().workspace;

        // The owner gives the place to a person no device of theirs has joined with: it is
        // recorded on an owner invitation, and the workspace keeps the owner it has, who still
        // hands a workspace on, which only an owner does.
        let root = SigningKey::from_bytes(&rand::random());
        let (newcomer_key, newcomer) = device_cert(&root, "guest", DeviceCapabilities::member());
        workspaces
            .add_member(
                &workspace,
                &host.1,
                crate::workspaces::AddMemberRequest {
                    request_id: [21; 32],
                    user_public_key: newcomer.user_public_key,
                    place: crate::workspaces::MemberPlace::Owner,
                    name: None,
                },
            )
            .await
            .expect("the owner gives the place to a person it knows by key");
        assert!(
            workspaces
                .is_owner(&workspace, host.1.user_public_key)
                .await
        );
        workspaces
            .issue_as(
                &workspace,
                &host.1,
                asked([22; 32], Role::Owner, Some(newcomer.user_public_key)),
                false,
            )
            .await
            .expect("the owner still hands the workspace on");
        let listed = workspaces.members(&workspace).await.unwrap();
        let listed_row = listed
            .iter()
            .find(|row| row["actor"]["member"]["person"] == hex::encode(newcomer.user_public_key))
            .unwrap_or_else(|| panic!("the person the invitation names is listed: {listed:?}"));
        assert_eq!(listed_row["owner"], false, "{listed:?}");
        assert_eq!(listed_row["owner_when_joined"], true, "{listed:?}");
        server.shutdown().await;

        // A restart keeps the invitation, and the card says what opening it makes.
        let server = site(dir.path(), &host).await;
        let workspaces = server.workspaces();
        let signed = {
            let invite_id = workspaces
                .pending(&workspace, &host.1)
                .await
                .unwrap()
                .into_iter()
                .find(|invitation| {
                    invitation.recipient_user_public_key == Some(newcomer.user_public_key)
                        && invitation.owner
                })
                .expect("the place is recorded on an owner invitation for them")
                .invite_id;
            let hosted = workspaces.workspace(&workspace).await.unwrap();
            let hosted = hosted.lock().await;
            hosted
                .collaboration()
                .open_invitation(invite_id, tilo_clock::unix_now_ns())
                .expect("the invitation is still open")
                .0
                .signed_invitation
        };
        let JoinReply::Invitation {
            inviter,
            members,
            role,
            administrator,
            owner,
            ..
        } = workspaces.inspect_invitation(&signed).await
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

        // Their first device redeems it: the workspace is theirs, and the owner it had is one of
        // its administrators.
        let reply = workspaces
            .admit_proven(
                signed.clone(),
                (newcomer.clone(), &newcomer_key),
                rand::random(),
                "",
                None,
            )
            .await;
        assert!(matches!(reply, JoinReply::Admitted { .. }), "{reply:?}");
        let owner = newcomer.user_public_key;
        let policy = workspaces.policy_of(&workspace).await.unwrap();
        assert_eq!(policy.owner(), Some(hex::encode(owner).as_str()));
        assert!(
            policy.is_administrator(&hex::encode(host.1.user_public_key)),
            "the host it was handed from serves it as an administrator"
        );

        // A further device of the owner's asks for an invitation: it asks as its person, and the
        // card reads the name the workspace shows them by now, not the one it was signed with.
        let (second_key, second) = device_cert(&root, "", DeviceCapabilities::member());
        let second_row = workspaces
            .issue_as(
                &workspace,
                &newcomer,
                asked([25; 32], Role::Editor, Some(owner)),
                false,
            )
            .await
            .expect("the owner invites a further device of theirs");
        let joined_row = workspaces
            .admit_proven(
                second_row.signed_invitation,
                (second.clone(), &second_key),
                rand::random(),
                "",
                None,
            )
            .await;
        assert!(
            matches!(joined_row, JoinReply::Admitted { .. }),
            "{joined_row:?}"
        );
        let second_link = workspaces
            .issue_as(
                &workspace,
                &second,
                asked([23; 32], Role::Editor, Some(owner)),
                true,
            )
            .await
            .expect("the owner's own device still asks as a person");
        assert!(
            opens(&second_link.signed_invitation).inviter().is_some(),
            "a device of the person still asks as a person"
        );
        workspaces
            .name_self(&workspace, &newcomer, "Quinten", tilo_clock::unix_now_ns())
            .await
            .unwrap();
        let JoinReply::Invitation { inviter, .. } = workspaces
            .inspect_invitation(&second_link.signed_invitation)
            .await
        else {
            panic!("the invitation is still honoured");
        };
        let from = inviter.expect("the card says who asked");
        assert_eq!(from.user_public_key, owner);
        assert_eq!(from.name, "Quinten", "the name the workspace shows them by");

        // The owner reads as the owner on their page, their own row is theirs, and they change a
        // member's role.
        let invite = workspaces
            .issue(&workspace, &newcomer, invitation(Role::Editor))
            .await
            .unwrap();
        let (member_key, member) = guest_device();
        let admitted = workspaces
            .admit_proven(
                invite.signed_invitation,
                (member.clone(), &member_key),
                rand::random(),
                "Grace",
                None,
            )
            .await;
        assert!(
            matches!(admitted, JoinReply::Admitted { .. }),
            "{admitted:?}"
        );
        let WorkspacesReply::Workspaces(listed) = workspaces
            .page_workspaces(&newcomer, WorkspacesRequest::List)
            .await
        else {
            panic!("the page lists its workspaces");
        };
        let page = listed
            .into_iter()
            .find(|page| page.workspace == workspace)
            .expect("the Lounge is listed");
        assert_eq!(row(&page, &newcomer).role, Some(Role::Owner), "{page:?}");
        assert!(row(&page, &newcomer).is_self, "their own row is theirs");
        assert!(!row(&page, &member).is_self, "nobody else's row is");
        let command: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand =
            tilo_action_policy::Change::SetRole {
                person: hex::encode(member.user_public_key),
                role: Role::Viewer,
            }
            .into();
        crate::workspaces::fixtures::signed_by(&newcomer, &newcomer_key, |ask| {
            workspaces.govern(&workspace, ask, &command)
        })
        .await
        .expect("the owner may change a role");

        // An owner invitation is only its owner's to issue: the host it was handed from
        // administers this workspace now, and its owner invitation admits nobody.
        assert!(
            workspaces
                .issue_as(
                    &workspace,
                    &host.1,
                    asked([24; 32], Role::Owner, Some(member.user_public_key)),
                    false,
                )
                .await
                .is_err(),
            "an administrator's owner invitation admits nobody"
        );
        let again = workspaces
            .admit_proven(
                signed,
                (newcomer.clone(), &newcomer_key),
                rand::random(),
                "Ada",
                None,
            )
            .await;
        assert!(
            matches!(again, JoinReply::Refused(_)),
            "one owner link, one owner: {again:?}"
        );
        server.shutdown().await;
    }

    /// Someone invited into any workspace on the host is past its home's
    /// invite-only entry: whoever invited them let them in, and the home is
    /// where everyone on the host is.
    #[tokio::test]
    async fn an_invitation_anywhere_on_the_host_opens_the_invite_only_home() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = hosting_device();
        let server = test_host(dir.path(), &key, &cert).await;
        let workspaces = server.workspaces();
        assert!(
            workspaces
                .offer_own_workspaces(crate::workspaces::OwnWorkspaceOffer::free("tilo.test"))
                .unwrap()
        );
        let home = workspaces.create("Lounge").await.unwrap().workspace;
        workspaces
            .found_entry(&home, &cert, |entry| {
                entry.listing = Some(tilo_action_policy::ListingPolicy {
                    description: "Everyone".into(),
                    default_role: Role::Editor,
                });
            })
            .await
            .unwrap();
        workspaces
            .found_entry(&home, &cert, |entry| entry.invitation_only = true)
            .await
            .unwrap();
        let elsewhere = workspaces.create("Acme").await.unwrap().workspace;
        let issued = workspaces
            .issue(&elsewhere, &cert, invitation(Role::Editor))
            .await
            .unwrap();
        let (newcomer_key, newcomer) = guest_device();
        let join = EntryJoin {
            user_public_key: hex::encode(newcomer.user_public_key),
            display_name: "Ada".into(),
        };
        assert!(
            walk_in_as(workspaces, &home, &join, (&newcomer_key, &newcomer))
                .await
                .is_err(),
            "a stranger is turned away from the home"
        );
        let reply = workspaces
            .admit_proven(
                issued.signed_invitation,
                (newcomer.clone(), &newcomer_key),
                rand::random(),
                "Ada",
                None,
            )
            .await;
        assert!(matches!(reply, JoinReply::Admitted { .. }), "{reply:?}");
        let reply = walk_in_as(workspaces, &home, &join, (&newcomer_key, &newcomer))
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
        let governing = hosting_device();
        let site = site(dir.path(), &governing).await;
        let workspaces = site.workspaces();
        let workspace = workspaces.create("Lounge").await.unwrap().workspace;
        workspaces
            .found_entry(&workspace, &governing.1, |entry| {
                entry.listing = Some(tilo_action_policy::ListingPolicy {
                    description: "The Tilo community".into(),
                    default_role: Role::Editor,
                });
            })
            .await
            .unwrap();
        let (newcomer_key, newcomer) = guest_device();
        let join = EntryJoin {
            user_public_key: hex::encode(newcomer.user_public_key),
            display_name: "Ada".into(),
        };
        let reply = walk_in_as(workspaces, &workspace, &join, (&newcomer_key, &newcomer))
            .await
            .unwrap();
        assert!(matches!(reply, JoinReply::Admitted { .. }), "{reply:?}");
        let may_invite = || WorkspacesRequest::Permits {
            workspace: workspace.clone(),
            node: tilo_action_policy::roles::governance_action(
                &workspace,
                tilo_action_policy::roles::governance::ADD_MEMBER,
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
