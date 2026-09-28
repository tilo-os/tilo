//! Coverage for [`Capability`] / [`GrantScope`] / [`FsMode`] and the
//! workspace capability store. Pure schema tests — no portal plumbing is
//! exercised here.

use std::path::PathBuf;

/// The clock these fixtures run at, Unix seconds.
const NOW: u64 = 1_800_000_000;

use collab_types::caps::{ResourceRef, Verb};
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
            assert!(ws.capabilities.is_empty());
            assert!(ws.app_grants_snapshot_at(NOW).is_empty());
            assert!(!ws.has_app_grants_at(NOW));
            assert!(
                !ws.may_administer_at(&"anyone".to_string(), NOW),
                "a room with no owner has no administrators"
            );
            assert!(
                !ws.allows_at(
                    &ws.actor_for_app("org.gnome.gedit"),
                    &ResourceRef::FsPath("/home/u/work".into()),
                    Verb::Read,
                    NOW
                ),
                "an app with no capability is denied"
            );
        }
    }
    // Phase `grants_round_trip_via_serde_json`.
    {
        // Pins the on-disk schema for portal-side consumers.
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
        let app = round.actor_for_app("org.gnome.gedit");
        assert!(round.allows_at(
            &app,
            &ResourceRef::FsPath("/home/u/work/notes.txt".into()),
            Verb::Write,
            NOW
        ));
        assert!(
            !round.allows_at(
                &app,
                &ResourceRef::FsPath("/home/u/secrets".into()),
                Verb::Read,
                NOW
            ),
            "a path outside the grant stays denied across serialisation"
        );
        assert!(round.allows_at(
            &app,
            &ResourceRef::NetHost("github.com".into()),
            Verb::Write,
            NOW
        ));
        assert!(
            !round.allows_at(
                &app,
                &ResourceRef::NetHost("evil.example".into()),
                Verb::Write,
                NOW
            ),
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
        // The app-id is distinct from the display label for every kind.
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
