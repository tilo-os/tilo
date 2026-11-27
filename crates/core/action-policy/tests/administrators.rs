//! A workspace's administrator: an Editor given every governance action.
//! They administer every member who is not an administrator, an Editor by role
//! alone administers nothing, and a foreign device, an agent, a ban and a
//! restriction still bound them. Administrators are made by the owner and
//! other administrators, and unmade by the owner alone.

use tilo_action_policy::ActingPolicy as _;
use tilo_action_policy::{
    ActionKind, Answer, Authority, Change, Context, Grant, Grantee, KindSet, Refusal, Role,
    TreePath, WorkspacePolicy,
    roles::{self, governance},
};
use tilo_member_types::Acting;

const NOW: u64 = 1_000_000;
const OWNER: &str = "a11ce";
const BOB: &str = "b0b";
const CAROL: &str = "ca401";

fn cx() -> Context<'static> {
    Context::at(NOW)
}

/// Workspace `r` owned by [`OWNER`] with `members`, and `administrator` made its
/// administrator by the owner.
fn workspace(members: &[(&str, Role)], administrator: &str) -> WorkspacePolicy {
    let mut workspace = WorkspacePolicy::new("r", OWNER);
    for (person, role) in members {
        workspace.members.insert((*person).to_owned(), *role);
    }
    workspace.grants.push(roles::administrator_grant(
        "r",
        administrator,
        Grantee::person(OWNER),
    ));
    workspace
}

tilo_test_env::scenario! {
    fn an_administrator_governs_within_their_role;

    phase an_administrator_runs_every_governance_action_and_an_editor_none;
    {
        let workspace = workspace(&[(BOB, Role::Editor), (CAROL, Role::Editor)], CAROL);
        let bob = Authority::person(workspace.party(BOB));
        let carol = Authority::person(workspace.party(CAROL));
        for action in governance::ALL {
            assert_eq!(
                workspace.check_action(&carol, action, &cx(), false),
                Ok(()),
                "an administrator runs {action}"
            );
            assert!(
                matches!(
                    workspace.check_action(&bob, action, &cx(), true),
                    Err(Refusal::NotPermitted(_))
                ),
                "an Editor by role alone does not run {action}"
            );
        }
        // The grant reaches governance only: content stays the Editor's.
        let send = TreePath::workspace("r")
            .app("mail")
            .kind("mail.message")
            .resource("m1")
            .action("send");
        assert_eq!(
            carol.decide(&send, ActionKind::Outside, &cx()).answer,
            Answer::Ask
        );
    }

    phase an_administrator_hands_out_every_role_and_reaches_every_member_but_administrators;
    {
        let mut workspace = workspace(
            &[
                (BOB, Role::Editor),
                (CAROL, Role::Editor),
                ("m", Role::Moderator),
                ("v", Role::Viewer),
                ("dee", Role::Editor),
            ],
            CAROL,
        );
        workspace
            .grants
            .push(roles::administrator_grant("r", "dee", Grantee::person(OWNER)));
        let carol = Authority::person(workspace.party(CAROL));
        for role in [Role::Viewer, Role::Commenter, Role::Editor, Role::Moderator] {
            workspace
                .check_admission(&carol, role, &cx(), false)
                .unwrap();
        }
        assert_eq!(
            workspace.check_admission(&carol, Role::Owner, &cx(), false),
            Err(Refusal::OwnerRole)
        );
        let set = |person: &str, role| Change::SetRole {
            person: person.into(),
            role,
        };
        workspace
            .check(CAROL, &carol, &set("v", Role::Editor), &cx(), false)
            .unwrap();
        workspace
            .check(CAROL, &carol, &set(BOB, Role::Moderator), &cx(), false)
            .unwrap();
        let remove = |person: &str| Change::RemoveMember {
            person: person.into(),
        };
        // Editors and Moderators alike: every member who is no administrator.
        for person in ["v", BOB, "m"] {
            workspace
                .check(CAROL, &carol, &remove(person), &cx(), false)
                .unwrap();
        }
        // Another administrator is the owner's to remove.
        assert!(matches!(
            workspace.check(CAROL, &carol, &remove("dee"), &cx(), false),
            Err(Refusal::Administrators { unmaking: true, .. })
        ));
        assert_eq!(
            workspace.check(CAROL, &carol, &remove(OWNER), &cx(), false),
            Err(Refusal::Owner)
        );
        // A grant beyond what the administrator holds on its node.
        let wider = Grant::new(
            "wider",
            Grantee::person(BOB),
            workspace.node(),
            KindSet::OUTSIDE,
            Answer::Allow,
            Grantee::person(CAROL),
        );
        assert!(matches!(
            workspace.check(CAROL, &carol, &Change::Grant(wider), &cx(), false),
            Err(Refusal::AboveActorGrant { .. })
        ));
    }

    phase an_administrator_governs_nothing_from_a_foreign_device_or_through_an_agent;
    {
        let workspace = workspace(&[(CAROL, Role::Editor)], CAROL);
        let foreign = workspace.person_on_device(CAROL, true);
        let delegation = Acting::handed_agent("scribe", CAROL).handed_preset("r");
        let acting = Acting::agent("scribe", CAROL).acting_for(Acting::person(CAROL));
        let agent = Authority::of_acting(&acting, &workspace, &delegation);
        for action in governance::ALL {
            assert!(
                workspace
                    .check_action(&foreign, action, &cx(), true)
                    .is_err()
            );
            assert!(workspace.check_action(&agent, action, &cx(), true).is_err());
        }
    }

    phase a_ban_or_restriction_stops_whatever_governance_a_member_holds;
    {
        let until = NOW + 60_000;
        let mut restricted = workspace(&[(CAROL, Role::Editor), (BOB, Role::Moderator)], CAROL);
        for person in [CAROL, BOB] {
            restricted.grants.push(roles::restriction_grant(
                "r",
                person,
                until,
                Grantee::person(OWNER),
            ));
        }
        let carol = Authority::person(restricted.party(CAROL));
        let bob = Authority::person(restricted.party(BOB));
        assert_eq!(
            restricted.check_action(&carol, governance::ADD_MEMBER, &cx(), true),
            Err(Refusal::Restricted(CAROL.into()))
        );
        assert_eq!(
            restricted.check_action(&bob, governance::BAN, &cx(), true),
            Err(Refusal::Restricted(BOB.into())),
            "a Moderator's own actions do not outlast a restriction either"
        );
        assert_eq!(
            restricted.check_action(&carol, governance::ADD_MEMBER, &Context::at(until), false),
            Ok(()),
            "a lapsed restriction holds nobody off"
        );

        let mut banned = workspace(&[(CAROL, Role::Editor)], CAROL);
        banned
            .grants
            .push(roles::block_grant("r", CAROL, Grantee::person(OWNER)));
        let carol = Authority::person(banned.party(CAROL));
        assert_eq!(
            banned.check_action(&carol, governance::ADD_MEMBER, &cx(), true),
            Err(Refusal::Banned(CAROL.into()))
        );
        let owner = Authority::person(banned.party(OWNER));
        assert_eq!(
            banned.check_action(&owner, governance::ADD_MEMBER, &cx(), false),
            Ok(())
        );
    }

    phase an_administrator_grant_on_the_workspace_resource_is_outranked_by_the_editor_rule;
    {
        let mut workspace = WorkspacePolicy::new("r", OWNER);
        workspace.members.insert(CAROL.into(), Role::Editor);
        let legacy = Grant::new(
            roles::administrator_grant_id(CAROL).as_str(),
            Grantee::person(CAROL),
            roles::workspace_resource("r"),
            KindSet::ALL,
            Answer::Allow,
            Grantee::person(OWNER),
        );
        workspace.grants.push(legacy);
        let carol = Authority::person(workspace.party(CAROL));
        assert!(matches!(
            workspace.check_action(&carol, governance::ADD_MEMBER, &cx(), false),
            Err(Refusal::NotPermitted(_))
        ));
    }
}

/// Workspace `r` owned by [`OWNER`] with [`ADMIN`] its administrator, as a
/// host administers a person's workspace, and `members` besides.
fn administered(members: &[(&str, Role)]) -> WorkspacePolicy {
    let mut workspace = WorkspacePolicy::new("r", OWNER);
    workspace.members.insert(ADMIN.to_owned(), Role::Editor);
    for (person, role) in members {
        workspace.members.insert((*person).to_owned(), *role);
    }
    for grant in [
        roles::administrator_grant("r", ADMIN, Grantee::person(OWNER)),
        roles::administrator_moderation_grant("r", ADMIN, Grantee::person(OWNER)),
    ] {
        workspace.grants.push(grant);
    }
    workspace
}

const ADMIN: &str = "4ee9e4";

/// `actor` making `person` an administrator: both grants, as a workspace's
/// governance writes them.
fn make(workspace: &mut WorkspacePolicy, actor: &str, person: &str) -> Result<(), Refusal> {
    for grant in [
        roles::administrator_grant("r", person, Grantee::person(actor)),
        roles::administrator_moderation_grant("r", person, Grantee::person(actor)),
    ] {
        let held = workspace.clone();
        let authority = Authority::person(held.party(actor));
        workspace.apply(actor, &authority, Change::Grant(grant), &cx(), false)?;
    }
    Ok(())
}

tilo_test_env::scenario! {
    fn administrators_are_made_by_those_who_may_and_unmade_by_the_owner;

    phase the_owner_and_administrators_make_administrators_and_nobody_else;
    {
        let workspace = administered(&[(BOB, Role::Editor), (CAROL, Role::Moderator), ("v", Role::Viewer)]);
        for (person, may) in [(OWNER, true), (ADMIN, true), (BOB, false), (CAROL, false), ("v", false)] {
            let authority = Authority::person(workspace.party(person));
            assert_eq!(
                workspace.may_make_administrators(&authority, &cx()),
                may,
                "{person} makes administrators"
            );
        }
        assert_eq!(workspace.administrators().collect::<Vec<_>>(), [ADMIN]);

        let mut made = workspace.clone();
        make(&mut made, ADMIN, BOB).unwrap();
        assert!(made.is_administrator(BOB));
        // An administrator an administrator made makes another in turn.
        made.members.insert("dee".into(), Role::Editor);
        make(&mut made, BOB, "dee").unwrap();
        assert!(made.is_administrator("dee"));

        // An Editor and a Moderator by role alone do not.
        for actor in [BOB, CAROL] {
            let mut tried = workspace.clone();
            assert!(make(&mut tried, actor, "v").is_err(), "{actor} made an administrator");
            assert!(!tried.is_administrator("v"));
        }
    }

    phase an_administrator_never_unmakes_another_and_the_owner_does;
    {
        let mut workspace = administered(&[(BOB, Role::Editor), ("dee", Role::Editor)]);
        make(&mut workspace, ADMIN, BOB).unwrap();
        make(&mut workspace, ADMIN, "dee").unwrap();
        let unmaking = [
            Change::Revoke(roles::administrator_grant_id("dee")),
            Change::Revoke(roles::administrator_moderation_grant_id("dee")),
            Change::SetRole { person: "dee".into(), role: Role::Viewer },
            Change::RemoveMember { person: "dee".into() },
            Change::Ban { person: "dee".into() },
            Change::Restrict { person: "dee".into(), until_ms: NOW + 1 },
        ];
        let bob = Authority::person(workspace.party(BOB));
        for change in &unmaking {
            assert!(
                matches!(
                    workspace.check(BOB, &bob, change, &cx(), false),
                    Err(Refusal::Administrators { unmaking: true, .. })
                ),
                "an administrator {change:?}"
            );
        }
        // Nor the administrator who made them: only the owner unmakes one.
        assert!(workspace
            .check(BOB, &bob, &Change::Revoke(roles::administrator_grant_id(ADMIN)), &cx(), false)
            .is_err());
        assert!(workspace
            .check(BOB, &bob, &Change::RemoveMember { person: ADMIN.into() }, &cx(), false)
            .is_err());
        // Giving up one's own administration is anyone's.
        workspace
            .check(BOB, &bob, &Change::Revoke(roles::administrator_grant_id(BOB)), &cx(), false)
            .unwrap();
        let owner = Authority::person(workspace.party(OWNER));
        for change in &unmaking {
            assert_eq!(
                workspace.check(OWNER, &owner, change, &cx(), false),
                Ok(()),
                "the owner {change:?}"
            );
        }
        let admin = Authority::person(workspace.party(ADMIN));
        assert!(!workspace.may_unmake_administrators(ADMIN, &admin, &cx()));
        for change in &unmaking {
            assert!(
                workspace.check(ADMIN, &admin, change, &cx(), false).is_err(),
                "the administrator who made them {change:?}"
            );
        }
    }

    phase a_removed_administrator_holds_nothing_of_it;
    {
        let mut workspace = administered(&[(BOB, Role::Editor), ("v", Role::Viewer)]);
        make(&mut workspace, ADMIN, BOB).unwrap();
        let held = workspace.clone();
        for id in [
            roles::administrator_moderation_grant_id(BOB),
            roles::administrator_grant_id(BOB),
        ] {
            let authority = Authority::person(held.party(OWNER));
            workspace.apply(OWNER, &authority, Change::Revoke(id), &cx(), false).unwrap();
        }
        assert!(!workspace.is_administrator(BOB));
        let bob = Authority::person(workspace.party(BOB));
        assert!(!workspace.may_make_administrators(&bob, &cx()));
        assert!(workspace.check_action(&bob, governance::ADD_MEMBER, &cx(), false).is_err());
        assert!(workspace
            .check_administrator_invite(BOB, &bob, &cx(), false)
            .is_err());
        assert!(workspace
            .check(BOB, &bob, &Change::RemoveMember { person: "v".into() }, &cx(), false)
            .is_err());
    }

    phase only_who_may_make_administrators_invites_one;
    {
        let mut workspace = administered(&[(BOB, Role::Editor), (CAROL, Role::Moderator)]);
        workspace.grants.push(Grant::new(
            "carol-invites",
            Grantee::person(CAROL),
            roles::governance_action("r", governance::ADD_MEMBER),
            KindSet::OUTSIDE,
            Answer::Allow,
            Grantee::person(OWNER),
        ));
        let carol = Authority::person(workspace.party(CAROL));
        workspace.check_admission(&carol, Role::Editor, &cx(), false).unwrap();
        assert!(matches!(
            workspace.check_administrator_invite(CAROL, &carol, &cx(), false),
            Err(Refusal::Administrators { unmaking: false, .. })
        ));
        for actor in [OWNER, ADMIN] {
            let authority = Authority::person(workspace.party(actor));
            workspace.check_administrator_invite(actor, &authority, &cx(), false).unwrap();
        }
        // Not from a foreign device either.
        let foreign = workspace.person_on_device(ADMIN, true);
        assert!(workspace.check_administrator_invite(ADMIN, &foreign, &cx(), false).is_err());
    }
}
