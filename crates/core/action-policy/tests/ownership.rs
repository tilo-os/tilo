//! A workspace's owner hands it to another person, and nobody else does: the
//! person becomes its owner, and the owner they replace stays in it as an
//! Editor and one of its administrators.

use tilo_action_policy::{
    ActionKind, Answer, Authority, Change, Context, Grant, Grantee, KindSet, Refusal, Role,
    TreePath, WorkspacePolicy, roles,
};

const NOW: u64 = 1_000_000;
const OWNER: &str = "a11ce";
const BOB: &str = "b0b";
const CAROL: &str = "ca401";

fn cx() -> Context<'static> {
    Context::at(NOW)
}

fn hand(workspace: &mut WorkspacePolicy, actor: &str, person: &str) -> Result<(), Refusal> {
    let authority = workspace.clone();
    let authority = Authority::person(authority.party(actor));
    workspace.apply(
        actor,
        &authority,
        Change::TransferOwnership {
            person: person.to_owned(),
        },
        &cx(),
        false,
    )
}

tilo_test_env::scenario! {
    fn an_owner_hands_the_workspace_over;

    phase the_owner_hands_it_over_and_stays_as_an_administrator;
    {
        let mut workspace = WorkspacePolicy::new("r", OWNER);
        workspace.members.insert(BOB.to_owned(), Role::Editor);
        workspace.grants.push(roles::administrator_grant(
            "r",
            BOB,
            Grantee::person(OWNER),
        ));
        hand(&mut workspace, OWNER, BOB).unwrap();
        assert_eq!(workspace.owner(), Some(BOB));
        assert_eq!(workspace.role_of(BOB), Some(Role::Owner));
        assert!(!workspace.members.contains_key(BOB), "an owner is no member");
        assert!(!workspace.is_administrator(BOB), "the owner governs as owner");
        assert_eq!(workspace.role_of(OWNER), Some(Role::Editor));
        assert!(workspace.is_administrator(OWNER));
        assert_eq!(workspace.joined.get(OWNER), Some(&NOW));
        // The new owner unmakes the administrator they replaced.
        let bob = Authority::person(workspace.party(BOB));
        assert!(workspace.may_unmake_administrators(BOB, &bob, &cx()));
        let held = workspace.clone();
        let owner = Authority::person(held.party(OWNER));
        assert!(!held.may_unmake_administrators(OWNER, &owner, &cx()));
    }

    phase a_workspace_goes_to_someone_not_yet_in_it;
    {
        let mut workspace = WorkspacePolicy::new("r", OWNER);
        hand(&mut workspace, OWNER, CAROL).unwrap();
        assert_eq!(workspace.owner(), Some(CAROL));
        assert!(workspace.is_administrator(OWNER));
    }

    phase nobody_else_hands_it_over;
    {
        let mut workspace = WorkspacePolicy::new("r", OWNER);
        workspace.members.insert(BOB.to_owned(), Role::Editor);
        workspace.grants.push(roles::administrator_grant(
            "r",
            BOB,
            Grantee::person(OWNER),
        ));
        workspace.grants.push(roles::administrator_moderation_grant(
            "r",
            BOB,
            Grantee::person(OWNER),
        ));
        let before = workspace.clone();
        assert_eq!(
            hand(&mut workspace, BOB, BOB),
            Err(Refusal::OwnershipByOwner),
            "an administrator does not take it"
        );
        assert_eq!(
            hand(&mut workspace, BOB, CAROL),
            Err(Refusal::OwnershipByOwner),
            "nor hand it to anyone"
        );
        assert_eq!(hand(&mut workspace, OWNER, OWNER), Err(Refusal::Owner));
        assert_eq!(workspace, before, "a refused transfer changes nothing");
    }

    phase a_banned_person_is_handed_nothing;
    {
        let mut workspace = WorkspacePolicy::new("r", OWNER);
        workspace.members.insert(BOB.to_owned(), Role::Editor);
        let owner = workspace.clone();
        workspace
            .apply(
                OWNER,
                &Authority::person(owner.party(OWNER)),
                Change::Ban {
                    person: BOB.to_owned(),
                },
                &cx(),
                false,
            )
            .unwrap();
        assert_eq!(
            hand(&mut workspace, OWNER, BOB),
            Err(Refusal::Banned(BOB.to_owned()))
        );
    }
}

/// A device's settings (`settings/devices/<device>/settings/<name>`) are its owner's: kept in the
/// person's own workspace, written there by them or by someone they granted, and never by a person
/// who governs a workspace the device is in.
fn displays(workspace: &str, device: &[u8; 32]) -> TreePath {
    TreePath::workspace(workspace)
        .app(tilo_app_id::apps::SETTINGS)
        .kind("settings.display")
        .resource(format!("devices/{}/settings/display", hex::encode(device)))
}

tilo_test_env::scenario! {
    fn a_devices_settings_are_its_owners_alone;

    phase the_persons_workspace_admits_them;
    {
        let root = [3u8; 32];
        let owner = hex::encode(root);
        let workspace = WorkspacePolicy::new(roles::person_workspace_id(&root), owner.clone());
        let authority = Authority::person(workspace.party(&owner));
        assert_eq!(
            authority
                .decide(&displays(&workspace.workspace, &root), ActionKind::Undoable, &cx())
                .answer,
            Answer::Allow,
            "the person writes their own device's settings"
        );
    }

    phase a_governing_role_in_a_shared_workspace_reaches_no_other_persons_device;
    {
        let root = [3u8; 32];
        let governor = [4u8; 32];
        let mut workspace = WorkspacePolicy::new("shared", hex::encode(governor));
        workspace.members.insert(hex::encode(root), Role::Editor);
        let node = displays("shared", &root);
        let owner = Authority::person(workspace.party(&hex::encode(governor)));
        assert_eq!(
            owner.decide(&node, ActionKind::Undoable, &cx()).answer,
            Answer::Deny,
            "the workspace's owner writes no other person's device"
        );
        let editor = Authority::person(workspace.party(&hex::encode(root)));
        assert_eq!(
            editor.decide(&node, ActionKind::Undoable, &cx()).answer,
            Answer::Deny,
            "nor does the role a member holds"
        );
    }

    phase a_grant_naming_the_person_admits_them;
    {
        let root = [3u8; 32];
        let guest = [5u8; 32];
        let mut workspace = WorkspacePolicy::new(roles::person_workspace_id(&root), hex::encode(root));
        let node = displays(&workspace.workspace, &root);
        workspace.grant(Grant::new(
            "displays-for-a-guest",
            Grantee::person(hex::encode(guest)),
            node.clone(),
            KindSet::UNDOABLE,
            Answer::Allow,
            Grantee::person(hex::encode(root)),
        ));
        let guest = Authority::person(workspace.party(&hex::encode(guest)));
        assert_eq!(
            guest.decide(&node, ActionKind::Undoable, &cx()).answer,
            Answer::Allow,
            "someone the device's person granted writes them"
        );
    }
}
