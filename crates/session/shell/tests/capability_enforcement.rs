//! Enforcement tests for the compositor's room policy and sandbox.
//!
//! Every assertion runs against the production API — `Workspace`'s
//! decision methods, `WorkspaceTree`'s grant/revoke/replicate path, the
//! portal's `GrantStoreHandle`, and `persist::{save_tree_to,
//! load_tree_from}` — nothing is re-implemented locally. The bias is
//! deliberately toward **denial**: "does deny work?", not just "does
//! allow work?".

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use shell::shell::persist;
use shell::shell::state::FreshId as _;
use shell::shell::state::WorkspaceAuthority as _;
use shell::shell::state::{
    Access, CapOverride, Capability, Expiry, FsMode, GRANT_ALWAYS, GRANT_ONCE, GRANT_SESSION,
    Principal, ResourceRef, Scope, Subject, Workspace, WorkspaceId, WorkspaceTree,
};
use tilo_action_policy::{ActionKind, Answer, Role, TreePath};

const ALICE: &str = "alice";
const BOB: &str = "bob";

/// What `person` gets for an action of `kind` at `relative` in `ws`, now.
fn answer(ws: &Workspace, person: &str, relative: &TreePath, kind: ActionKind) -> Answer {
    ws.answer_at(
        person,
        &ws.node().join(relative),
        kind,
        tilo_clock::unix_now(),
    )
}

/// Whether `person` may change the room's contents outright.
fn may_change(ws: &Workspace, person: &str) -> bool {
    answer(ws, person, &TreePath::root(), ActionKind::Undoable) == Answer::Allow
}

/// Whether `person` may read the room.
fn may_read(ws: &Workspace, person: &str) -> bool {
    answer(ws, person, &TreePath::root(), ActionKind::Read) == Answer::Allow
}

fn room_owned_by(owner: &str) -> (WorkspaceTree, WorkspaceId) {
    let mut tree = WorkspaceTree::default();
    let mut ws = Workspace::new(WorkspaceId::new(), "Design");
    ws.set_owner(owner);
    let id = ws.id;
    tree.desktop.active = id;
    tree.desktop.workspaces.push(ws);
    // Seed the replicated doc exactly as the compositor's load path does.
    tree.seed_doc_from_cache();
    (tree, id)
}

fn ws_of(tree: &WorkspaceTree, id: WorkspaceId) -> &Workspace {
    tree.desktop
        .workspaces
        .iter()
        .find(|w| w.id == id)
        .expect("workspace")
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn only_the_owner_administers_a_room() {
    // Phase `only_the_owner_may_administer_the_room`.
    {
        let (tree, id) = room_owned_by(ALICE);
        let ws = ws_of(&tree, id);

        assert!(
            ws.may_administer(&ALICE.to_string()),
            "the recorded owner administers their own room"
        );
        assert!(
            !ws.may_administer(&BOB.to_string()),
            "a peer who is not the owner must NOT administer the room — this is the \
             check that used to return true unconditionally for everybody"
        );
        assert!(
            !ws.may_administer(&"nobody".to_string()),
            "a stranger must not administer the room"
        );
    }

    // Phase `an_ownerless_room_has_no_administrators`.
    {
        let mut tree = WorkspaceTree::default();
        let ws = Workspace::new(WorkspaceId::new(), "Orphan");
        let id = ws.id;
        tree.desktop.workspaces.push(ws);

        assert!(
            !ws_of(&tree, id).may_administer(&ALICE.to_string()),
            "an unowned room must have no administrators, not only administrators"
        );

        // Adoption is the production remedy, and it names a specific owner.
        let adopted = tree.adopt_ownerless_workspaces(&ALICE.to_string());
        assert_eq!(adopted, vec![id]);
        assert!(ws_of(&tree, id).may_administer(&ALICE.to_string()));
        assert!(
            !ws_of(&tree, id).may_administer(&BOB.to_string()),
            "adoption must name one owner, not open the room to everyone"
        );
    }

    // Phase `a_non_owner_cannot_grant_or_revoke`.
    {
        let (mut tree, id) = room_owned_by(ALICE);

        tree.admit_in_workspace(id, &ALICE.to_string(), BOB, Role::Editor)
            .expect("the owner may admit a member");

        // Bob tries to widen his own authority. This must fail.
        let escalation = tree.admit_in_workspace(id, &BOB.to_string(), "mallory", Role::Editor);
        assert_eq!(
            escalation, None,
            "a member who is not an administrator must not be able to grant, least of \
             all to themselves"
        );
        assert!(
            !ws_of(&tree, id).may_administer(&BOB.to_string()),
            "and the attempted escalation must not have taken effect"
        );

        let carol_cap = tree
            .grant_sandbox_in_workspace(
                id,
                &ALICE.to_string(),
                CapOverride::grant(
                    Subject::person("carol"),
                    Scope::FsPath("/var/workspace/design".into()),
                    Access::Read,
                ),
            )
            .expect("owner grants");
        assert!(
            !tree.revoke_sandbox_in_workspace(id, &BOB.to_string(), &carol_cap),
            "a non-administrator must not be able to revoke"
        );
        assert!(
            ws_of(&tree, id).sandbox.iter().any(|c| c.id == carol_cap),
            "and the capability must still be there"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn peer_capabilities_are_enforced_live_attributed_and_replicated() {
    // Phase `a_viewer_is_denied_changes_and_governance`.
    {
        let (mut tree, id) = room_owned_by(ALICE);
        tree.admit_in_workspace(id, &ALICE.to_string(), BOB, Role::Viewer)
            .expect("owner admits a viewer");

        let ws = ws_of(&tree, id);
        assert!(may_read(ws, BOB), "a Viewer may look");
        assert_eq!(
            answer(ws, BOB, &TreePath::root(), ActionKind::Undoable),
            Answer::Deny,
            "a Viewer must be denied changes"
        );
        assert!(
            !ws.may_administer(&BOB.to_string()),
            "a Viewer must be denied governance"
        );
    }

    // Phase `the_host_can_change_a_peers_permissions_live`.
    {
        let (mut tree, id) = room_owned_by(ALICE);
        let bob = BOB.to_string();

        tree.admit_in_workspace(id, &ALICE.to_string(), BOB, Role::Viewer)
            .expect("owner admits");
        assert!(
            !may_change(ws_of(&tree, id), BOB),
            "read-only to begin with"
        );

        tree.admit_in_workspace(id, &ALICE.to_string(), BOB, Role::Editor)
            .expect("owner promotes");
        assert!(
            may_change(ws_of(&tree, id), BOB),
            "the promotion takes effect immediately"
        );

        assert!(tree.remove_from_workspace(id, &ALICE.to_string(), BOB));
        assert!(
            !may_read(ws_of(&tree, id), BOB),
            "ending the membership removes access outright"
        );
        assert_eq!(
            ws_of(&tree, id).role_of(&bob),
            None,
            "and Bob is no longer a member"
        );
    }

    // Phase `a_revoked_capability_stops_working_on_a_peer`.
    {
        let (mut host, id) = room_owned_by(ALICE);
        let bob = BOB.to_string();

        admit_editor(&mut host, id);

        let mut peer = WorkspaceTree::default();
        let changes = host.export_changes_since(&[]);
        peer.apply_changes(&changes).expect("peer applies");

        let peer_ws = ws_of(&peer, id);
        assert!(
            may_change(peer_ws, &bob),
            "the peer must see the membership the host granted"
        );

        let heads = host.current_heads();
        assert!(host.remove_from_workspace(id, &ALICE.to_string(), BOB));
        let revoke_changes = host.export_changes_since(&heads);
        assert!(
            !revoke_changes.is_empty(),
            "a revoke must produce replicable changes — a revoke that only mutates the \
             local cache is invisible to every peer"
        );
        peer.apply_changes(&revoke_changes)
            .expect("peer applies revoke");

        let peer_ws = ws_of(&peer, id);
        assert!(
            !may_change(peer_ws, &bob),
            "the revocation must take effect ON THE PEER, not merely be recorded on the host"
        );
        assert_eq!(
            peer_ws.role_of(&bob),
            None,
            "and the membership must be gone from the peer's roster"
        );
    }

    // Phase `a_peers_grant_replicates_to_the_host`.
    {
        // Replication is two-way: the host must also converge on a capability a
        // co-owner issued.
        let (mut host, id) = room_owned_by(ALICE);
        let mut peer = WorkspaceTree::default();
        peer.apply_changes(&host.export_changes_since(&[]))
            .expect("hydrate");

        delegate_granting(&mut host, id);
        peer.apply_changes(&host.export_changes_since(&[]))
            .expect("sync");

        let heads = peer.current_heads();
        peer.grant_sandbox_in_workspace(
            id,
            &BOB.to_string(),
            CapOverride::grant(
                Subject::person("carol"),
                Scope::FsPath("/var/workspace/design".into()),
                Access::Read,
            )
            .with_id("carol-read"),
        )
        .expect("a delegated administrator may grant");
        let out = peer.export_changes_since(&heads);
        host.apply_changes(&out).expect("host applies");

        assert!(
            ws_of(&host, id)
                .sandbox
                .iter()
                .any(|c| c.id == "carol-read"),
            "the host must converge on the capability the co-owner issued"
        );
    }
}

fn admit_editor(tree: &mut WorkspaceTree, id: WorkspaceId) {
    tree.admit_in_workspace(id, &ALICE.to_string(), BOB, Role::Editor)
        .expect("owner admits");
}

/// Bob, an editor, may also grant in the room: the owner delegated the
/// room's grant action to him.
fn delegate_granting(tree: &mut WorkspaceTree, id: WorkspaceId) {
    admit_editor(tree, id);
    let room = ws_of(tree, id).room.to_string();
    tree.grant_in_workspace(
        id,
        &ALICE.to_string(),
        tilo_action_policy::Grant::new(
            "bob-grants",
            tilo_action_policy::Subject::person(BOB),
            tilo_action_policy::roles::governance_action(
                &room,
                tilo_action_policy::roles::governance::GRANT,
            ),
            tilo_action_policy::KindSet::ALL,
            Answer::Allow,
            tilo_action_policy::Subject::person(ALICE),
        ),
    )
    .expect("owner delegates");
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn capability_lifetimes_survive_restart_never_shorten_and_expire() {
    // Phase `always_survives_a_restart_and_session_does_not`.
    {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");

        let (mut tree, id) = room_owned_by(ALICE);
        {
            let ws = tree
                .desktop
                .workspaces
                .iter_mut()
                .find(|w| w.id == id)
                .unwrap();
            ws.grant_app(
                "org.tilo.Ebook",
                &Capability::Filesystem {
                    path: "/var/workspace/books".into(),
                    mode: FsMode::Read,
                    scope: GRANT_ALWAYS,
                },
            );
            ws.grant_app(
                "org.tilo.Ebook",
                &Capability::Camera {
                    scope: GRANT_SESSION,
                },
            );
            ws.grant_app(
                "org.tilo.Ebook",
                &Capability::Microphone { scope: GRANT_ONCE },
            );
        }

        let ws = ws_of(&tree, id);
        let app = ws.actor_for_app("org.tilo.Ebook");
        assert!(ws.allows(
            &app,
            &ResourceRef::FsPath("/var/workspace/books/dune.epub".into()),
            Access::Read
        ));
        assert!(ws.allows(&app, &ResourceRef::Device("camera".into()), Access::Read));
        assert!(ws.allows(
            &app,
            &ResourceRef::Device("microphone".into()),
            Access::Read
        ));

        persist::save_tree_to(&tree, &path).expect("save");
        let reloaded = persist::load_tree_from(&path).expect("load");

        let ws = ws_of(&reloaded, id);
        let app = ws.actor_for_app("org.tilo.Ebook");
        assert!(
            ws.allows(
                &app,
                &ResourceRef::FsPath("/var/workspace/books/dune.epub".into()),
                Access::Read
            ),
            "an Always grant MUST survive a restart — before this, nothing in the \
             compositor called save_tree_to at all, so Always behaved like Session"
        );
        assert!(
            !ws.allows(&app, &ResourceRef::Device("camera".into()), Access::Read),
            "a Session grant must NOT survive a restart"
        );
        assert!(
            !ws.allows(
                &app,
                &ResourceRef::Device("microphone".into()),
                Access::Read
            ),
            "a one-shot grant must NOT survive a restart"
        );
        assert!(
            ws.may_administer(&ALICE.to_string()),
            "and ownership survives too, or the reloaded room would have no administrator"
        );
    }

    // Phase `re_granting_never_shortens_an_existing_grant`.
    {
        // A grant is monotonic: an app holding "Always" for the camera that
        // makes a fresh (one-shot-defaulting) request must not have its
        // permanent grant quietly demoted — reducing authority is what revoke
        // and Restrict are for, both explicit acts the user can see.
        let (mut tree, id) = room_owned_by(ALICE);
        let ws = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|w| w.id == id)
            .unwrap();

        ws.grant_app(
            "org.tilo.Meet",
            &Capability::Camera {
                scope: GRANT_ALWAYS,
            },
        );
        ws.grant_app("org.tilo.Meet", &Capability::Camera { scope: GRANT_ONCE });
        ws.grant_app(
            "org.tilo.Meet",
            &Capability::Camera {
                scope: GRANT_SESSION,
            },
        );

        let live: Vec<&CapOverride> = ws.app_capabilities("org.tilo.Meet");
        assert_eq!(live.len(), 1, "re-granting merges rather than stacking");
        assert_eq!(
            live[0].expiry, GRANT_ALWAYS,
            "the longest-lived expiry must win; a later one-shot or session request \
             must not demote a permanent grant"
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        persist::save_tree_to(&tree, &path).expect("save");
        let reloaded = persist::load_tree_from(&path).expect("load");
        assert!(
            ws_of(&reloaded, id).allows(
                &ws_of(&reloaded, id).actor_for_app("org.tilo.Meet"),
                &ResourceRef::Device("camera".into()),
                Access::Read
            ),
            "the merged grant is still the permanent one after a restart"
        );
    }

    // Phase `an_expired_capability_stops_working_without_being_revoked`.
    {
        let (mut tree, id) = room_owned_by(ALICE);
        let past = 1_000u64; // long before any plausible `now`
        tree.grant_sandbox_in_workspace(
            id,
            &ALICE.to_string(),
            CapOverride::grant(
                Subject::app("org.tilo.Ebook"),
                Scope::FsPath("/var/workspace/books".into()),
                Access::Read,
            )
            .with_expiry(past),
        )
        .expect("owner grants");

        let ws = ws_of(&tree, id);
        assert!(
            !ws.allows(
                &ws.actor_for_app("org.tilo.Ebook"),
                &ResourceRef::FsPath("/var/workspace/books/dune.epub".into()),
                Access::Read
            ),
            "a capability past its expiry must stop working even though nobody revoked it"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn filesystem_capabilities_export_exactly_and_inherit_to_the_boundary() {
    // Phase `the_filesystem_capability_export_carries_exactly_the_path_capabilities`.
    {
        // The FUSE mount is a separate process enforcing the list it reads from
        // this file — if the compositor never writes it, enforcement is real but
        // always enforcing nothing, indistinguishable from none at all.
        let dir = tempfile::tempdir().expect("tempdir");
        let acl_path = dir.path().join("fs-capabilities.json");

        let (mut tree, id) = room_owned_by(ALICE);
        tree.fs_acl_path = Some(acl_path.clone());
        tree.grant_sandbox_in_workspace(
            id,
            &ALICE.to_string(),
            CapOverride::grant(
                Subject::person(BOB),
                Scope::FsPath("/var/workspace/design".into()),
                Access::Write,
            )
            .with_id("bob-design"),
        )
        .expect("grant a path");
        // Membership must NOT reach the filesystem as a path: it has no
        // business deciding room membership there.
        tree.admit_in_workspace(id, &ALICE.to_string(), BOB, Role::Editor)
            .expect("grant membership");

        persist::wait_for_saves();
        let raw = std::fs::read_to_string(&acl_path).expect("the export must exist after a grant");
        let exported: Vec<CapOverride> =
            serde_json::from_str(&raw).expect("the export must be the array the filesystem parses");

        assert!(
            exported.iter().any(|c| c.id == "bob-design"),
            "a path capability must reach the filesystem"
        );
        assert!(
            !exported
                .iter()
                .any(|c| c.subject == Subject::person(BOB) && !matches!(c.scope, Scope::FsPath(_))),
            "room membership is not a filesystem fact and must not be exported"
        );
        assert!(
            exported.iter().all(|c| matches!(c.scope, Scope::FsPath(_))),
            "only path-scoped capabilities belong in the filesystem's list"
        );

        assert!(tree.revoke_sandbox_in_workspace(id, &ALICE.to_string(), "bob-design"));
        persist::wait_for_saves();
        let raw = std::fs::read_to_string(&acl_path).expect("export still present");
        let exported: Vec<CapOverride> = serde_json::from_str(&raw).expect("parse");
        assert!(
            !exported.iter().any(|c| c.id == "bob-design"),
            "a revoked capability must disappear from the filesystem's list, or the mount \
             keeps honouring access the owner took away"
        );
    }

    // Phase `the_portal_denies_an_app_a_path_it_was_never_granted`.
    {
        use shell::portal::{CapabilityKind, GrantStoreHandle, PortalCaller};

        let mut ws = Workspace::new(WorkspaceId::new(), "Home");
        ws.set_owner(ALICE);
        let (store, ws_id) = GrantStoreHandle::test_with_workspace(ws);
        let caller = PortalCaller::new(ws_id, "org.tilo.Ebook");

        // Nothing granted yet: every lookup and every check denies.
        assert_eq!(
            store.lookup(&caller, CapabilityKind::FilesystemRead),
            None,
            "no grant means no grant"
        );
        assert!(
            !store.permits(
                &caller,
                &ResourceRef::FsPath("/var/workspace/books/dune.epub".into()),
                Access::Read
            ),
            "an app with no capability must be denied"
        );

        assert!(store.insert(
            &caller,
            &Capability::Filesystem {
                path: "/var/workspace/books".into(),
                mode: FsMode::Read,
                scope: GRANT_ALWAYS,
            }
        ));

        assert!(
            store.permits(
                &caller,
                &ResourceRef::FsPath("/var/workspace/books/dune.epub".into()),
                Access::Read
            ),
            "the granted subtree is reachable"
        );
        assert!(
            !store.permits(
                &caller,
                &ResourceRef::FsPath("/var/workspace/finances/tax.ods".into()),
                Access::Read
            ),
            "a sibling directory the app was never granted must be denied — this is the \
             case that is supposed to raise a prompt, not silently succeed"
        );
        assert!(
            !store.permits(
                &caller,
                &ResourceRef::FsPath("/var/workspace/booksSECRET/keys".into()),
                Access::Read
            ),
            "and a path that merely shares a string prefix must be denied"
        );
        assert!(
            !store.permits(
                &caller,
                &ResourceRef::FsPath("/var/workspace/books/dune.epub".into()),
                Access::Write
            ),
            "a read grant is not a write grant"
        );

        let other = PortalCaller::new(ws_id, "org.tilo.Files");
        assert!(
            !store.permits(
                &other,
                &ResourceRef::FsPath("/var/workspace/books/dune.epub".into()),
                Access::Read
            ),
            "a capability granted to one app must not serve another"
        );
        assert_eq!(store.lookup(&other, CapabilityKind::FilesystemRead), None);
    }

    // Phase `file_capabilities_inherit_down_the_tree_and_stop_at_the_boundary`.
    {
        let (mut tree, id) = room_owned_by(ALICE);
        tree.admit_in_workspace(id, &ALICE.to_string(), BOB, Role::Viewer)
            .expect("admit bob");
        tree.grant_sandbox_in_workspace(
            id,
            &ALICE.to_string(),
            CapOverride::grant(
                Subject::person(BOB),
                Scope::FsPath("/var/workspace/design".into()),
                Access::Write,
            ),
        )
        .expect("give bob the design tree");

        let ws = ws_of(&tree, id);
        let bob = ws.actor_for_person(&BOB.to_string());

        for path in [
            "/var/workspace/design",
            "/var/workspace/design/logo.svg",
            "/var/workspace/design/icons/a/b/c.svg",
        ] {
            assert!(
                ws.allows(&bob, &ResourceRef::FsPath(path.into()), Access::Write),
                "{path} is under the granted directory"
            );
        }
        for path in [
            "/var/workspace",
            "/var/workspace/designSECRET/keys",
            "/var/workspace/finance/budget.ods",
            "/etc/shadow",
        ] {
            assert!(
                !ws.allows(&bob, &ResourceRef::FsPath(path.into()), Access::Read),
                "{path} is outside the granted directory and must be denied"
            );
        }
    }
}

// The portal: an app reaching outside what it owns is denied.

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn scoped_capabilities_carry_exactly_their_own_authority() {
    // Phase `a_network_allowlist_is_actually_a_list`.
    {
        let (mut tree, id) = room_owned_by(ALICE);
        {
            let ws = tree
                .desktop
                .workspaces
                .iter_mut()
                .find(|w| w.id == id)
                .unwrap();
            ws.grant_app(
                "org.tilo.Browser",
                &Capability::Network {
                    hosts: Some(vec!["docs.example.com".into()]),
                    scope: GRANT_ALWAYS,
                },
            );
        }
        let ws = ws_of(&tree, id);
        let app = ws.actor_for_app("org.tilo.Browser");

        assert!(
            ws.allows(
                &app,
                &ResourceRef::NetHost("docs.example.com".into()),
                Access::Write
            ),
            "the allowlisted host is reachable"
        );
        assert!(
            !ws.allows(
                &app,
                &ResourceRef::NetHost("evil.example".into()),
                Access::Write
            ),
            "a host NOT on the allowlist must be denied — recording an allowlist and then \
             behaving as though it said `any host` is the bug this replaces"
        );
    }

    // Phase `sharing_one_file_never_puts_a_person_in_the_roster`.
    {
        let (mut tree, id) = room_owned_by(ALICE);

        // Bob is admitted to the room. Carol is only given one file.
        tree.admit_in_workspace(id, &ALICE.to_string(), BOB, Role::Editor)
            .expect("admit bob");
        tree.grant_sandbox_in_workspace(
            id,
            &ALICE.to_string(),
            CapOverride::grant(
                Subject::person("carol"),
                Scope::FsPath("/var/workspace/design/logo.svg".into()),
                Access::Read,
            ),
        )
        .expect("share one file with carol");

        let ws = ws_of(&tree, id);
        let roster: Vec<String> = ws.roster().into_iter().map(|(u, _)| u).collect();

        assert!(roster.contains(&ALICE.to_string()), "the owner is a member");
        assert!(
            roster.contains(&BOB.to_string()),
            "an admitted peer is a member"
        );
        assert!(
            !roster.contains(&"carol".to_string()),
            "a person given one file must NEVER appear in the room roster — presenting an \
             ad-hoc share as 'I made you a collaborator' is exactly what the model forbids"
        );

        let shares: Vec<&CapOverride> = ws
            .sandbox
            .iter()
            .filter(|c| c.scope == Scope::FsPath("/var/workspace/design/logo.svg".into()))
            .collect();
        assert_eq!(shares.len(), 1, "one share on this file");
        assert_eq!(
            shares[0].subject,
            Subject::Principal(Principal::Person("carol".into()))
        );
        assert!(
            !shares
                .iter()
                .any(|c| c.subject == Subject::Principal(Principal::Person(BOB.into()))),
            "and room membership must NOT be rendered as a share of this file — the two \
             lists are disjoint in both directions"
        );

        let carol = ws.actor_for_person(&"carol".to_string());
        assert!(ws.allows(
            &carol,
            &ResourceRef::FsPath("/var/workspace/design/logo.svg".into()),
            Access::Read
        ));
        assert!(
            !may_read(ws, "carol"),
            "a file share grants no standing in the room itself"
        );
        assert_eq!(ws.role_of("carol"), None, "and no role");
    }

    // Phase `a_restriction_narrows_a_member_below_their_role`.
    {
        let (mut tree, id) = room_owned_by(ALICE);
        tree.admit_in_workspace(id, &ALICE.to_string(), BOB, Role::Editor)
            .expect("admit bob as editor");
        let finance = ws_of(&tree, id).node().app("finance");
        tree.grant_in_workspace(
            id,
            &ALICE.to_string(),
            tilo_action_policy::Grant::new(
                "bob-not-finance",
                tilo_action_policy::Subject::person(BOB),
                finance,
                tilo_action_policy::KindSet::UNDOABLE,
                Answer::Deny,
                tilo_action_policy::Subject::person(ALICE),
            ),
        )
        .expect("narrow bob inside one app");

        let ws = ws_of(&tree, id);
        assert_eq!(
            answer(
                ws,
                BOB,
                &TreePath::root().app("finance"),
                ActionKind::Undoable
            ),
            Answer::Deny,
            "the restriction removes changes inside that app even though the role grants them"
        );
        assert_eq!(
            answer(
                ws,
                BOB,
                &TreePath::root().app("notes"),
                ActionKind::Undoable
            ),
            Answer::Allow,
            "and it does not leak to other apps"
        );
    }

    // Phase `a_bearer_token_carries_exactly_its_own_authority`.
    {
        let (mut tree, id) = room_owned_by(ALICE);
        tree.grant_sandbox_in_workspace(
            id,
            &ALICE.to_string(),
            CapOverride::grant(
                Subject::token("inv-7f3a"),
                Scope::FsPath("/var/workspace/design/logo.svg".into()),
                Access::Read,
            )
            .with_id("share-link"),
        )
        .expect("owner mints a share link");

        let ws = ws_of(&tree, id);
        let holder = shell::shell::state::Actor::default()
            .with_principal(Principal::Token("inv-7f3a".into()));
        let path = ResourceRef::FsPath("/var/workspace/design/logo.svg".into());

        assert!(ws.allows(&holder, &path, Access::Read));
        assert!(
            !ws.allows(&holder, &path, Access::Write),
            "a read link is not a write link"
        );
        assert!(
            !ws.allows(&holder, &ResourceRef::Workspace, Access::Read),
            "a share link grants no standing in the room"
        );

        let wrong = shell::shell::state::Actor::default()
            .with_principal(Principal::Token("inv-0000".into()));
        assert!(
            !ws.allows(&wrong, &path, Access::Read),
            "a different token grants nothing"
        );

        assert!(tree.revoke_sandbox_in_workspace(id, &ALICE.to_string(), "share-link"));
        assert!(
            !ws_of(&tree, id).allows(&holder, &path, Access::Read),
            "a revoked link stops working"
        );
    }

    // Phase `an_agent_holds_a_scoped_revocable_capability_set_like_an_app`.
    {
        let (mut tree, id) = room_owned_by(ALICE);
        tree.grant_sandbox_in_workspace(
            id,
            &ALICE.to_string(),
            CapOverride::grant(
                Subject::agent("assistant"),
                Scope::FsPath("/var/workspace/notes".into()),
                Access::Write,
            )
            .with_id("agent-notes")
            .expiring(Expiry::Never),
        )
        .expect("owner scopes the agent");

        let ws = ws_of(&tree, id);
        let agent = ws.actor_for_agent("assistant");
        assert!(ws.allows(
            &agent,
            &ResourceRef::FsPath("/var/workspace/notes/todo.md".into()),
            Access::Write
        ));
        assert!(
            !ws.allows(
                &agent,
                &ResourceRef::FsPath("/var/workspace/finance/budget.ods".into()),
                Access::Read
            ),
            "an agent is confined to what it was given, like any other subject"
        );
        assert!(
            !ws.allows(&agent, &ResourceRef::Workspace, Access::Write),
            "and holds no role, so it never inherits room-wide authority"
        );

        assert!(tree.revoke_sandbox_in_workspace(id, &ALICE.to_string(), "agent-notes"));
        assert!(
            !ws_of(&tree, id).allows(
                &ws_of(&tree, id).actor_for_agent("assistant"),
                &ResourceRef::FsPath("/var/workspace/notes/todo.md".into()),
                Access::Write
            ),
            "the agent's authority is revocable in one act"
        );
    }
}

// The hard UI constraint, asserted on data rather than trusted to a
// renderer: a per-file share is never room membership.

// File permissions inherit down the tree, Linux-style.

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use shell::shell::state::WorkspaceExt as _;
