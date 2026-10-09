//! The sandbox capability primitive, exercised through the production
//! decision function ([`decide_capability`]) — the same function the
//! compositor, portal and filesystem call, so nothing here is re-implemented
//! locally.
//!
//! Emphasis is deliberately on **denial**: a model only shown to permit
//! things hasn't been shown to work at all.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use collab_types::caps::{
    Access, Actor, CapDecision, CapOverride, Effect, Principal, ResourceRef, Scope, Subject,
    decide_capability,
};
use collab_types::crypto::Ed25519PublicKey;

fn key(seed: u8) -> Ed25519PublicKey {
    Ed25519PublicKey::from_bytes([seed; 32])
}

fn fs(path: &str) -> ResourceRef {
    ResourceRef::FsPath(path.into())
}

/// `decide_capability` with no clock — every case but the expiry ones.
fn decide(
    actor: &Actor,
    resource: &ResourceRef,
    access: Access,
    caps: &[CapOverride],
) -> CapDecision {
    decide_capability(actor, resource, access, caps, None)
}

/// An app without a capability reaches nothing; a path or host grant reaches
/// exactly its subtree or domain.
#[test]
fn an_app_reaches_only_the_paths_and_hosts_it_was_granted() {
    // Case: an app with no capability is denied everything.
    let app = Actor::app("org.tilo.Ebook");
    for resource in [
        fs("/var/workspace/books/dune.epub"),
        ResourceRef::Device("camera".into()),
        ResourceRef::NetHost("example.com".into()),
        ResourceRef::FsRecord("file-1".into()),
    ] {
        for access in [Access::Read, Access::Write] {
            assert_eq!(
                decide(&app, &resource, access, &[]),
                CapDecision::NotGranted,
                "an app with no capability must be denied {access:?} on {resource:?}"
            );
        }
    }

    // Case: an app granted one path cannot reach a sibling path.
    let caps = vec![CapOverride::grant(
        Subject::app("org.tilo.Ebook"),
        Scope::FsPath("/var/workspace/books".into()),
        Access::Write,
    )];
    assert_eq!(
        decide(
            &app,
            &fs("/var/workspace/books/dune.epub"),
            Access::Write,
            &caps
        ),
        CapDecision::Allow,
        "the granted subtree is reachable"
    );
    assert_eq!(
        decide(
            &app,
            &fs("/var/workspace/finances/tax.ods"),
            Access::Read,
            &caps
        ),
        CapDecision::NotGranted,
        "a sibling directory the app was never granted must be denied"
    );

    // Case: a path grant does not leak across a component boundary.
    let caps = vec![CapOverride::grant(
        Subject::app("org.tilo.Ebook"),
        Scope::FsPath("/var/workspace/docs".into()),
        Access::Write,
    )];
    assert_eq!(
        decide(
            &app,
            &fs("/var/workspace/docs/a/b/c.txt"),
            Access::Write,
            &caps
        ),
        CapDecision::Allow,
        "grants inherit down the tree"
    );
    assert_eq!(
        decide(
            &app,
            &fs("/var/workspace/docsSECRET/keys"),
            Access::Read,
            &caps
        ),
        CapDecision::NotGranted,
        "a string-prefix match that stops mid-component is not a parent directory"
    );

    // Case: a network capability is not a blanket one.
    let browser = Actor::app("org.tilo.Browser");
    let caps = vec![CapOverride::grant(
        Subject::app("org.tilo.Browser"),
        Scope::NetHost(".example.com".into()),
        Access::Write,
    )];
    for host in ["example.com", "api.example.com", "a.b.example.com"] {
        assert_eq!(
            decide(
                &browser,
                &ResourceRef::NetHost(host.into()),
                Access::Write,
                &caps
            ),
            CapDecision::Allow,
            "{host} is in the granted domain"
        );
    }
    for host in ["evil.com", "example.com.evil.com", "notexample.com"] {
        assert_eq!(
            decide(
                &browser,
                &ResourceRef::NetHost(host.into()),
                Access::Write,
                &caps
            ),
            CapDecision::NotGranted,
            "{host} is outside the granted domain and must be denied"
        );
    }
}

/// A workspace-wide grant reaches every sandboxed resource; a restriction
/// caps it, and a device grant reaches only its device.
#[test]
fn restrictions_cap_grants_and_devices_stay_apart() {
    let app = Actor::app("org.tilo.Camera");
    let caps = vec![CapOverride::grant(
        Subject::app("org.tilo.Camera"),
        Scope::Device("camera".into()),
        Access::Read,
    )];
    assert_eq!(
        decide(
            &app,
            &ResourceRef::Device("camera".into()),
            Access::Read,
            &caps
        ),
        CapDecision::Allow
    );
    assert_eq!(
        decide(
            &app,
            &ResourceRef::Device("camera".into()),
            Access::Write,
            &caps
        ),
        CapDecision::NotGranted,
        "reading a device is not driving it"
    );
    assert_eq!(
        decide(
            &app,
            &ResourceRef::Device("microphone".into()),
            Access::Read,
            &caps
        ),
        CapDecision::NotGranted
    );

    let mut caps = vec![CapOverride::grant(
        Subject::app("org.tilo.Camera"),
        Scope::Workspace,
        Access::Write,
    )];
    for resource in [
        fs("/var/workspace/a"),
        ResourceRef::Device("printer".into()),
        ResourceRef::NetHost("example.com".into()),
    ] {
        assert_eq!(
            decide(&app, &resource, Access::Write, &caps),
            CapDecision::Allow
        );
    }
    caps.push(CapOverride::new(
        Subject::app("org.tilo.Camera"),
        Scope::FsPath("/var/workspace/finance".into()),
        Effect::Restrict,
        Access::Read,
    ));
    assert_eq!(
        decide(
            &app,
            &fs("/var/workspace/finance/tax"),
            Access::Write,
            &caps
        ),
        CapDecision::DeniedByRestriction
    );
    assert_eq!(
        decide(&app, &fs("/var/workspace/finance/tax"), Access::Read, &caps),
        CapDecision::Allow
    );
    assert_eq!(
        decide(&app, &fs("/var/workspace/design"), Access::Write, &caps),
        CapDecision::Allow
    );
}

/// Expiry: a capability past its expiry is not "restricted" — it is simply
/// not there. Grants and restrictions lapse symmetrically.
#[test]
fn grants_and_restrictions_lapse_at_their_expiry() {
    let app = Actor::app("org.tilo.Ebook");
    let path = fs("/var/workspace/books/dune.epub");
    let caps = vec![
        CapOverride::grant(
            Subject::app("org.tilo.Ebook"),
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
            decide_capability(&app, &path, Access::Write, &caps, Some(now)),
            want,
            "{why}"
        );
    }

    let caps = vec![
        CapOverride::grant(
            Subject::app("org.tilo.Ebook"),
            Scope::Workspace,
            Access::Write,
        ),
        CapOverride::new(
            Subject::app("org.tilo.Ebook"),
            Scope::Workspace,
            Effect::Restrict,
            Access::Read,
        )
        .with_expiry(50),
    ];
    assert_eq!(
        decide_capability(&app, &path, Access::Write, &caps, Some(10)),
        CapDecision::DeniedByRestriction
    );
    assert_eq!(
        decide_capability(&app, &path, Access::Write, &caps, Some(51)),
        CapDecision::Allow,
        "a lapsed restriction stops biting, symmetrically with a lapsed grant"
    );
}

/// A capability serves exactly its subject: one app, a bearer token, a
/// room's members, or one person however they are named.
#[test]
fn a_capability_serves_only_its_subject() {
    let caps = vec![CapOverride::grant(
        Subject::app("org.tilo.Ebook"),
        Scope::FsPath("/var/workspace/books".into()),
        Access::Read,
    )];
    let path = fs("/var/workspace/books/dune.epub");
    assert_eq!(
        decide(&Actor::app("org.tilo.Ebook"), &path, Access::Read, &caps),
        CapDecision::Allow
    );
    assert_eq!(
        decide(&Actor::app("org.tilo.Files"), &path, Access::Read, &caps),
        CapDecision::NotGranted,
        "the Files app was not the subject of that grant"
    );
    assert_eq!(
        decide(&Actor::agent("assistant"), &path, Access::Read, &caps),
        CapDecision::NotGranted,
        "an agent is its own subject, not a stand-in for any app"
    );

    // Case: a bearer-token holder gets exactly the token's access.
    let caps = vec![CapOverride::grant(
        Subject::token("inv-7f3a"),
        Scope::FsPath("/var/workspace/shared/report.md".into()),
        Access::Read,
    )];
    let path = fs("/var/workspace/shared/report.md");
    let holder = Actor::default().with_principal(Principal::Token("inv-7f3a".into()));
    assert_eq!(
        decide(&holder, &path, Access::Read, &caps),
        CapDecision::Allow
    );
    assert_eq!(
        decide(&holder, &path, Access::Write, &caps),
        CapDecision::NotGranted,
        "a read link is not a write link"
    );
    let wrong_token = Actor::default().with_principal(Principal::Token("inv-0000".into()));
    assert_eq!(
        decide(&wrong_token, &path, Access::Read, &caps),
        CapDecision::NotGranted,
        "presenting a different token grants nothing"
    );
    assert_eq!(
        decide(&Actor::default(), &path, Access::Read, &caps),
        CapDecision::NotGranted,
        "an actor presenting no principal holds nothing"
    );
}

/// A room subject covers exactly that room's members, and one person is one
/// subject whether named by key or by hex id.
#[test]
fn rooms_and_people_are_matched_by_identity() {
    let caps = vec![CapOverride::grant(
        Subject::room("ws-design"),
        Scope::FsPath("/var/workspace/design".into()),
        Access::Write,
    )];
    let path = fs("/var/workspace/design/logo.svg");
    let insider = Actor::person("bob").in_room("ws-design");
    let outsider = Actor::person("carol").in_room("ws-finance");
    assert_eq!(
        decide(&insider, &path, Access::Write, &caps),
        CapDecision::Allow,
        "membership of the granted room carries the capability"
    );
    assert_eq!(
        decide(&outsider, &path, Access::Write, &caps),
        CapDecision::NotGranted,
        "being in a *different* room grants nothing here"
    );

    let k = key(3);
    let hex = Principal::person_from_key(k);
    let Principal::Person(hex_id) = hex.clone() else {
        panic!("person_from_key must produce a Person principal");
    };
    assert_eq!(hex_id.len(), 64, "32 bytes as lowercase hex");
    let path = fs("/var/workspace/a.txt");
    let by_key = vec![CapOverride::grant(
        Subject::User(k),
        Scope::FsPath("/var/workspace".into()),
        Access::Read,
    )];
    let by_id = vec![CapOverride::grant(
        Subject::Principal(hex),
        Scope::FsPath("/var/workspace".into()),
        Access::Read,
    )];
    let id_actor = Actor::person(hex_id);
    assert_eq!(
        decide(&id_actor, &path, Access::Read, &by_key),
        CapDecision::Allow
    );
    let key_actor = Actor::signed(k);
    assert_eq!(
        decide(&key_actor, &path, Access::Read, &by_id),
        CapDecision::Allow
    );
    let other = Actor::signed(key(4));
    assert_eq!(
        decide(&other, &path, Access::Read, &by_key),
        CapDecision::NotGranted
    );
    assert_eq!(
        decide(&other, &path, Access::Read, &by_id),
        CapDecision::NotGranted
    );
}

#[test]
fn a_capability_round_trips_through_json() {
    let cap = CapOverride::grant(
        Subject::person("bob"),
        Scope::FsPath("/var/workspace/design".into()),
        Access::Write,
    )
    .with_id("cap-1")
    .with_expiry(1_900_000_000)
    .granted_by(Principal::Person("alice".into()));
    let json = serde_json::to_string(&cap).expect("serialise");
    let back: CapOverride = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(cap, back);
}
