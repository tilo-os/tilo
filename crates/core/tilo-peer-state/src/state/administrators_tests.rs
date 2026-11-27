//! A workspace's administrators govern it: a member given the administrator
//! grant holds every governance action of the workspace, on a node the Editor's
//! own rule on its resource does not outrank, so workspaces that held the grant
//! where it did are rewritten to govern through it.
use super::*;
use tilo_action_policy::KindSet;
use tilo_action_policy::roles::{self, governance};

/// How many of the workspace's governance actions the member with device
/// `device` may run, as `peer`'s workspace policy decides it now.
fn administers(peer: &StatePeer, device: [u8; 32], now: SystemTime) -> usize {
    let member = peer.permissions(device, now).unwrap();
    let authority = member.authority();
    let cx = tilo_action_policy::Context::at_time(now);
    governance::ALL
        .into_iter()
        .filter(|action| {
            member
                .policy
                .check_action(&authority, action, &cx, false)
                .is_ok()
        })
        .count()
}

const EVERY: usize = governance::ALL.len();

fn is_administrator(peer: &StatePeer, user: [u8; 32], now: SystemTime) -> bool {
    peer.governance()
        .member_list(now)
        .unwrap()
        .members
        .iter()
        .any(|member| member.root() == Some(user) && member.administrator)
}

tilo_test_env::scenario! {
    fn an_administrator_may_what_an_editor_may_not;

    phase an_administrator_administers_and_an_editor_does_not;
    {
        let (mut peer, host_key, _, _, now) = crate::admission_tests::seeded();
        let (editor, user) = crate::admission_tests::add_editor(&mut peer, &host_key, now);
        assert_eq!(administers(&peer, editor.device_public_key, now), 0);

        assert!(
            peer.governance_mut()
                .set_member_administrator(user, None, &host_key, now)
                .unwrap()
        );
        assert!(is_administrator(&peer, user, now));
        assert_eq!(administers(&peer, editor.device_public_key, now), EVERY);
        let held = peer.permissions(editor.device_public_key, now).unwrap();
        assert_eq!(
            held.policy.role_of(held.person_id().unwrap()),
            Some(Role::Editor)
        );

        // Setting a role takes the access back with the grant.
        peer.governance_mut()
            .set_member_role(user, crate::join::Role::Editor, &host_key, now)
            .unwrap();
        assert!(!is_administrator(&peer, user, now));
        assert_eq!(administers(&peer, editor.device_public_key, now), 0);
    }

    phase an_administrator_moderates_and_an_editor_does_not;
    // An administrator moderates the workspace as a Moderator does, an Editor not:
    // the grant comes and goes with the administrator's.
    {
        let (mut peer, host_key, _, _, now) = crate::admission_tests::seeded();
        let (editor, user) = crate::admission_tests::add_editor(&mut peer, &host_key, now);
        let node = TreePath::workspace(peer.workspace_id())
            .app("so.tilo.chat")
            .kind("chat.message")
            .resource("welcome")
            .field(roles::MODERATION_FIELD)
            .action("remove");
        let moderates = |peer: &StatePeer| {
            peer.permissions(editor.device_public_key, now)
                .unwrap()
                .allows(&node, tilo_action_policy::ActionKind::Undoable, now)
        };
        assert!(!moderates(&peer));
        assert!(
            peer.governance_mut()
                .set_member_administrator(user, None, &host_key, now)
                .unwrap()
        );
        assert!(moderates(&peer));
        peer.governance_mut()
            .set_member_role(user, crate::join::Role::Editor, &host_key, now)
            .unwrap();
        assert!(!moderates(&peer));
    }

    phase an_administrators_own_device_changes_a_role_and_an_editors_cannot;
    // A member writes the workspace's membership from their own device, as far as
    // their grants reach: an administrator's device changes a role itself, an
    // editor's cannot.
    {
        let (mut peer, host_key, _, _, now) = crate::admission_tests::seeded();
        let (editor, user) = crate::admission_tests::add_editor(&mut peer, &host_key, now);
        let editor_key = SigningKey::from_bytes(&[6; 32]);
        assert!(
            peer.clone()
                .governance_mut()
                .set_member_role(user, crate::join::Role::Viewer, &editor_key, now)
                .is_err()
        );
        peer.governance_mut()
            .set_member_administrator(user, None, &host_key, now)
            .unwrap();
        assert!(
            peer.governance_mut()
                .set_member_role(user, crate::join::Role::Viewer, &editor_key, now)
                .unwrap()
        );
        assert_eq!(
            peer.role(&editor.device_public_key, now).unwrap(),
            Role::Viewer
        );
        assert!(!is_administrator(&peer, user, now));
    }

    phase an_administrators_own_device_changes_the_workspaces_settings_and_an_editors_cannot;
    // A member changes the workspace's own settings from their own device as far as
    // their grants reach: an administrator renames the workspace, saves and assigns
    // its grant sets and grants and takes back its apps' sandbox capabilities;
    // an editor does none of it.
    {
        use crate::workspace_membership::SettingsChange;
        use collab_types::caps::{Access, CapOverride, Scope};
        use tilo_action_policy::Grantee;
        let (mut peer, host_key, _, _, now) = crate::admission_tests::seeded();
        let (editor, user) = crate::admission_tests::add_editor(&mut peer, &host_key, now);
        let rename = SettingsChange::Rename {
            name: "Plans".into(),
        };
        let capability = CapOverride::grant(
            Grantee::person(hex::encode(user)),
            Scope::NetHost("example.org".into()),
            Access::Read,
        )
        .through("so.tilo.reader");
        let sandbox = SettingsChange::GrantSandbox { capability };
        let save = SettingsChange::SavePreset { preset: readers() };
        for change in [rename.clone(), sandbox.clone(), save.clone()] {
            assert!(
                change_settings(&mut peer.clone(), &editor, change.clone(), now).is_err(),
                "an editor does not {change:?}"
            );
        }

        peer.governance_mut()
            .set_member_administrator(user, None, &host_key, now)
            .unwrap();
        assert!(change_settings(&mut peer, &editor, rename, now).unwrap());
        assert_eq!(peer.governance().workspace_name().as_deref(), Some("Plans"));
        assert!(change_settings(&mut peer, &editor, sandbox, now).unwrap());
        let granted = peer.governance().member_sandbox().unwrap();
        let [held] = granted.as_slice() else {
            panic!("one capability: {granted:?}");
        };
        assert_eq!(
            held.granted_by,
            Some(tilo_member_types::MemberId::person(hex::encode(user)))
        );
        assert!(change_settings(&mut peer, &editor, save, now).unwrap());
        assert!(
            change_settings(
                &mut peer,
                &editor,
                SettingsChange::AssignPreset {
                    preset: "Readers".into(),
                    subject: tilo_action_policy::Grantee::agent("scribe", "ada"),
                    lifetime: tilo_action_policy::Lifetime::UntilRevoked,
                },
                now,
            )
            .unwrap()
        );
        let policy = peer
            .permissions(editor.device_public_key, now)
            .unwrap()
            .policy;
        assert!(
            policy
                .grants
                .iter()
                .any(|grant| grant.grantee == tilo_action_policy::Grantee::agent("scribe", "ada"))
        );
        assert!(
            change_settings(
                &mut peer,
                &editor,
                SettingsChange::RevokeSandbox {
                    capability: held.id.clone(),
                },
                now,
            )
            .unwrap()
        );
        assert!(peer.governance().member_sandbox().unwrap().is_empty());
    }

    phase a_moderator_renames_the_workspace_and_a_grant_on_a_set_assigns_only_it;
    // A Moderator names the workspace by their role; a grant on one saved set's
    // assign node lets an editor assign that set, and nothing else.
    {
        use crate::workspace_membership::SettingsChange;
        let (mut peer, host_key, _, _, now) = crate::admission_tests::seeded();
        let (editor, user) = crate::admission_tests::add_editor(&mut peer, &host_key, now);
        peer.governance_mut()
            .put_saved_presets(&[readers()], &host_key, now)
            .unwrap();
        let assign = SettingsChange::AssignPreset {
            preset: "Readers".into(),
            subject: tilo_action_policy::Grantee::agent("scribe", "ada"),
            lifetime: tilo_action_policy::Lifetime::UntilRevoked,
        };
        assert!(change_settings(&mut peer.clone(), &editor, assign.clone(), now).is_err());
        let owner = hex::encode(peer.owner());
        let on_set = tilo_action_policy::Grant::new(
            "assign-readers",
            tilo_action_policy::Grantee::person(hex::encode(user)),
            tilo_action_policy::roles::preset_assign_node(peer.workspace_id(), "Readers"),
            KindSet::OUTSIDE,
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Grantee::person(owner),
        );
        peer.governance_mut()
            .put_member_grants(&[on_set], &host_key, now)
            .unwrap();
        assert!(change_settings(&mut peer, &editor, assign, now).unwrap());
        assert!(
            change_settings(
                &mut peer.clone(),
                &editor,
                SettingsChange::SavePreset { preset: readers() },
                now
            )
            .is_err(),
            "assigning one set saves none"
        );

        let rename = SettingsChange::Rename {
            name: "Plans".into(),
        };
        assert!(change_settings(&mut peer.clone(), &editor, rename.clone(), now).is_err());
        peer.governance_mut()
            .set_member_role(user, crate::join::Role::Moderator, &host_key, now)
            .unwrap();
        assert!(change_settings(&mut peer, &editor, rename, now).unwrap());
        assert_eq!(peer.governance().workspace_name().as_deref(), Some("Plans"));
    }
}

tilo_test_env::scenario! {
    fn a_members_own_device_signs_what_the_member_asks_of_the_membership;

    phase a_members_membership_command_is_signed_by_their_own_device_and_decided_by_their_grants;
    {
        use crate::workspace_membership::WorkspaceMembershipCommand;
        let (mut peer, host_key, _, _, now) = crate::admission_tests::seeded();
        let (editor, user) = crate::admission_tests::add_editor(&mut peer, &host_key, now);
        let editor_key = SigningKey::from_bytes(&[6; 32]);
        let demote = WorkspaceMembershipCommand::Member {
            change: tilo_action_policy::Change::SetRole {
                person: hex::encode(user),
                role: Role::Viewer,
            },
        };
        assert!(
            peer.clone()
                .workspace_mut()
                .apply_workspace_membership_command(&editor, &demote, &editor_key, now)
                .is_err(),
            "an editor's grants change no role"
        );
        peer.governance_mut()
            .set_member_administrator(user, None, &host_key, now)
            .unwrap();
        assert!(
            peer.clone()
                .workspace_mut()
                .apply_workspace_membership_command(
                    &editor,
                    &demote,
                    &SigningKey::from_bytes(&[7; 32]),
                    now
                )
                .is_err(),
            "another key does not sign for the caller's device"
        );
        assert!(
            peer.workspace_mut()
                .apply_workspace_membership_command(&editor, &demote, &editor_key, now)
                .unwrap()
        );
        assert_eq!(
            peer.role(&editor.device_public_key, now).unwrap(),
            Role::Viewer
        );
    }

    phase a_members_own_device_leaves_the_workspace;
    // An administrator's own device lets an app author the workspace, as a grant
    // of their own; an editor's cannot, and letting it again changes nothing.
    // A member leaves the workspace on their own device: leaving is nobody's
    // grant, and the workspace they leave takes their device rows with them.
    {
        use crate::workspace_membership::WorkspaceMembershipCommand;
        let (mut peer, host_key, _, _, now) = crate::admission_tests::seeded();
        let (editor, user) = crate::admission_tests::add_editor(&mut peer, &host_key, now);
        let editor_key = SigningKey::from_bytes(&[6; 32]);
        let leave = WorkspaceMembershipCommand::Member {
            change: tilo_action_policy::Change::RemoveMember {
                person: hex::encode(user),
            },
        };
        assert!(peer.permissions(editor.device_public_key, now).is_ok());
        assert!(
            peer.workspace_mut()
                .apply_workspace_membership_command(&editor, &leave, &editor_key, now)
                .unwrap(),
            "the editor left"
        );
        assert!(
            peer.permissions(editor.device_public_key, now).is_err(),
            "a device whose person left is no member's"
        );
        assert!(
            peer.governance()
                .member_list(now)
                .unwrap()
                .members
                .iter()
                .all(|member| member.root() != Some(user)),
            "the member and their device rows are off the roster"
        );
    }
}

/// The workspace's own settings an editor's device writes as `change`.
fn change_settings(
    peer: &mut StatePeer,
    editor: &DeviceCert,
    change: crate::workspace_membership::SettingsChange,
    now: SystemTime,
) -> Result<bool, String> {
    peer.workspace_mut().apply_workspace_membership_command(
        editor,
        &crate::workspace_membership::WorkspaceMembershipCommand::Settings(change),
        &SigningKey::from_bytes(&[6; 32]),
        now,
    )
}

fn readers() -> tilo_action_policy::Preset {
    tilo_action_policy::Preset::new(
        "Readers",
        "Read notes.",
        vec![tilo_action_policy::Rule::new(
            tilo_action_policy::TreePath::root().app("so.tilo.notes"),
            KindSet::READ,
            tilo_action_policy::Answer::Allow,
        )],
    )
}

mod made_and_invited {
    //! Who makes administrators, as every member replays it: the owner and
    //! administrators make them; only the owner unmakes them; an invitation
    //! makes its holder an administrator only when whom it carries the making
    //! of may.
    use super::*;
    use crate::join::{
        InviteGrant, Inviter, Role as JoinRole, VerifiedRedemption, issue_administrator_invitation,
    };

    /// A device of the root seeded `root`, its key seeded `device`.
    fn device(root: u8, device: u8) -> (SigningKey, DeviceCert) {
        let root = SigningKey::from_bytes(&[root; 32]);
        let key = SigningKey::from_bytes(&[device; 32]);
        let cert = crate::fixture::cert(
            &root,
            &key,
            tilo_user_identity::DeviceCapabilities::member(),
        );
        (key, cert)
    }

    /// `joiner` redeeming an administrator's invitation `issuer` signed for
    /// `inviter`.
    fn administrator_redemption(
        peer: &StatePeer,
        (issuer_key, issuer): (&SigningKey, &DeviceCert),
        inviter: [u8; 32],
        (key, joiner): &(SigningKey, DeviceCert),
        nonce: u8,
        now: SystemTime,
    ) -> VerifiedRedemption {
        let now_ns = now_ns(now);
        let signed = issue_administrator_invitation(
            InviteGrant {
                workspace_id: crate::join::WorkspaceId::parse(peer.workspace_id()).unwrap(),
                issuer: issuer.clone(),
                role: JoinRole::Editor,
                nonce: [nonce; 16],
                recipient_user_public_key: None,
                invitee_name: None,
                expires_at_ns: now_ns.checked_add(10_000_000_000).unwrap(),
                transport: vec![],
            },
            Some(Inviter {
                user_public_key: inviter,
                name: "Inviter".into(),
            }),
            issuer_key,
        )
        .unwrap();
        crate::fixture::redeem(&signed, (key, joiner), nonce, now)
    }

    /// A workspace its host founded and handed to the Editor it seeded,
    /// staying its administrator: the host's key, the owner's device key
    /// and root.
    fn handed() -> (StatePeer, SigningKey, SigningKey, [u8; 32], SystemTime) {
        let (mut peer, host_key, _, _, now) = crate::admission_tests::seeded();
        let (_, owner) = crate::admission_tests::add_editor(&mut peer, &host_key, now);
        assert!(
            peer.governance_mut()
                .transfer_ownership(owner, &host_key, now)
                .unwrap()
        );
        (peer, host_key, SigningKey::from_bytes(&[6; 32]), owner, now)
    }

    tilo_test_env::scenario! {
        fn an_administrator_is_made_by_the_owner_or_by_an_invitation;

        phase the_owner_makes_an_administrator_who_makes_another_and_only_the_owner_unmakes;
        {
            let (mut peer, host_key, owner_key, owner, now) = handed();
            let (bob_key, _, bob) =
                crate::admission_tests::add_member_with(&mut peer, &host_key, now, 30, 31);
            let (_, _, dee) =
                crate::admission_tests::add_member_with(&mut peer, &host_key, now, 32, 33);
            let (eve_key, _, eve) =
                crate::admission_tests::add_member_with(&mut peer, &host_key, now, 34, 35);
            let policy = |peer: &StatePeer| peer.governance().member_policy().unwrap();
            assert_eq!(policy(&peer).owner(), Some(hex::encode(owner).as_str()));

            // An Editor makes nobody an administrator, nor takes the workspace.
            assert!(
                peer.clone()
                    .governance_mut()
                    .set_member_administrator(dee, None, &eve_key, now)
                    .is_err()
            );
            assert!(
                peer.clone()
                    .governance_mut()
                    .transfer_ownership(eve, &eve_key, now)
                    .is_err()
            );

            // The owner's own device makes Bob one, and Bob makes Dee one.
            assert!(
                peer.governance_mut()
                    .set_member_administrator(bob, None, &owner_key, now)
                    .unwrap()
            );
            assert!(is_administrator(&peer, bob, now));
            assert!(
                peer.governance_mut()
                    .set_member_administrator(dee, None, &bob_key, now)
                    .unwrap()
            );
            assert!(is_administrator(&peer, dee, now));
            let made = policy(&peer);
            let given = made
                .grants
                .iter()
                .find(|grant| grant.id == roles::administrator_grant_id(&hex::encode(dee)))
                .unwrap();
            assert_eq!(
                given.granted_by,
                Grantee::person(hex::encode(bob)),
                "Bob gave it"
            );

            // Bob unmakes and removes neither Dee nor the owner.
            for person in [dee, owner] {
                assert!(
                    peer.clone()
                        .governance_mut()
                        .set_member_role(person, crate::join::Role::Editor, &bob_key, now)
                        .is_err()
                );
            }
            // The owner unmakes Dee, who administers nothing from then on.
            peer.governance_mut()
                .set_member_role(dee, crate::join::Role::Editor, &owner_key, now)
                .unwrap();
            assert!(!is_administrator(&peer, dee, now));
            let dee_device = SigningKey::from_bytes(&[33; 32]).verifying_key().to_bytes();
            assert_eq!(administers(&peer, dee_device, now), 0);
            assert!(
                !peer
                    .governance()
                    .may_invite_administrator(&SigningKey::from_bytes(&[33; 32]), now)
            );
            assert!(peer.governance().may_invite_administrator(&bob_key, now));
            assert!(peer.governance().may_invite_administrator(&owner_key, now));
            assert!(!peer.governance().may_invite_administrator(&eve_key, now));
        }

        phase an_administrator_invitation_makes_one_only_when_its_inviter_may;
        {
            let (mut peer, host_key, owner_key, owner, now) = handed();
            let host = peer
                .certificates()
                .into_iter()
                .find(|cert| cert.device_public_key == host_key.verifying_key().to_bytes())
                .unwrap();
            let (eve_key, eve_cert, eve) =
                crate::admission_tests::add_member_with(&mut peer, &host_key, now, 34, 35);

            // The host signs for the owner: the guest enters as an administrator,
            // given by the owner, and a fresh replica reads it so.
            let guest = device(40, 41);
            let invited = administrator_redemption(&peer, (&host_key, &host), owner, &guest, 42, now);
            assert!(
                peer.governance_mut()
                    .admit_invited_member(&invited, &guest.0, now)
                    .unwrap()
            );
            assert!(is_administrator(&peer, guest.1.user_public_key, now));
            assert_eq!(administers(&peer, guest.1.device_public_key, now), EVERY);
            let replica = peer.governance().member_policy().unwrap();
            let guest_person = hex::encode(guest.1.user_public_key);
            assert!(replica.is_administrator(&guest_person));
            assert_eq!(replica.role_of(&guest_person), Some(Role::Editor));

            // Escalated: the host signs one for Eve, an Editor who may not make
            // administrators. It admits nobody.
            let escalated = device(43, 44);
            let refused = administrator_redemption(&peer, (&host_key, &host), eve, &escalated, 45, now);
            let error = peer
                .clone()
                .governance_mut()
                .admit_invited_member(&refused, &escalated.0, now)
                .unwrap_err();
            assert!(error.contains("may not make"), "{error}");
            // Forged: Eve's own device signs one. It admits nobody either.
            let forged = device(46, 47);
            let by_eve = administrator_redemption(&peer, (&eve_key, &eve_cert), eve, &forged, 48, now);
            assert!(
                peer.clone()
                    .governance_mut()
                    .admit_invited_member(&by_eve, &forged.0, now)
                    .is_err()
            );
            assert!(peer.permissions(forged.1.device_public_key, now).is_err());

            // An administrator the owner unmakes invites no administrator after.
            let guest_key = &guest.0;
            peer.governance_mut()
                .set_member_role(
                    guest.1.user_public_key,
                    crate::join::Role::Editor,
                    &owner_key,
                    now,
                )
                .unwrap();
            let late = device(49, 50);
            let after = administrator_redemption(
                &peer,
                (&host_key, &host),
                guest.1.user_public_key,
                &late,
                51,
                now,
            );
            assert!(
                peer.clone()
                    .governance_mut()
                    .admit_invited_member(&after, guest_key, now)
                    .is_err()
            );
            assert!(
                peer.clone()
                    .governance_mut()
                    .admit_invited_member(&after, &host_key, now)
                    .is_err()
            );
        }
    }
}
