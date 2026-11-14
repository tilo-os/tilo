//! The sandbox capability primitive, exercised through the production
//! decision functions ([`decide_capability`], [`decide_app`]) — the same
//! functions the compositor, portal and filesystem call, so nothing here is
//! re-implemented locally.
//!
//! Emphasis is deliberately on **denial**: a model only shown to permit
//! things hasn't been shown to work at all.

use collab_types::caps::{
    Access, CapDecision, CapOverride, Effect, ResourceRef, Scope, decide_app, decide_capability,
};
use tilo_action_policy::{Acting, Grantee, MemberId, Role};

fn fs(path: &str) -> ResourceRef {
    ResourceRef::FsPath(path.into())
}

/// `decide_capability` with no clock — every case but the expiry ones.
fn decide(
    acting: &Acting,
    resource: &ResourceRef,
    access: Access,
    caps: &[CapOverride],
) -> CapDecision {
    decide_capability(acting, resource, access, caps, None)
}

fn app_grant(app: &str, scope: Scope, access: Access) -> CapOverride {
    CapOverride::grant(Grantee::person("ada"), scope, access).through(app)
}

tilo_test_env::scenario! {
    /// The capability model end to end, each case on capabilities of its own.
    fn the_capability_model_decides_by_grantee_scope_and_expiry;

    phase an_app_reaches_only_the_paths_and_hosts_it_was_granted;
    // An app without a capability reaches nothing; a path or host grant reaches
    // exactly its subtree or domain.
    {
        // Case: an app with no capability is denied everything.
        for resource in [
            fs("/var/workspace/books/dune.epub"),
            ResourceRef::Device("camera".into()),
            ResourceRef::NetHost("example.com".into()),
            ResourceRef::FsRecord("file-1".into()),
        ] {
            for access in [Access::Read, Access::Write] {
                assert_eq!(
                    decide_app("org.tilo.Ebook", &resource, access, &[], None),
                    CapDecision::NotGranted,
                    "an app with no capability must be denied {access:?} on {resource:?}"
                );
            }
        }

        // Case: an app granted one path cannot reach a sibling path.
        let caps = vec![app_grant(
            "org.tilo.Ebook",
            Scope::FsPath("/var/workspace/books".into()),
            Access::Write,
        )];
        assert_eq!(
            decide_app(
                "org.tilo.Ebook",
                &fs("/var/workspace/books/dune.epub"),
                Access::Write,
                &caps,
                None
            ),
            CapDecision::Allow,
            "the granted subtree is reachable"
        );
        assert_eq!(
            decide_app(
                "org.tilo.Ebook",
                &fs("/var/workspace/finances/tax.ods"),
                Access::Read,
                &caps,
                None
            ),
            CapDecision::NotGranted,
            "a sibling directory the app was never granted must be denied"
        );

        // Case: a path grant does not leak across a component boundary.
        let caps = vec![app_grant(
            "org.tilo.Ebook",
            Scope::FsPath("/var/workspace/docs".into()),
            Access::Write,
        )];
        assert_eq!(
            decide_app(
                "org.tilo.Ebook",
                &fs("/var/workspace/docs/a/b/c.txt"),
                Access::Write,
                &caps,
                None
            ),
            CapDecision::Allow,
            "grants inherit down the tree"
        );
        assert_eq!(
            decide_app(
                "org.tilo.Ebook",
                &fs("/var/workspace/docsSECRET/keys"),
                Access::Read,
                &caps,
                None
            ),
            CapDecision::NotGranted,
            "a string-prefix match that stops mid-component is not a parent directory"
        );

        // Case: a network capability is not a blanket one.
        let caps = vec![app_grant(
            "org.tilo.Browser",
            Scope::NetHost(".example.com".into()),
            Access::Write,
        )];
        for host in ["example.com", "api.example.com", "a.b.example.com"] {
            assert_eq!(
                decide_app(
                    "org.tilo.Browser",
                    &ResourceRef::NetHost(host.into()),
                    Access::Write,
                    &caps,
                    None
                ),
                CapDecision::Allow,
                "{host} is in the granted domain"
            );
        }
        for host in ["evil.com", "example.com.evil.com", "notexample.com"] {
            assert_eq!(
                decide_app(
                    "org.tilo.Browser",
                    &ResourceRef::NetHost(host.into()),
                    Access::Write,
                    &caps,
                    None
                ),
                CapDecision::NotGranted,
                "{host} is outside the granted domain and must be denied"
            );
        }
    }

    phase restrictions_cap_grants_and_devices_stay_apart;
    // A restriction caps a grant, and a device grant reaches only its device.
    {
        let caps = vec![app_grant(
            "org.tilo.Camera",
            Scope::Device("camera".into()),
            Access::Read,
        )];
        assert_eq!(
            decide_app(
                "org.tilo.Camera",
                &ResourceRef::Device("camera".into()),
                Access::Read,
                &caps,
                None
            ),
            CapDecision::Allow
        );
        assert_eq!(
            decide_app(
                "org.tilo.Camera",
                &ResourceRef::Device("camera".into()),
                Access::Write,
                &caps,
                None
            ),
            CapDecision::NotGranted,
            "reading a device is not driving it"
        );
        assert_eq!(
            decide_app(
                "org.tilo.Camera",
                &ResourceRef::Device("microphone".into()),
                Access::Read,
                &caps,
                None
            ),
            CapDecision::NotGranted
        );

        let mut caps = vec![app_grant(
            "org.tilo.Camera",
            Scope::FsPath("/var/workspace".into()),
            Access::Write,
        )];
        caps.push(
            CapOverride::new(
                Grantee::person("ada"),
                Scope::FsPath("/var/workspace/finance".into()),
                Effect::Restrict,
                Access::Read,
            )
            .through("org.tilo.Camera"),
        );
        assert_eq!(
            decide_app(
                "org.tilo.Camera",
                &fs("/var/workspace/finance/tax"),
                Access::Write,
                &caps,
                None
            ),
            CapDecision::DeniedByRestriction
        );
        assert_eq!(
            decide_app(
                "org.tilo.Camera",
                &fs("/var/workspace/finance/tax"),
                Access::Read,
                &caps,
                None
            ),
            CapDecision::Allow
        );
        assert_eq!(
            decide_app(
                "org.tilo.Camera",
                &fs("/var/workspace/design"),
                Access::Write,
                &caps,
                None
            ),
            CapDecision::Allow
        );
    }

    phase grants_and_restrictions_lapse_at_their_expiry;
    // Expiry: a capability past its expiry is not "restricted" — it is simply
    // not there. Grants and restrictions lapse symmetrically.
    {
        let app = "org.tilo.Ebook";
        let path = fs("/var/workspace/books/dune.epub");
        let caps = vec![
            app_grant(
                app,
                Scope::FsPath("/var/workspace/books".into()),
                Access::Write,
            )
            .with_expiry(1_000),
        ];
        for (now, want, why) in [
            (999, CapDecision::Allow, "live one second before expiry"),
            (
                1_000,
                CapDecision::NotGranted,
                "expiry is exclusive: at the expiry instant the capability is gone",
            ),
            (5_000, CapDecision::NotGranted, "and stays gone"),
        ] {
            assert_eq!(
                decide_app(app, &path, Access::Write, &caps, Some(now)),
                want,
                "{why}"
            );
        }

        let caps = vec![
            app_grant(app, Scope::FsPath("/var/workspace".into()), Access::Write),
            CapOverride::new(
                Grantee::person("ada"),
                Scope::FsPath("/var/workspace".into()),
                Effect::Restrict,
                Access::Read,
            )
            .through(app)
            .with_expiry(50),
        ];
        assert_eq!(
            decide_app(app, &path, Access::Write, &caps, Some(10)),
            CapDecision::DeniedByRestriction
        );
        assert_eq!(
            decide_app(app, &path, Access::Write, &caps, Some(51)),
            CapDecision::Allow,
            "a lapsed restriction stops biting, symmetrically with a lapsed grant"
        );
    }

    phase a_capability_serves_only_its_grantee;
    // A capability serves exactly its grantee: one app, one member, or a role.
    {
        // Case: an app's capability is that app's, whoever runs it.
        let caps = vec![app_grant(
            "org.tilo.Ebook",
            Scope::FsPath("/var/workspace/books".into()),
            Access::Read,
        )];
        let path = fs("/var/workspace/books/dune.epub");
        assert_eq!(
            decide_app("org.tilo.Ebook", &path, Access::Read, &caps, None),
            CapDecision::Allow
        );
        assert_eq!(
            decide_app("org.tilo.Files", &path, Access::Read, &caps, None),
            CapDecision::NotGranted,
            "the Files app was not the app of that grant"
        );
        assert_eq!(
            decide_app("agent:assistant", &path, Access::Read, &caps, None),
            CapDecision::NotGranted,
            "an agent's id is not an app id"
        );
        assert_eq!(
            decide(&Acting::person("ada"), &path, Access::Read, &caps),
            CapDecision::NotGranted,
            "an app's sandbox is not the person's own reach"
        );
        assert_eq!(
            decide(
                &Acting::person("ada").in_app("org.tilo.Ebook"),
                &path,
                Access::Read,
                &caps
            ),
            CapDecision::Allow,
            "the app acting for its person reaches it"
        );

        // Case: a member's capability is reached by every node of the acting tree,
        // and by nobody else.
        let caps = vec![CapOverride::grant(
            Grantee::person("ada"),
            Scope::FsPath("/var/workspace/ada".into()),
            Access::Read,
        )];
        let path = fs("/var/workspace/ada/plan.md");
        assert_eq!(
            decide(&Acting::person("ada"), &path, Access::Read, &caps),
            CapDecision::Allow
        );
        assert_eq!(
            decide(
                &Acting::agent("agent:scribe").acting_for(Acting::person("ada")),
                &path,
                Access::Read,
                &caps
            ),
            CapDecision::Allow,
            "an agent working for the member reaches what the member does"
        );
        assert_eq!(
            decide(&Acting::person("bob"), &path, Access::Read, &caps),
            CapDecision::NotGranted
        );
        assert_eq!(
            decide(&Acting::agent("agent:scribe"), &path, Access::Read, &caps),
            CapDecision::NotGranted,
            "an agent acting for nobody reaches nothing"
        );

        // Case: a role names its holders through the workspace's policy, which is
        // not what a published list knows: the list names its members (see
        // `crates/session/session/src/fs_authority.rs`).
        let caps = vec![CapOverride::grant(
            Grantee::Role(Role::Viewer),
            Scope::FsPath("/var/workspace/design".into()),
            Access::Read,
        )];
        assert_eq!(
            decide(
                &Acting::person("bob"),
                &fs("/var/workspace/design/logo.svg"),
                Access::Read,
                &caps
            ),
            CapDecision::NotGranted
        );
    }

    phase a_capability_round_trips_through_json;
    {
        let cap = CapOverride::grant(
            Grantee::Member(MemberId::agent("helper")),
            Scope::FsPath("/var/workspace/design".into()),
            Access::Write,
        )
        .through("so.tilo.notes")
        .with_id("cap-1")
        .with_expiry(1_900_000_000)
        .granted_by(MemberId::person("alice"));
        let json = serde_json::to_string(&cap).expect("serialise");
        let back: CapOverride = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(cap, back);
    }
}

/// A person's own capability is theirs through whatever app they run, and only
/// the app a capability names carries the one that names it.
#[test]
fn an_app_scopes_a_capability_to_that_app() {
    let caps = vec![
        CapOverride::grant(
            Grantee::person("ada"),
            Scope::FsPath("/var/workspace/ada".into()),
            Access::Read,
        ),
        CapOverride::grant(
            Grantee::person("ada"),
            Scope::FsPath("/var/workspace/photos".into()),
            Access::Read,
        )
        .through("org.tilo.Photos"),
    ];
    let ada = || Acting::person("ada");
    let own = fs("/var/workspace/ada/plan.md");
    let photos = fs("/var/workspace/photos/cat.jpg");
    assert_eq!(
        decide(&ada(), &own, Access::Read, &caps),
        CapDecision::Allow
    );
    assert_eq!(
        decide(&ada().in_app("org.tilo.Photos"), &own, Access::Read, &caps),
        CapDecision::Allow,
        "an app reached for its person reaches what the person holds"
    );
    assert_eq!(
        decide(&ada(), &photos, Access::Read, &caps),
        CapDecision::NotGranted,
        "a capability naming an app is not the person's own reach"
    );
    assert_eq!(
        decide(
            &ada().in_app("org.tilo.Photos"),
            &photos,
            Access::Read,
            &caps
        ),
        CapDecision::Allow
    );
    assert_eq!(
        decide(
            &ada().in_app("org.tilo.Notes"),
            &photos,
            Access::Read,
            &caps
        ),
        CapDecision::NotGranted,
        "another app does not reach it"
    );
    assert_eq!(
        decide(
            &Acting::person("bob").in_app("org.tilo.Photos"),
            &photos,
            Access::Read,
            &caps
        ),
        CapDecision::NotGranted,
        "another person does not reach it either"
    );
}
