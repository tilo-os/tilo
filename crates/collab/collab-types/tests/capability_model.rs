//! The capability primitive, exercised through the production decision
//! functions ([`decide_capability`]/[`decide_actor`]) — the same
//! functions the compositor, portal, filesystem and action broker call,
//! so nothing here is re-implemented locally.
//!
//! Emphasis is deliberately on **denial**: a model only shown to permit
//! things hasn't been shown to work at all.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

use collab_types::caps::{
    Actor, CapDecision, CapOverride, Effect, Expiry, Principal, ResourceRef, Scope, Subject, Verb,
    decide_capability, role_base_applies,
};
use collab_types::channel::ArtifactId;
use collab_types::crypto::Ed25519PublicKey;
use collab_types::role::{ArtifactKind, Role};

fn key(seed: u8) -> Ed25519PublicKey {
    Ed25519PublicKey::from_bytes([seed; 32])
}

fn artifact_of(app: &str, id: ArtifactId) -> ResourceRef {
    ResourceRef::Artifact {
        app: Some(app.to_string()),
        kind: ArtifactKind::Note,
        instance: id,
    }
}

// Apps hold no role, so they can only ever act through a capability
// — what makes the portal prompt the one user-facing dialog.

#[test]
fn an_app_with_no_capability_is_denied_everything() {
    let app = Actor::app("org.qos.Ebook");
    let doc = artifact_of("org.qos.Notes", ArtifactId::new());

    for verb in [Verb::Read, Verb::Comment, Verb::Write, Verb::Admin] {
        assert_eq!(
            decide_capability(&app, &doc, verb, &[], None),
            CapDecision::DeniedByRole,
            "an app with no capability must be denied {verb:?}; it holds no role \
             and therefore has no base authority to fall back on"
        );
    }
}

#[test]
fn an_app_granted_one_path_cannot_reach_a_sibling_path() {
    let app = Actor::app("org.qos.Ebook");
    let caps = vec![CapOverride::grant(
        Subject::app("org.qos.Ebook"),
        Scope::FsPath("/var/workspace/books".into()),
        Verb::Write,
    )];

    assert_eq!(
        decide_capability(
            &app,
            &ResourceRef::FsPath("/var/workspace/books/dune.epub".into()),
            Verb::Write,
            &caps,
            None
        ),
        CapDecision::Allow,
        "the granted subtree is reachable"
    );

    // The whole point of the portal prompt: reaching outside what the
    // app owns is denied until the user says otherwise.
    assert_eq!(
        decide_capability(
            &app,
            &ResourceRef::FsPath("/var/workspace/finances/tax.ods".into()),
            Verb::Read,
            &caps,
            None
        ),
        CapDecision::DeniedByRole,
        "a sibling directory the app was never granted must be denied"
    );
}

#[test]
fn a_path_grant_does_not_leak_across_a_component_boundary() {
    let app = Actor::app("org.qos.Ebook");
    let caps = vec![CapOverride::grant(
        Subject::app("org.qos.Ebook"),
        Scope::FsPath("/var/workspace/docs".into()),
        Verb::Write,
    )];

    assert_eq!(
        decide_capability(
            &app,
            &ResourceRef::FsPath("/var/workspace/docs/a/b/c.txt".into()),
            Verb::Write,
            &caps,
            None
        ),
        CapDecision::Allow,
        "grants inherit down the tree"
    );

    // `/var/workspace/docsSECRET` shares a string prefix with the
    // granted path but is not underneath it.
    assert_eq!(
        decide_capability(
            &app,
            &ResourceRef::FsPath("/var/workspace/docsSECRET/keys".into()),
            Verb::Read,
            &caps,
            None
        ),
        CapDecision::DeniedByRole,
        "a string-prefix match that stops mid-component is not a parent directory"
    );
}

// Roles govern the room's content, not the filesystem, devices, or
// network — reaching those always needs a capability.

#[test]
fn even_an_owner_needs_a_capability_to_touch_a_path() {
    let owner = Actor::person(Some(Role::Owner), "alice");

    assert_eq!(
        decide_capability(
            &owner,
            &ResourceRef::FsPath("/etc/shadow".into()),
            Verb::Read,
            &[],
            None
        ),
        CapDecision::DeniedByRole,
        "role authority is standing in a room, not authority over the machine"
    );

    assert_eq!(
        decide_capability(
            &owner,
            &ResourceRef::Device("camera".into()),
            Verb::Read,
            &[],
            None
        ),
        CapDecision::DeniedByRole,
        "devices are outside the room and are capability-only"
    );

    assert_eq!(
        decide_capability(
            &owner,
            &ResourceRef::NetHost("evil.example".into()),
            Verb::Read,
            &[],
            None
        ),
        CapDecision::DeniedByRole,
        "network hosts are outside the room and are capability-only"
    );

    // ... but the same Owner governs the room itself with no explicit
    // capability at all.
    assert_eq!(
        decide_capability(&owner, &ResourceRef::Workspace, Verb::Admin, &[], None),
        CapDecision::Allow,
        "an Owner governs their own room by role"
    );
}

/// `Scope::Workspace` really does mean *everything*, including paths and
/// devices — unlike a role, which stops at the room's own content.
///
/// A sharp edge, pinned deliberately: right semantics for "this local
/// user is unrestricted" (`ui::PermissionContext::permissive`), wrong to
/// hand a remote peer — a workspace-wide grant to Bob would give Bob
/// every path on the machine. The compositor never issues one to a peer;
/// room membership is `Scope::Room`, which does **not** reach paths
/// (asserted below).
#[test]
fn a_workspace_wide_grant_reaches_everything_but_a_room_grant_does_not() {
    let path = ResourceRef::FsPath("/etc/shadow".into());

    let workspace_wide = vec![CapOverride::grant(
        Subject::person("bob"),
        Scope::Workspace,
        Verb::Admin,
    )];
    assert_eq!(
        decide_capability(
            &Actor::person(None, "bob"),
            &path,
            Verb::Write,
            &workspace_wide,
            None
        ),
        CapDecision::Allow,
        "Scope::Workspace matches every resource, paths included — this is what \
         makes it usable as 'unrestricted local user' and unsafe to hand a peer"
    );

    let room_wide = vec![CapOverride::membership(
        Subject::person("bob"),
        "ws-design",
        Verb::Admin,
    )];
    assert_eq!(
        decide_capability(
            &Actor::person(None, "bob").in_room("ws-design"),
            &path,
            Verb::Write,
            &room_wide,
            None
        ),
        CapDecision::DeniedByRole,
        "room membership — even as an Admin — must NOT reach the filesystem; this is \
         the shape the compositor actually writes for a peer"
    );
    assert_eq!(
        decide_capability(
            &Actor::person(None, "bob").in_room("ws-design"),
            &ResourceRef::Device("camera".into()),
            Verb::Read,
            &room_wide,
            None
        ),
        CapDecision::DeniedByRole,
        "nor the camera"
    );
}

#[test]
fn role_base_applies_exactly_to_in_room_resources() {
    assert!(role_base_applies(&ResourceRef::Workspace));
    assert!(role_base_applies(&ResourceRef::App("a".into())));
    assert!(role_base_applies(&artifact_of("a", ArtifactId::new())));
    assert!(role_base_applies(&ResourceRef::Action {
        app: "a".into(),
        action: "turn-page".into()
    }));
    assert!(role_base_applies(&ResourceRef::Room("r".into())));

    assert!(!role_base_applies(&ResourceRef::FsPath("/".into())));
    assert!(!role_base_applies(&ResourceRef::Device("camera".into())));
    assert!(!role_base_applies(&ResourceRef::NetHost("*".into())));
}

// A peer without the verb is denied, and the denial names its reason.

#[test]
fn a_viewer_cannot_write_and_the_denial_is_attributable() {
    let viewer = Actor::person(Some(Role::Viewer), "bob");
    let doc = artifact_of("org.qos.Notes", ArtifactId::new());

    assert_eq!(
        decide_capability(&viewer, &doc, Verb::Read, &[], None),
        CapDecision::Allow,
        "showing a resource is not an action"
    );
    assert_eq!(
        decide_capability(&viewer, &doc, Verb::Write, &[], None),
        CapDecision::DeniedByRole,
        "a Viewer holds no Write and no grant extends it"
    );
    assert_eq!(
        decide_capability(&viewer, &doc, Verb::Comment, &[], None),
        CapDecision::DeniedByRole,
    );
}

#[test]
fn a_restriction_denies_an_editor_and_reports_a_different_reason() {
    let editor = Actor::person(Some(Role::Editor), "bob");
    let id = ArtifactId::new();
    let doc = artifact_of("finance", id);

    let caps = vec![CapOverride::restrict_role_in_app(
        Role::Editor,
        "finance",
        Verb::Comment,
    )];

    assert_eq!(
        decide_capability(&editor, &doc, Verb::Write, &caps, None),
        CapDecision::DeniedByRestriction,
        "the Editor would have held Write; the app-scoped Restrict removed it"
    );
    // The two denials are distinguishable, which is what lets the UI
    // say *why* an affordance is missing.
    assert_ne!(CapDecision::DeniedByRestriction, CapDecision::DeniedByRole);

    assert_eq!(
        decide_capability(&editor, &doc, Verb::Comment, &caps, None),
        CapDecision::Allow,
        "the restriction caps authority at Comment, it does not erase it"
    );
}

#[test]
fn a_restriction_beats_a_grant() {
    let bob = key(7);
    let actor = Actor::signed(Role::Viewer, bob);
    let id = ArtifactId::new();
    let doc = artifact_of("finance", id);

    let caps = vec![
        CapOverride::share(bob, id, Verb::Write),
        CapOverride::new(
            Subject::User(bob),
            Scope::App("finance".into()),
            Effect::Restrict,
            Verb::Read,
        ),
    ];

    assert_eq!(
        decide_capability(&actor, &doc, Verb::Write, &caps, None),
        CapDecision::DeniedByRestriction,
        "deny-biased: a matching Restrict removes authority a Grant offered"
    );
}

#[test]
fn a_denied_role_cannot_be_rescued_by_any_capability() {
    let blocked = Actor::person(Some(Role::Denied), "mallory");
    let caps = vec![CapOverride::grant(
        Subject::person("mallory"),
        Scope::Workspace,
        Verb::Admin,
    )];

    assert_eq!(
        decide_capability(&blocked, &ResourceRef::Workspace, Verb::Read, &caps, None),
        CapDecision::DeniedByRole,
        "Denied is an explicit block, not merely an absence of authority"
    );
}

// Expiry: a capability past its expiry is not "restricted" — it is
// simply not there.

#[test]
fn an_expired_capability_stops_working() {
    let app = Actor::app("org.qos.Ebook");
    let path = ResourceRef::FsPath("/var/workspace/books/dune.epub".into());
    let caps = vec![
        CapOverride::grant(
            Subject::app("org.qos.Ebook"),
            Scope::FsPath("/var/workspace/books".into()),
            Verb::Write,
        )
        .with_expiry(1_000),
    ];

    assert_eq!(
        decide_capability(&app, &path, Verb::Write, &caps, Some(999)),
        CapDecision::Allow,
        "live one second before expiry"
    );
    assert_eq!(
        decide_capability(&app, &path, Verb::Write, &caps, Some(1_000)),
        CapDecision::DeniedByRole,
        "expiry is exclusive: at the expiry instant the capability is gone"
    );
    assert_eq!(
        decide_capability(&app, &path, Verb::Write, &caps, Some(5_000)),
        CapDecision::DeniedByRole,
        "and stays gone"
    );
}

#[test]
fn an_expired_restriction_also_lapses() {
    let editor = Actor::person(Some(Role::Editor), "bob");
    let doc = artifact_of("finance", ArtifactId::new());
    let caps = vec![
        CapOverride::restrict_role_in_app(Role::Editor, "finance", Verb::Read).with_expiry(50),
    ];

    assert_eq!(
        decide_capability(&editor, &doc, Verb::Write, &caps, Some(10)),
        CapDecision::DeniedByRestriction
    );
    assert_eq!(
        decide_capability(&editor, &doc, Verb::Write, &caps, Some(51)),
        CapDecision::Allow,
        "a lapsed restriction stops biting, symmetrically with a lapsed grant"
    );
}

// Nesting: scopes contain each other; a capability on a container
// reaches its contents and nothing else.

#[test]
fn an_app_scope_reaches_that_apps_artifacts_and_actions_only() {
    let bob = Actor::person(Some(Role::Viewer), "bob");
    let caps = vec![CapOverride::grant(
        Subject::person("bob"),
        Scope::App("org.qos.Ebook".into()),
        Verb::Write,
    )];

    assert_eq!(
        decide_capability(
            &bob,
            &artifact_of("org.qos.Ebook", ArtifactId::new()),
            Verb::Write,
            &caps,
            None
        ),
        CapDecision::Allow,
    );
    assert_eq!(
        decide_capability(
            &bob,
            &ResourceRef::Action {
                app: "org.qos.Ebook".into(),
                action: "annotate".into()
            },
            Verb::Write,
            &caps,
            None
        ),
        CapDecision::Allow,
        "actions nest under their app"
    );
    assert_eq!(
        decide_capability(
            &bob,
            &artifact_of("org.qos.Notes", ArtifactId::new()),
            Verb::Write,
            &caps,
            None
        ),
        CapDecision::DeniedByRole,
        "a capability on one app must not reach another app's artifacts"
    );
}

#[test]
fn an_instance_share_does_not_reach_a_sibling_instance() {
    let shared = ArtifactId::new();
    let other = ArtifactId::new();
    let bob = key(9);
    let actor = Actor::signed(Role::Viewer, bob);
    let caps = vec![CapOverride::share(bob, shared, Verb::Write)];

    assert_eq!(
        decide_capability(
            &actor,
            &artifact_of("notes", shared),
            Verb::Write,
            &caps,
            None
        ),
        CapDecision::Allow
    );
    assert_eq!(
        decide_capability(
            &actor,
            &artifact_of("notes", other),
            Verb::Write,
            &caps,
            None
        ),
        CapDecision::DeniedByRole,
        "sharing one document must not share the next one"
    );
}

#[test]
fn a_network_capability_is_not_a_blanket_one() {
    let app = Actor::app("org.qos.Browser");
    let caps = vec![CapOverride::grant(
        Subject::app("org.qos.Browser"),
        Scope::NetHost(".example.com".into()),
        Verb::Write,
    )];

    for host in ["example.com", "api.example.com", "a.b.example.com"] {
        assert_eq!(
            decide_capability(
                &app,
                &ResourceRef::NetHost(host.into()),
                Verb::Write,
                &caps,
                None
            ),
            CapDecision::Allow,
            "{host} is in the granted domain"
        );
    }
    for host in ["evil.com", "example.com.evil.com", "notexample.com"] {
        assert_eq!(
            decide_capability(
                &app,
                &ResourceRef::NetHost(host.into()),
                Verb::Write,
                &caps,
                None
            ),
            CapDecision::DeniedByRole,
            "{host} is outside the granted domain and must be denied"
        );
    }
}

// Subjects do not bleed into one another.

#[test]
fn a_capability_granted_to_one_app_does_not_serve_another() {
    let caps = vec![CapOverride::grant(
        Subject::app("org.qos.Ebook"),
        Scope::FsPath("/var/workspace/books".into()),
        Verb::Read,
    )];
    let path = ResourceRef::FsPath("/var/workspace/books/dune.epub".into());

    assert_eq!(
        decide_capability(&Actor::app("org.qos.Ebook"), &path, Verb::Read, &caps, None),
        CapDecision::Allow
    );
    assert_eq!(
        decide_capability(&Actor::app("org.qos.Files"), &path, Verb::Read, &caps, None),
        CapDecision::DeniedByRole,
        "the Files app was not the subject of that grant"
    );
    assert_eq!(
        decide_capability(&Actor::agent("assistant"), &path, Verb::Read, &caps, None),
        CapDecision::DeniedByRole,
        "an agent is its own subject, not a stand-in for any app"
    );
}

#[test]
fn a_role_wide_grant_never_covers_an_app_or_an_agent() {
    // "Every Editor may write in this room" must not hand authority to
    // software running inside the room.
    let caps = vec![CapOverride::grant(
        Subject::Role(Role::Editor),
        Scope::Workspace,
        Verb::Admin,
    )];
    let doc = artifact_of("notes", ArtifactId::new());

    assert_eq!(
        decide_capability(
            &Actor::person(Some(Role::Editor), "bob"),
            &doc,
            Verb::Admin,
            &caps,
            None
        ),
        CapDecision::Allow,
        "a person holding the role is covered"
    );
    assert_eq!(
        decide_capability(&Actor::app("org.qos.Ebook"), &doc, Verb::Admin, &caps, None),
        CapDecision::DeniedByRole,
        "an app holds no role, so a role-wide grant is inert for it"
    );
    assert_eq!(
        decide_capability(&Actor::agent("assistant"), &doc, Verb::Admin, &caps, None),
        CapDecision::DeniedByRole,
        "same for an agent"
    );
}

#[test]
fn a_bearer_token_holder_gets_exactly_the_tokens_authority() {
    let link = CapOverride::grant(
        Subject::token("inv-7f3a"),
        Scope::FsPath("/var/workspace/shared/report.md".into()),
        Verb::Read,
    );
    let caps = vec![link];
    let path = ResourceRef::FsPath("/var/workspace/shared/report.md".into());

    let holder = Actor::default().with_principal(Principal::Token("inv-7f3a".into()));
    assert_eq!(
        decide_capability(&holder, &path, Verb::Read, &caps, None),
        CapDecision::Allow
    );
    assert_eq!(
        decide_capability(&holder, &path, Verb::Write, &caps, None),
        CapDecision::DeniedByRole,
        "a read link is not a write link"
    );

    let wrong_token = Actor::default().with_principal(Principal::Token("inv-0000".into()));
    assert_eq!(
        decide_capability(&wrong_token, &path, Verb::Read, &caps, None),
        CapDecision::DeniedByRole,
        "presenting a different token grants nothing"
    );

    // A default Actor presents nothing at all — the fail-closed floor.
    assert_eq!(
        decide_capability(&Actor::default(), &path, Verb::Read, &caps, None),
        CapDecision::DeniedByRole,
        "an actor presenting no principal holds nothing"
    );
}

#[test]
fn a_room_subject_covers_everyone_in_that_room_and_no_one_else() {
    let caps = vec![CapOverride::grant(
        Subject::room("ws-design"),
        Scope::FsPath("/var/workspace/design".into()),
        Verb::Write,
    )];
    let path = ResourceRef::FsPath("/var/workspace/design/logo.svg".into());

    let insider = Actor::person(Some(Role::Viewer), "bob").in_room("ws-design");
    let outsider = Actor::person(Some(Role::Owner), "carol").in_room("ws-finance");

    assert_eq!(
        decide_capability(&insider, &path, Verb::Write, &caps, None),
        CapDecision::Allow,
        "membership of the granted room carries the capability"
    );
    assert_eq!(
        decide_capability(&outsider, &path, Verb::Write, &caps, None),
        CapDecision::DeniedByRole,
        "being an Owner of a *different* room grants nothing here"
    );
}

#[test]
fn a_person_named_by_key_and_by_hex_id_are_the_same_subject() {
    let k = key(3);
    let hex = Principal::person_from_key(k);
    let Principal::Person(hex_id) = hex.clone() else {
        panic!("person_from_key must produce a Person principal");
    };
    assert_eq!(hex_id.len(), 64, "32 bytes as lowercase hex");

    let path = ResourceRef::FsPath("/var/workspace/a.txt".into());
    let by_key = vec![CapOverride::grant(
        Subject::User(k),
        Scope::FsPath("/var/workspace".into()),
        Verb::Read,
    )];
    let by_id = vec![CapOverride::grant(
        Subject::Principal(hex),
        Scope::FsPath("/var/workspace".into()),
        Verb::Read,
    )];

    // Named by key, matched by an actor carrying only the hex id.
    let id_actor = Actor::person(None, hex_id);
    assert_eq!(
        decide_capability(&id_actor, &path, Verb::Read, &by_key, None),
        CapDecision::Allow
    );
    // Named by hex id, matched by an actor carrying only the key.
    let key_actor = Actor::signed(Role::Viewer, k);
    assert_eq!(
        decide_capability(&key_actor, &path, Verb::Read, &by_id, None),
        CapDecision::Allow
    );

    // A different key is a different person, both ways round.
    let other = Actor::signed(Role::Viewer, key(4));
    assert_eq!(
        decide_capability(&other, &path, Verb::Read, &by_key, None),
        CapDecision::DeniedByRole
    );
    assert_eq!(
        decide_capability(&other, &path, Verb::Read, &by_id, None),
        CapDecision::DeniedByRole
    );
}

// The hard UI constraint, enforced on the data rather than trusted
// to the renderer: ad-hoc sharing is never room membership.

#[test]
fn sharing_one_file_with_a_person_is_not_room_membership() {
    let share = CapOverride::grant(
        Subject::person("bob"),
        Scope::FsPath("/var/workspace/design/logo.svg".into()),
        Verb::Read,
    );
    let membership = CapOverride::membership(Subject::person("carol"), "ws-design", Verb::Write);

    assert!(share.subject_is_person());
    assert!(membership.subject_is_person());

    assert!(
        !share.is_room_membership(),
        "a per-file share must never be classifiable as membership — this is \
         what stops Bob appearing in the roster as a collaborator"
    );
    assert!(
        membership.is_room_membership(),
        "room membership is an ordinary capability whose resource is the room"
    );

    // Sharing one artifact instance is likewise not membership.
    let doc_share = CapOverride::share(key(1), ArtifactId::new(), Verb::Comment);
    assert!(!doc_share.is_room_membership());
}

#[test]
fn a_role_wide_capability_is_never_a_person_share() {
    let role_cap = CapOverride::grant(Subject::Role(Role::Editor), Scope::Workspace, Verb::Write);
    assert!(
        !role_cap.subject_is_person(),
        "a role bundle is not a named person and must not render as one on a share list"
    );

    let app_cap = CapOverride::grant(
        Subject::app("org.qos.Ebook"),
        Scope::FsPath("/var/workspace/books".into()),
        Verb::Read,
    );
    assert!(
        !app_cap.subject_is_person(),
        "an app grant is not a person share"
    );
    assert!(!app_cap.is_room_membership());
}

// Serialisation: capabilities persist and replicate, so their wire
// form has to round-trip and tolerate records written before expiry existed.

#[test]
fn a_capability_round_trips_through_json() {
    let cap = CapOverride::grant(
        Subject::person("bob"),
        Scope::FsPath("/var/workspace/design".into()),
        Verb::Write,
    )
    .with_id("cap-1")
    .with_expiry(1_900_000_000)
    .granted_by(Principal::Person("alice".into()));

    let json = serde_json::to_string(&cap).expect("serialise");
    let back: CapOverride = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(cap, back);
}

#[test]
fn a_capability_written_before_expiry_existed_still_loads() {
    let legacy =
        r#"{"subject":{"Role":"Editor"},"scope":"Workspace","effect":"Grant","verb":"Write"}"#;
    let cap: CapOverride = serde_json::from_str(legacy).expect("legacy record must still load");
    assert_eq!(cap.expiry, Expiry::Never, "no expiry means never expires");
    assert_eq!(cap.id, "");
    assert_eq!(cap.granted_by, None);
    assert!(cap.is_live_at(Some(u64::MAX)));
}
