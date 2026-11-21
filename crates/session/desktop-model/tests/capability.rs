//! Coverage for the workspace capability store. Pure schema tests — no
//! portal plumbing is exercised here.

/// The clock these fixtures run at, Unix seconds.
const NOW: u64 = 1_800_000_000;

use collab_types::caps::{Access, CapDecision, CapOverride, DeviceCap, ResourceRef, Scope};
use tilo_action_policy::{Grantee, Lifetime};
use tilo_desktop_model::*;

fn grant(app: &str, scope: Scope, access: Access, lifetime: Lifetime) -> CapOverride {
    CapOverride::grant(Grantee::person("ada"), scope, access)
        .through(app)
        .lasting(lifetime)
}

tilo_test_env::scenario! {
    fn capability_grants_start_empty_and_round_trip;

    phase a_new_workspace_holds_no_capabilities_and_grants_nothing;
    {
        // Both `Workspace::default()` (used by `..Default::default()`
        // sweeps) and `Workspace::new(...)` must start empty, and an
        // empty workspace must deny rather than default-allow.
        for ws in [
            Workspace::default(),
            Workspace::new(WorkspaceId::from_u128(1), "Demo"),
        ] {
            assert!(ws.sandbox.is_empty());
            assert!(
                !ws.may_govern_at(
                    "anyone",
                    tilo_action_policy::roles::governance::SETTINGS,
                    NOW
                ),
                "a workspace with no owner has no administrators"
            );
            assert_ne!(
                ws.sandbox_decide_at(
                    "org.gnome.gedit",
                    &ResourceRef::FsPath("/home/u/work".into()),
                    Access::Read,
                    NOW
                ),
                CapDecision::Allow,
                "an app with no capability is denied"
            );
        }
    }

    phase grants_round_trip_via_serde_json;
    {
        let mut ws = Workspace::new(WorkspaceId::from_u128(1), "Demo");
        ws.grant_sandbox(grant(
            "org.gnome.gedit",
            Scope::FsPath("/home/u/work".into()),
            Access::Write,
            Lifetime::UntilRevoked,
        ));
        ws.grant_sandbox(grant(
            "org.gnome.gedit",
            Scope::NetHost("github.com".into()),
            Access::Write,
            Lifetime::Session,
        ));

        let json = serde_json::to_string(&ws).expect("serialize");
        let round: Workspace = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(round.sandbox, ws.sandbox);
        assert_eq!(round.app_capabilities_at("org.gnome.gedit", NOW).len(), 2);

        // And the decision function agrees after the round-trip.
        let app = "org.gnome.gedit";
        assert_eq!(
            round.sandbox_decide_at(
                app,
                &ResourceRef::FsPath("/home/u/work/notes.txt".into()),
                Access::Write,
                NOW
            ),
            CapDecision::Allow
        );
        assert_ne!(
            round.sandbox_decide_at(
                app,
                &ResourceRef::FsPath("/home/u/secrets".into()),
                Access::Read,
                NOW
            ),
            CapDecision::Allow,
            "a path outside the grant stays denied across serialisation"
        );
        assert_eq!(
            round.sandbox_decide_at(
                app,
                &ResourceRef::NetHost("github.com".into()),
                Access::Write,
                NOW
            ),
            CapDecision::Allow
        );
        assert_ne!(
            round.sandbox_decide_at(
                app,
                &ResourceRef::NetHost("evil.example".into()),
                Access::Write,
                NOW
            ),
            CapDecision::Allow,
            "and a host outside the allowlist stays denied"
        );
    }

    phase every_device_round_trips_through_serde_under_its_name;
    {
        for device in DeviceCap::ALL {
            let json = serde_json::to_string(&Scope::Device(device)).expect("serialize");
            assert_eq!(json, format!("{{\"Device\":\"{}\"}}", device.name()));
            let round: Scope = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(round, Scope::Device(device));
            assert_eq!(DeviceCap::from_name(device.name()), Some(device));
        }
    }

    phase the_next_grant_expiry_is_the_earliest_one_still_ahead;
    // What the broker re-checks at: the earliest expiry still ahead, over every
    // workspace, and nothing for grants that never expire or already did.
    {
        use tilo_action_policy::{Answer, Grant, KindSet, TreePath};
        const SOON: u64 = NOW + 5;
        const LATER: u64 = NOW + 60;
        let member = |person: &str, until: Option<u64>| {
            let grant = Grant::new(
                format!("member-{person}"),
                Grantee::person(person),
                TreePath::root(),
                KindSet::ALL,
                Answer::Allow,
                Grantee::person("owner"),
            );
            match until {
                Some(at) => grant.with_lifetime(Lifetime::Until(at * 1000)),
                None => grant,
            }
        };
        let mut first = Workspace::new(WorkspaceId::from_u128(1), "First");
        first.grant(member("forever", None));
        assert_eq!(first.next_grant_expiry_after(NOW), None);
        first.grant(member("past", Some(NOW)));
        assert_eq!(
            first.next_grant_expiry_after(NOW),
            None,
            "a grant ending at `now` is already over"
        );
        first.grant(member("later", Some(LATER)));
        let mut second = Workspace::new(WorkspaceId::from_u128(2), "Second");
        second.grant_sandbox(grant(
            "org.gnome.gedit",
            Scope::FsPath("/home/u/work".into()),
            Access::Write,
            Lifetime::Until(SOON * 1000),
        ));
        assert_eq!(first.next_grant_expiry_after(NOW), Some(LATER));
        assert_eq!(second.next_grant_expiry_after(NOW), Some(SOON));
        let tree = Layout {
            workspaces: vec![first, second],
            ..Layout::default()
        };
        assert_eq!(tree.next_grant_expiry_after(NOW), Some(SOON));
        assert_eq!(tree.next_grant_expiry_after(SOON), Some(LATER));
        assert_eq!(tree.next_grant_expiry_after(LATER), None);
    }

    phase a_windows_desktop_control_ends_when_it_is_taken_back;
    // Desktop control is an action grant naming the app that holds it, read
    // and taken back for that app alone; its other grants stay.
    {
        const TERMINAL: &str = "org.example.terminal";
        let mut ws = Workspace::new(WorkspaceId::from_u128(1), "Work");
        assert!(!ws.holds_desktop_control_at(TERMINAL, NOW));
        assert!(!ws.stop_desktop_control(TERMINAL), "nothing to take back");
        let control = ws.desktop_control_grant(TERMINAL, Grantee::person("ada"));
        ws.grant(control);
        ws.grant_sandbox(grant(
            TERMINAL,
            Scope::NetHost("*".into()),
            Access::Write,
            Lifetime::UntilRevoked,
        ));
        assert!(ws.holds_desktop_control_at(TERMINAL, NOW));
        assert!(
            !ws.holds_desktop_control_at("org.example.other", NOW),
            "one app's grant is not another's"
        );
        assert!(ws.stop_desktop_control(TERMINAL));
        assert!(!ws.holds_desktop_control_at(TERMINAL, NOW));
        assert_eq!(
            ws.app_capabilities_at(TERMINAL, NOW).len(),
            1,
            "its network grant stays"
        );
        assert!(!ws.stop_desktop_control(TERMINAL), "taken back once");
    }
}

tilo_test_env::scenario! {
    fn app_kind_app_id_is_reverse_dns_or_the_verbatim_label;

    phase app_kind_app_id_is_a_reverse_dns_id_not_the_display_label;
    {
        // The grant key must be a real app-id, in the same namespace as
        // an adopted client's `set_app_id` — never the display label.
        assert_eq!(AppKind::FileExplorer.app_id(), "so.tilo.file-explorer");
        assert_eq!(AppKind::Terminal.app_id(), "so.tilo.terminal");
        assert_eq!(AppKind::Browser.app_id(), "so.tilo.browser");
        assert_eq!(AppKind::Settings.app_id(), "so.tilo.settings");
        // A bare `Other` label no package answers to keeps its built-in id.
        assert_eq!(
            AppKind::Other("Scratch".into()).app_id(),
            "org.tilo.Scratch"
        );
        for kind in [
            AppKind::FileExplorer,
            AppKind::Terminal,
            AppKind::Browser,
            AppKind::Settings,
            AppKind::Other("Scratch".into()),
        ] {
            assert_ne!(
                kind.app_id(),
                kind.as_str(),
                "app_id must not collapse to the display label for {kind:?}"
            );
        }
    }

    phase app_kind_app_id_trusts_reverse_dns_other_labels_verbatim;
    {
        // An `Other` value already in reverse-DNS form is used as-is,
        // not double-prefixed.
        assert_eq!(
            AppKind::Other("org.gimp.GIMP".into()).app_id(),
            "org.gimp.GIMP"
        );
    }
}
