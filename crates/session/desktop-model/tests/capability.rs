//! Coverage for [`Capability`] / [`GrantScope`] / [`FsMode`] and the
//! workspace capability store. Pure schema tests — no portal plumbing is
//! exercised here.

use std::path::PathBuf;

/// The clock these fixtures run at, Unix seconds.
const NOW: u64 = 1_800_000_000;

use collab_types::caps::{Access, Actor, CapDecision, ResourceRef};
use tilo_desktop_model::*;

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
)]
fn capability_grants_start_empty_and_round_trip() {
    // Phase `a_new_workspace_holds_no_capabilities_and_grants_nothing`.
    {
        // Both `Workspace::default()` (used by `..Default::default()`
        // sweeps) and `Workspace::new(...)` must start empty, and an
        // empty room must deny rather than default-allow.
        for ws in [
            Workspace::default(),
            Workspace::new(WorkspaceId::from(1), "Demo"),
        ] {
            assert!(ws.sandbox.is_empty());
            assert!(ws.app_grants_snapshot_at(NOW).is_empty());
            assert!(!ws.has_app_grants_at(NOW));
            assert!(
                !ws.may_govern_at(
                    "anyone",
                    tilo_action_policy::roles::governance::SETTINGS,
                    NOW
                ),
                "a room with no owner has no administrators"
            );
            assert_ne!(
                ws.sandbox_decide_at(
                    &Actor::app("org.gnome.gedit"),
                    &ResourceRef::FsPath("/home/u/work".into()),
                    Access::Read,
                    NOW
                ),
                CapDecision::Allow,
                "an app with no capability is denied"
            );
        }
    }
    // Phase `grants_round_trip_via_serde_json`.
    {
        let mut ws = Workspace::new(WorkspaceId::from(1), "Demo");
        ws.grant_app(
            "org.gnome.gedit",
            &Capability::Filesystem {
                path: PathBuf::from("/home/u/work"),
                mode: FsMode::ReadWrite,
                scope: GRANT_ALWAYS,
            },
        );
        ws.grant_app(
            "org.gnome.gedit",
            &Capability::Network {
                hosts: Some(vec!["github.com".into()]),
                scope: GRANT_SESSION,
            },
        );

        let json = serde_json::to_string(&ws).expect("serialize");
        let round: Workspace = serde_json::from_str(&json).expect("deserialize");

        let view = round.app_grants_snapshot_at(NOW);
        assert_eq!(view.len(), 1);
        let caps = view.get("org.gnome.gedit").expect("app present");
        assert_eq!(caps.len(), 2);
        assert!(caps.contains(&Capability::Filesystem {
            path: PathBuf::from("/home/u/work"),
            mode: FsMode::ReadWrite,
            scope: GRANT_ALWAYS,
        }));
        assert!(caps.contains(&Capability::Network {
            hosts: Some(vec!["github.com".into()]),
            scope: GRANT_SESSION,
        }));

        // And the decision function agrees after the round-trip.
        let app = Actor::app("org.gnome.gedit");
        assert_eq!(
            round.sandbox_decide_at(
                &app,
                &ResourceRef::FsPath("/home/u/work/notes.txt".into()),
                Access::Write,
                NOW
            ),
            CapDecision::Allow
        );
        assert_ne!(
            round.sandbox_decide_at(
                &app,
                &ResourceRef::FsPath("/home/u/secrets".into()),
                Access::Read,
                NOW
            ),
            CapDecision::Allow,
            "a path outside the grant stays denied across serialisation"
        );
        assert_eq!(
            round.sandbox_decide_at(
                &app,
                &ResourceRef::NetHost("github.com".into()),
                Access::Write,
                NOW
            ),
            CapDecision::Allow
        );
        assert_ne!(
            round.sandbox_decide_at(
                &app,
                &ResourceRef::NetHost("evil.example".into()),
                Access::Write,
                NOW
            ),
            CapDecision::Allow,
            "and a host outside the allowlist stays denied"
        );
    }
    // Phase `all_capability_variants_round_trip_through_serde`.
    {
        // Pin the wire shape of every variant so a downstream change
        // breaks loudly here rather than silently in the portal backend.
        let caps = vec![
            Capability::Filesystem {
                path: PathBuf::from("/tmp/ws"),
                mode: FsMode::Read,
                scope: GRANT_ONCE,
            },
            Capability::Camera {
                scope: GRANT_SESSION,
            },
            Capability::Microphone {
                scope: GRANT_ALWAYS,
            },
            Capability::Screencast { scope: GRANT_ONCE },
            Capability::Network {
                hosts: None,
                scope: GRANT_ALWAYS,
            },
        ];
        let json = serde_json::to_string(&caps).expect("serialize caps");
        let round: Vec<Capability> = serde_json::from_str(&json).expect("deserialize caps");
        assert_eq!(round, caps);
    }
}

#[test]
fn app_kind_app_id_is_reverse_dns_or_the_verbatim_label() {
    // Phase `app_kind_app_id_is_a_reverse_dns_id_not_the_display_label`.
    {
        // The grant key must be a real app-id, in the same namespace as
        // an adopted client's `set_app_id` — never the display label.
        assert_eq!(AppKind::FileExplorer.app_id(), "so.tilo.file-explorer");
        assert_eq!(AppKind::Terminal.app_id(), "so.tilo.terminal");
        assert_eq!(AppKind::Browser.app_id(), "so.tilo.browser");
        assert_eq!(AppKind::Settings.app_id(), "so.tilo.settings");
        // A bare `Other` label no package answers to keeps its built-in id.
        assert_eq!(AppKind::Other("Editor".into()).app_id(), "org.tilo.Editor");
        for kind in [
            AppKind::FileExplorer,
            AppKind::Terminal,
            AppKind::Browser,
            AppKind::Settings,
            AppKind::Other("Editor".into()),
        ] {
            assert_ne!(
                kind.app_id(),
                kind.as_str(),
                "app_id must not collapse to the display label for {kind:?}"
            );
        }
    }
    // Phase `app_kind_app_id_trusts_reverse_dns_other_labels_verbatim`.
    {
        // An `Other` value already in reverse-DNS form is used as-is,
        // not double-prefixed.
        assert_eq!(
            AppKind::Other("org.gimp.GIMP".into()).app_id(),
            "org.gimp.GIMP"
        );
    }
}

/// What the broker re-checks at: the earliest expiry still ahead, over every
/// workspace, and nothing for grants that never expire or already did.
#[test]
fn the_next_grant_expiry_is_the_earliest_one_still_ahead() {
    use tilo_action_policy::{Answer, Grant, KindSet, Lifetime, Subject, TreePath};
    const SOON: u64 = NOW + 5;
    const LATER: u64 = NOW + 60;
    let member = |person: &str, until: Option<u64>| {
        let grant = Grant::new(
            format!("member-{person}"),
            Subject::person(person),
            TreePath::root(),
            KindSet::ALL,
            Answer::Allow,
            Subject::person("owner"),
        );
        match until {
            Some(at) => grant.with_lifetime(Lifetime::Until(at * 1000)),
            None => grant,
        }
    };
    let mut first = Workspace::new(WorkspaceId::from(1), "First");
    first.grant(member("forever", None));
    assert_eq!(first.next_grant_expiry_after(NOW), None);
    first.grant(member("past", Some(NOW)));
    assert_eq!(
        first.next_grant_expiry_after(NOW),
        None,
        "a grant ending at `now` is already over"
    );
    first.grant(member("later", Some(LATER)));
    let mut second = Workspace::new(WorkspaceId::from(2), "Second");
    second.grant(member("sooner", Some(SOON)));
    assert_eq!(first.next_grant_expiry_after(NOW), Some(LATER));
    assert_eq!(second.next_grant_expiry_after(NOW), Some(SOON));
    let tree = DesktopTree {
        workspaces: vec![first, second],
        ..DesktopTree::default()
    };
    assert_eq!(tree.next_grant_expiry_after(NOW), Some(SOON));
    assert_eq!(tree.next_grant_expiry_after(SOON), Some(LATER));
    assert_eq!(tree.next_grant_expiry_after(LATER), None);
}
