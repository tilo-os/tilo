//! Enforcement tests for the compositor's capability model.
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
    CapOverride, Capability, Effect, Expiry, FsMode, GRANT_ALWAYS, GRANT_ONCE, GRANT_SESSION,
    Principal, ResourceRef, Scope, Subject, Verb, Workspace, WorkspaceId, WorkspaceTree,
};

const ALICE: &str = "alice";
const BOB: &str = "bob";

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

        tree.grant_in_workspace(
            id,
            &ALICE.to_string(),
            CapOverride::membership(Subject::person(BOB), id.raw().to_string(), Verb::Write),
        )
        .expect("the owner may admit a member");

        // Bob tries to widen his own authority. This must fail.
        let escalation = tree.grant_in_workspace(
            id,
            &BOB.to_string(),
            CapOverride::membership(Subject::person(BOB), id.raw().to_string(), Verb::Admin),
        );
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
            .grant_in_workspace(
                id,
                &ALICE.to_string(),
                CapOverride::grant(
                    Subject::person("carol"),
                    Scope::FsPath("/var/workspace/design".into()),
                    Verb::Read,
                ),
            )
            .expect("owner grants");
        assert!(
            !tree.revoke_in_workspace(id, &BOB.to_string(), &carol_cap),
            "a non-administrator must not be able to revoke"
        );
        assert!(
            ws_of(&tree, id)
                .capabilities
                .iter()
                .any(|c| c.id == carol_cap),
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
    // Phase `a_peer_without_the_verb_is_denied_and_the_denial_is_attributable`.
    {
        use shell::shell::state::CapDecision;
        let (mut tree, id) = room_owned_by(ALICE);
        tree.grant_in_workspace(
            id,
            &ALICE.to_string(),
            CapOverride::membership(Subject::person(BOB), id.raw().to_string(), Verb::Read),
        )
        .expect("owner admits a viewer");

        let ws = ws_of(&tree, id);
        let bob = ws.actor_for_person(&BOB.to_string());

        assert_eq!(
            ws.decide(&bob, &ResourceRef::Workspace, Verb::Read),
            CapDecision::Allow,
            "a Viewer may look"
        );
        assert_eq!(
            ws.decide(&bob, &ResourceRef::Workspace, Verb::Write),
            CapDecision::DeniedByRole,
            "a Viewer must be denied Write"
        );
        assert_eq!(
            ws.decide(&bob, &ResourceRef::Workspace, Verb::Admin),
            CapDecision::DeniedByRole,
            "a Viewer must be denied Admin"
        );
        assert!(!ws.allows(&bob, &ResourceRef::Workspace, Verb::Write));
    }

    // Phase `the_host_can_change_a_peers_permissions_live`.
    {
        let (mut tree, id) = room_owned_by(ALICE);
        let bob = BOB.to_string();

        let membership = tree
            .grant_in_workspace(
                id,
                &ALICE.to_string(),
                CapOverride::membership(Subject::person(BOB), id.raw().to_string(), Verb::Read)
                    .with_id("bob-membership"),
            )
            .expect("owner admits");
        assert_eq!(membership, "bob-membership");
        assert!(
            !ws_of(&tree, id).allows(
                &ws_of(&tree, id).actor_for_person(&bob),
                &ResourceRef::Workspace,
                Verb::Write
            ),
            "read-only to begin with"
        );

        tree.grant_in_workspace(
            id,
            &ALICE.to_string(),
            CapOverride::membership(Subject::person(BOB), id.raw().to_string(), Verb::Write)
                .with_id("bob-membership"),
        )
        .expect("owner promotes");
        assert!(
            ws_of(&tree, id).allows(
                &ws_of(&tree, id).actor_for_person(&bob),
                &ResourceRef::Workspace,
                Verb::Write
            ),
            "the promotion takes effect immediately"
        );

        assert!(tree.revoke_in_workspace(id, &ALICE.to_string(), "bob-membership"));
        assert!(
            !ws_of(&tree, id).allows(
                &ws_of(&tree, id).actor_for_person(&bob),
                &ResourceRef::Workspace,
                Verb::Read
            ),
            "revoking the membership capability removes access outright"
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

        tree_grant(&mut host, id, "bob-write");

        let mut peer = WorkspaceTree::default();
        let changes = host.export_changes_since(&[]);
        peer.apply_changes(&changes).expect("peer applies");

        let peer_ws = ws_of(&peer, id);
        assert!(
            peer_ws.allows(
                &peer_ws.actor_for_person(&bob),
                &ResourceRef::Workspace,
                Verb::Write
            ),
            "the peer must see the capability the host granted"
        );

        let heads = host.current_heads();
        assert!(host.revoke_in_workspace(id, &ALICE.to_string(), "bob-write"));
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
            !peer_ws.allows(
                &peer_ws.actor_for_person(&bob),
                &ResourceRef::Workspace,
                Verb::Write
            ),
            "the revocation must take effect ON THE PEER, not merely be recorded on the host"
        );
        assert!(
            !peer_ws.capabilities.iter().any(|c| c.id == "bob-write"),
            "and the capability must be gone from the peer's list"
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

        tree_grant_admin(&mut host, id, "bob-admin");
        peer.apply_changes(&host.export_changes_since(&[]))
            .expect("sync");

        let heads = peer.current_heads();
        peer.grant_in_workspace(
            id,
            &BOB.to_string(),
            CapOverride::grant(
                Subject::person("carol"),
                Scope::FsPath("/var/workspace/design".into()),
                Verb::Read,
            )
            .with_id("carol-read"),
        )
        .expect("a delegated administrator may grant");
        let out = peer.export_changes_since(&heads);
        host.apply_changes(&out).expect("host applies");

        assert!(
            ws_of(&host, id)
                .capabilities
                .iter()
                .any(|c| c.id == "carol-read"),
            "the host must converge on the capability the co-owner issued"
        );
    }
}

fn tree_grant(tree: &mut WorkspaceTree, id: WorkspaceId, cap_id: &str) {
    tree.grant_in_workspace(
        id,
        &ALICE.to_string(),
        CapOverride::membership(Subject::person(BOB), id.raw().to_string(), Verb::Write)
            .with_id(cap_id),
    )
    .expect("owner grants");
}

fn tree_grant_admin(tree: &mut WorkspaceTree, id: WorkspaceId, cap_id: &str) {
    tree.grant_in_workspace(
        id,
        &ALICE.to_string(),
        CapOverride::membership(Subject::person(BOB), id.raw().to_string(), Verb::Admin)
            .with_id(cap_id),
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
            Verb::Read
        ));
        assert!(ws.allows(&app, &ResourceRef::Device("camera".into()), Verb::Read));
        assert!(ws.allows(&app, &ResourceRef::Device("microphone".into()), Verb::Read));

        persist::save_tree_to(&tree, &path).expect("save");
        let reloaded = persist::load_tree_from(&path).expect("load");

        let ws = ws_of(&reloaded, id);
        let app = ws.actor_for_app("org.tilo.Ebook");
        assert!(
            ws.allows(
                &app,
                &ResourceRef::FsPath("/var/workspace/books/dune.epub".into()),
                Verb::Read
            ),
            "an Always grant MUST survive a restart — before this, nothing in the \
             compositor called save_tree_to at all, so Always behaved like Session"
        );
        assert!(
            !ws.allows(&app, &ResourceRef::Device("camera".into()), Verb::Read),
            "a Session grant must NOT survive a restart"
        );
        assert!(
            !ws.allows(&app, &ResourceRef::Device("microphone".into()), Verb::Read),
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
                Verb::Read
            ),
            "the merged grant is still the permanent one after a restart"
        );
    }

    // Phase `an_expired_capability_stops_working_without_being_revoked`.
    {
        let (mut tree, id) = room_owned_by(ALICE);
        let past = 1_000u64; // long before any plausible `now`
        tree.grant_in_workspace(
            id,
            &ALICE.to_string(),
            CapOverride::grant(
                Subject::app("org.tilo.Ebook"),
                Scope::FsPath("/var/workspace/books".into()),
                Verb::Read,
            )
            .with_expiry(past),
        )
        .expect("owner grants");

        let ws = ws_of(&tree, id);
        assert!(
            !ws.allows(
                &ws.actor_for_app("org.tilo.Ebook"),
                &ResourceRef::FsPath("/var/workspace/books/dune.epub".into()),
                Verb::Read
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
        tree.grant_in_workspace(
            id,
            &ALICE.to_string(),
            CapOverride::grant(
                Subject::person(BOB),
                Scope::FsPath("/var/workspace/design".into()),
                Verb::Write,
            )
            .with_id("bob-design"),
        )
        .expect("grant a path");
        // A non-path capability must NOT reach the filesystem: it has no
        // business deciding room membership.
        tree.grant_in_workspace(
            id,
            &ALICE.to_string(),
            CapOverride::membership(Subject::person(BOB), id.raw().to_string(), Verb::Write)
                .with_id("bob-membership"),
        )
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
            !exported.iter().any(|c| c.id == "bob-membership"),
            "room membership is not a filesystem fact and must not be exported"
        );
        assert!(
            exported.iter().all(|c| matches!(c.scope, Scope::FsPath(_))),
            "only path-scoped capabilities belong in the filesystem's list"
        );

        assert!(tree.revoke_in_workspace(id, &ALICE.to_string(), "bob-design"));
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
                Verb::Read
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
                Verb::Read
            ),
            "the granted subtree is reachable"
        );
        assert!(
            !store.permits(
                &caller,
                &ResourceRef::FsPath("/var/workspace/finances/tax.ods".into()),
                Verb::Read
            ),
            "a sibling directory the app was never granted must be denied — this is the \
             case that is supposed to raise a prompt, not silently succeed"
        );
        assert!(
            !store.permits(
                &caller,
                &ResourceRef::FsPath("/var/workspace/booksSECRET/keys".into()),
                Verb::Read
            ),
            "and a path that merely shares a string prefix must be denied"
        );
        assert!(
            !store.permits(
                &caller,
                &ResourceRef::FsPath("/var/workspace/books/dune.epub".into()),
                Verb::Write
            ),
            "a read grant is not a write grant"
        );

        let other = PortalCaller::new(ws_id, "org.tilo.Files");
        assert!(
            !store.permits(
                &other,
                &ResourceRef::FsPath("/var/workspace/books/dune.epub".into()),
                Verb::Read
            ),
            "a capability granted to one app must not serve another"
        );
        assert_eq!(store.lookup(&other, CapabilityKind::FilesystemRead), None);
    }

    // Phase `file_capabilities_inherit_down_the_tree_and_stop_at_the_boundary`.
    {
        let (mut tree, id) = room_owned_by(ALICE);
        tree.grant_in_workspace(
            id,
            &ALICE.to_string(),
            CapOverride::membership(Subject::person(BOB), id.raw().to_string(), Verb::Read),
        )
        .expect("admit bob");
        tree.grant_in_workspace(
            id,
            &ALICE.to_string(),
            CapOverride::grant(
                Subject::person(BOB),
                Scope::FsPath("/var/workspace/design".into()),
                Verb::Write,
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
                ws.allows(&bob, &ResourceRef::FsPath(path.into()), Verb::Write),
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
                !ws.allows(&bob, &ResourceRef::FsPath(path.into()), Verb::Read),
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
                Verb::Write
            ),
            "the allowlisted host is reachable"
        );
        assert!(
            !ws.allows(
                &app,
                &ResourceRef::NetHost("evil.example".into()),
                Verb::Write
            ),
            "a host NOT on the allowlist must be denied — recording an allowlist and then \
             behaving as though it said `any host` is the bug this replaces"
        );
    }

    // Phase `sharing_one_file_never_puts_a_person_in_the_roster`.
    {
        let (mut tree, id) = room_owned_by(ALICE);

        // Bob is admitted to the room. Carol is only given one file.
        tree.grant_in_workspace(
            id,
            &ALICE.to_string(),
            CapOverride::membership(Subject::person(BOB), id.raw().to_string(), Verb::Write),
        )
        .expect("admit bob");
        tree.grant_in_workspace(
            id,
            &ALICE.to_string(),
            CapOverride::grant(
                Subject::person("carol"),
                Scope::FsPath("/var/workspace/design/logo.svg".into()),
                Verb::Read,
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

        let shares = ws.shares_of(&ResourceRef::FsPath(
            "/var/workspace/design/logo.svg".into(),
        ));
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
            Verb::Read
        ));
        assert!(
            !ws.allows(&carol, &ResourceRef::Workspace, Verb::Read),
            "a file share grants no standing in the room itself"
        );
        assert_eq!(ws.role_of(&"carol".to_string()), None, "and no role");
    }

    // Phase `a_restriction_narrows_a_member_below_their_role`.
    {
        use shell::shell::state::CapDecision;
        let (mut tree, id) = room_owned_by(ALICE);
        tree.grant_in_workspace(
            id,
            &ALICE.to_string(),
            CapOverride::membership(Subject::person(BOB), id.raw().to_string(), Verb::Write),
        )
        .expect("admit bob as editor");
        tree.grant_in_workspace(
            id,
            &ALICE.to_string(),
            CapOverride::new(
                Subject::person(BOB),
                Scope::App("finance".into()),
                Effect::Restrict,
                Verb::Read,
            ),
        )
        .expect("narrow bob inside one app");

        let ws = ws_of(&tree, id);
        let bob = ws.actor_for_person(&BOB.to_string());

        assert_eq!(
            ws.decide(&bob, &ResourceRef::App("finance".into()), Verb::Write),
            CapDecision::DeniedByRestriction,
            "the restriction removes Write inside that app even though the role grants it"
        );
        assert_eq!(
            ws.decide(&bob, &ResourceRef::App("notes".into()), Verb::Write),
            CapDecision::Allow,
            "and it does not leak to other apps"
        );
    }

    // Phase `a_bearer_token_carries_exactly_its_own_authority`.
    {
        let (mut tree, id) = room_owned_by(ALICE);
        tree.grant_in_workspace(
            id,
            &ALICE.to_string(),
            CapOverride::grant(
                Subject::token("inv-7f3a"),
                Scope::FsPath("/var/workspace/design/logo.svg".into()),
                Verb::Read,
            )
            .with_id("share-link"),
        )
        .expect("owner mints a share link");

        let ws = ws_of(&tree, id);
        let holder = shell::shell::state::Actor::default()
            .with_principal(Principal::Token("inv-7f3a".into()));
        let path = ResourceRef::FsPath("/var/workspace/design/logo.svg".into());

        assert!(ws.allows(&holder, &path, Verb::Read));
        assert!(
            !ws.allows(&holder, &path, Verb::Write),
            "a read link is not a write link"
        );
        assert!(
            !ws.allows(&holder, &ResourceRef::Workspace, Verb::Read),
            "a share link grants no standing in the room"
        );

        let wrong = shell::shell::state::Actor::default()
            .with_principal(Principal::Token("inv-0000".into()));
        assert!(
            !ws.allows(&wrong, &path, Verb::Read),
            "a different token grants nothing"
        );

        assert!(tree.revoke_in_workspace(id, &ALICE.to_string(), "share-link"));
        assert!(
            !ws_of(&tree, id).allows(&holder, &path, Verb::Read),
            "a revoked link stops working"
        );
    }

    // Phase `an_agent_holds_a_scoped_revocable_capability_set_like_an_app`.
    {
        let (mut tree, id) = room_owned_by(ALICE);
        tree.grant_in_workspace(
            id,
            &ALICE.to_string(),
            CapOverride::grant(
                Subject::agent("assistant"),
                Scope::FsPath("/var/workspace/notes".into()),
                Verb::Write,
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
            Verb::Write
        ));
        assert!(
            !ws.allows(
                &agent,
                &ResourceRef::FsPath("/var/workspace/finance/budget.ods".into()),
                Verb::Read
            ),
            "an agent is confined to what it was given, like any other subject"
        );
        assert!(
            !ws.allows(&agent, &ResourceRef::Workspace, Verb::Admin),
            "and holds no role, so it never inherits room-wide authority"
        );

        assert!(tree.revoke_in_workspace(id, &ALICE.to_string(), "agent-notes"));
        assert!(
            !ws_of(&tree, id).allows(
                &ws_of(&tree, id).actor_for_agent("assistant"),
                &ResourceRef::FsPath("/var/workspace/notes/todo.md".into()),
                Verb::Write
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
