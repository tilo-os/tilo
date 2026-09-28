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
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use collab_types::caps::{
    Actor, CapDecision, CapOverride, Effect, Principal, ResourceRef, Scope, Subject, Verb,
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

fn fs(path: &str) -> ResourceRef {
    ResourceRef::FsPath(path.into())
}

/// `decide_capability` with no clock — every case but the expiry ones.
fn decide(actor: &Actor, resource: &ResourceRef, verb: Verb, caps: &[CapOverride]) -> CapDecision {
    decide_capability(actor, resource, verb, caps, None)
}

// Apps hold no role, so they can only ever act through a capability
// — what makes the portal prompt the one user-facing dialog. Roles govern
// the room's content, not the filesystem, devices, or network — reaching
// those always needs a capability.

/// An app holds no role: without a capability it is denied everything, and a
/// path or host grant reaches exactly its subtree or domain.
#[test]
fn an_app_reaches_only_the_paths_and_hosts_it_was_granted() {
    // Case: an app with no capability is denied everything.
    let app = Actor::app("org.tilo.Ebook");
    let doc = artifact_of("org.tilo.Notes", ArtifactId::new());
    for verb in [Verb::Read, Verb::Comment, Verb::Write, Verb::Admin] {
        assert_eq!(
            decide(&app, &doc, verb, &[]),
            CapDecision::DeniedByRole,
            "an app with no capability must be denied {verb:?}; it holds no role \
             and therefore has no base authority to fall back on"
        );
    }

    // Case: an app granted one path cannot reach a sibling path.
    let caps = vec![CapOverride::grant(
        Subject::app("org.tilo.Ebook"),
        Scope::FsPath("/var/workspace/books".into()),
        Verb::Write,
    )];
    assert_eq!(
        decide(
            &app,
            &fs("/var/workspace/books/dune.epub"),
            Verb::Write,
            &caps
        ),
        CapDecision::Allow,
        "the granted subtree is reachable"
    );
    // The whole point of the portal prompt: reaching outside what the
    // app owns is denied until the user says otherwise.
    assert_eq!(
        decide(
            &app,
            &fs("/var/workspace/finances/tax.ods"),
            Verb::Read,
            &caps
        ),
        CapDecision::DeniedByRole,
        "a sibling directory the app was never granted must be denied"
    );

    // Case: a path grant does not leak across a component boundary.
    let caps = vec![CapOverride::grant(
        Subject::app("org.tilo.Ebook"),
        Scope::FsPath("/var/workspace/docs".into()),
        Verb::Write,
    )];
    assert_eq!(
        decide(
            &app,
            &fs("/var/workspace/docs/a/b/c.txt"),
            Verb::Write,
            &caps
        ),
        CapDecision::Allow,
        "grants inherit down the tree"
    );
    // `/var/workspace/docsSECRET` shares a string prefix with the
    // granted path but is not underneath it.
    assert_eq!(
        decide(
            &app,
            &fs("/var/workspace/docsSECRET/keys"),
            Verb::Read,
            &caps
        ),
        CapDecision::DeniedByRole,
        "a string-prefix match that stops mid-component is not a parent directory"
    );

    // Case: a network capability is not a blanket one.
    let browser = Actor::app("org.tilo.Browser");
    let caps = vec![CapOverride::grant(
        Subject::app("org.tilo.Browser"),
        Scope::NetHost(".example.com".into()),
        Verb::Write,
    )];
    for host in ["example.com", "api.example.com", "a.b.example.com"] {
        assert_eq!(
            decide(
                &browser,
                &ResourceRef::NetHost(host.into()),
                Verb::Write,
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
                Verb::Write,
                &caps
            ),
            CapDecision::DeniedByRole,
            "{host} is outside the granted domain and must be denied"
        );
    }
}

/// Role authority is standing in a room: it stops at the room boundary
/// (paths, devices, hosts), as does room membership, while a workspace-wide
/// grant reaches everything.
#[test]
fn role_authority_stops_at_the_room_boundary() {
    // Case: even an owner needs a capability to touch a path, device or host.
    let owner = Actor::person(Some(Role::Owner), "alice");
    for (resource, why) in [
        (
            fs("/etc/shadow"),
            "role authority is standing in a room, not authority over the machine",
        ),
        (
            ResourceRef::Device("camera".into()),
            "devices are outside the room and are capability-only",
        ),
        (
            ResourceRef::NetHost("evil.example".into()),
            "network hosts are outside the room and are capability-only",
        ),
    ] {
        assert_eq!(
            decide(&owner, &resource, Verb::Read, &[]),
            CapDecision::DeniedByRole,
            "{why}"
        );
    }
    // ... but the same Owner governs the room itself with no explicit
    // capability at all.
    assert_eq!(
        decide(&owner, &ResourceRef::Workspace, Verb::Admin, &[]),
        CapDecision::Allow,
        "an Owner governs their own room by role"
    );

    // Case: a workspace-wide grant reaches everything but a room grant does
    // not. `Scope::Workspace` really does mean *everything*, including paths
    // and devices — unlike a role, which stops at the room's own content.
    //
    // A sharp edge, pinned deliberately: right semantics for "this local
    // user is unrestricted" (`ui::PermissionContext::permissive`), wrong to
    // hand a remote peer — a workspace-wide grant to Bob would give Bob
    // every path on the machine. The compositor never issues one to a peer;
    // room membership is `Scope::Room`, which does **not** reach paths.
    let path = fs("/etc/shadow");
    let workspace_wide = vec![CapOverride::grant(
        Subject::person("bob"),
        Scope::Workspace,
        Verb::Admin,
    )];
    assert_eq!(
        decide(
            &Actor::person(None, "bob"),
            &path,
            Verb::Write,
            &workspace_wide
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
    let member = Actor::person(None, "bob").in_room("ws-design");
    assert_eq!(
        decide(&member, &path, Verb::Write, &room_wide),
        CapDecision::DeniedByRole,
        "room membership — even as an Admin — must NOT reach the filesystem; this is \
         the shape the compositor actually writes for a peer"
    );
    assert_eq!(
        decide(
            &member,
            &ResourceRef::Device("camera".into()),
            Verb::Read,
            &room_wide
        ),
        CapDecision::DeniedByRole,
        "nor the camera"
    );

    // Case: role base applies exactly to in-room resources.
    for (resource, applies) in [
        (ResourceRef::Workspace, true),
        (ResourceRef::App("a".into()), true),
        (artifact_of("a", ArtifactId::new()), true),
        (
            ResourceRef::Action {
                app: "a".into(),
                action: "turn-page".into(),
            },
            true,
        ),
        (ResourceRef::Room("r".into()), true),
        (fs("/"), false),
        (ResourceRef::Device("camera".into()), false),
        (ResourceRef::NetHost("*".into()), false),
    ] {
        assert_eq!(role_base_applies(&resource), applies, "case {resource:?}");
    }
}

// A peer without the verb is denied, and the denial names its reason.

/// Inside the room a role is the base authority: a missing verb is denied by
/// role, a restriction caps it (and beats a grant) with a distinguishable
/// reason, and `Denied` cannot be rescued.
#[test]
fn role_authority_is_capped_by_restrictions_and_denied_is_final() {
    // Case: a viewer cannot write and the denial is attributable.
    let viewer = Actor::person(Some(Role::Viewer), "bob");
    let doc = artifact_of("org.tilo.Notes", ArtifactId::new());
    assert_eq!(
        decide(&viewer, &doc, Verb::Read, &[]),
        CapDecision::Allow,
        "showing a resource is not an action"
    );
    assert_eq!(
        decide(&viewer, &doc, Verb::Write, &[]),
        CapDecision::DeniedByRole,
        "a Viewer holds no Write and no grant extends it"
    );
    assert_eq!(
        decide(&viewer, &doc, Verb::Comment, &[]),
        CapDecision::DeniedByRole,
    );

    // Case: a restriction denies an editor and reports a different reason.
    let editor = Actor::person(Some(Role::Editor), "bob");
    let finance_doc = artifact_of("finance", ArtifactId::new());
    let caps = vec![CapOverride::restrict_role_in_app(
        Role::Editor,
        "finance",
        Verb::Comment,
    )];
    assert_eq!(
        decide(&editor, &finance_doc, Verb::Write, &caps),
        CapDecision::DeniedByRestriction,
        "the Editor would have held Write; the app-scoped Restrict removed it"
    );
    // The two denials are distinguishable, which is what lets the UI
    // say *why* an affordance is missing.
    assert_ne!(CapDecision::DeniedByRestriction, CapDecision::DeniedByRole);
    assert_eq!(
        decide(&editor, &finance_doc, Verb::Comment, &caps),
        CapDecision::Allow,
        "the restriction caps authority at Comment, it does not erase it"
    );

    // Case: a restriction beats a grant.
    let bob = key(7);
    let actor = Actor::signed(Role::Viewer, bob);
    let id = ArtifactId::new();
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
        decide(&actor, &artifact_of("finance", id), Verb::Write, &caps),
        CapDecision::DeniedByRestriction,
        "deny-biased: a matching Restrict removes authority a Grant offered"
    );

    // Case: a denied role cannot be rescued by any capability.
    let blocked = Actor::person(Some(Role::Denied), "mallory");
    let caps = vec![CapOverride::grant(
        Subject::person("mallory"),
        Scope::Workspace,
        Verb::Admin,
    )];
    assert_eq!(
        decide(&blocked, &ResourceRef::Workspace, Verb::Read, &caps),
        CapDecision::DeniedByRole,
        "Denied is an explicit block, not merely an absence of authority"
    );
}

/// Expiry: a capability past its expiry is not "restricted" — it is simply
/// not there. Grants and restrictions lapse symmetrically.
#[test]
fn grants_and_restrictions_lapse_at_their_expiry() {
    // Case: an expired capability stops working.
    let app = Actor::app("org.tilo.Ebook");
    let path = fs("/var/workspace/books/dune.epub");
    let caps = vec![
        CapOverride::grant(
            Subject::app("org.tilo.Ebook"),
            Scope::FsPath("/var/workspace/books".into()),
            Verb::Write,
        )
        .with_expiry(1_000),
    ];
    for (now, want, why) in [
        (999, CapDecision::Allow, "live one second before expiry"),
        (
            1_000,
            CapDecision::DeniedByRole,
            "expiry is exclusive: at the expiry instant the capability is gone",
        ),
        (5_000, CapDecision::DeniedByRole, "and stays gone"),
    ] {
        assert_eq!(
            decide_capability(&app, &path, Verb::Write, &caps, Some(now)),
            want,
            "{why}"
        );
    }

    // Case: an expired restriction also lapses.
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

// Nesting: scopes contain each other; a capability on a container reaches
// its contents and nothing else.

/// A capability on a container reaches its contents and nothing else.
#[test]
fn a_scope_reaches_its_contents_and_nothing_else() {
    // Case: an app scope reaches that app's artifacts and actions only.
    let bob = Actor::person(Some(Role::Viewer), "bob");
    let caps = vec![CapOverride::grant(
        Subject::person("bob"),
        Scope::App("org.tilo.Ebook".into()),
        Verb::Write,
    )];
    assert_eq!(
        decide(
            &bob,
            &artifact_of("org.tilo.Ebook", ArtifactId::new()),
            Verb::Write,
            &caps
        ),
        CapDecision::Allow,
    );
    assert_eq!(
        decide(
            &bob,
            &ResourceRef::Action {
                app: "org.tilo.Ebook".into(),
                action: "annotate".into()
            },
            Verb::Write,
            &caps
        ),
        CapDecision::Allow,
        "actions nest under their app"
    );
    assert_eq!(
        decide(
            &bob,
            &artifact_of("org.tilo.Notes", ArtifactId::new()),
            Verb::Write,
            &caps
        ),
        CapDecision::DeniedByRole,
        "a capability on one app must not reach another app's artifacts"
    );

    // Case: an instance share does not reach a sibling instance.
    let shared = ArtifactId::new();
    let other = ArtifactId::new();
    let bob_key = key(9);
    let actor = Actor::signed(Role::Viewer, bob_key);
    let caps = vec![CapOverride::share(bob_key, shared, Verb::Write)];
    assert_eq!(
        decide(&actor, &artifact_of("notes", shared), Verb::Write, &caps),
        CapDecision::Allow
    );
    assert_eq!(
        decide(&actor, &artifact_of("notes", other), Verb::Write, &caps),
        CapDecision::DeniedByRole,
        "sharing one document must not share the next one"
    );
}

// Subjects do not bleed into one another.

/// A capability serves exactly its subject: one app, a role's people (never
/// its apps or agents), a bearer token, a room's members, or one person
/// however they are named.
#[test]
fn a_capability_serves_only_its_subject() {
    // Case: a capability granted to one app does not serve another.
    let caps = vec![CapOverride::grant(
        Subject::app("org.tilo.Ebook"),
        Scope::FsPath("/var/workspace/books".into()),
        Verb::Read,
    )];
    let path = fs("/var/workspace/books/dune.epub");
    assert_eq!(
        decide(&Actor::app("org.tilo.Ebook"), &path, Verb::Read, &caps),
        CapDecision::Allow
    );
    assert_eq!(
        decide(&Actor::app("org.tilo.Files"), &path, Verb::Read, &caps),
        CapDecision::DeniedByRole,
        "the Files app was not the subject of that grant"
    );
    assert_eq!(
        decide(&Actor::agent("assistant"), &path, Verb::Read, &caps),
        CapDecision::DeniedByRole,
        "an agent is its own subject, not a stand-in for any app"
    );

    // Case: a role-wide grant never covers an app or an agent. "Every Editor
    // may write in this room" must not hand authority to software running
    // inside the room.
    let caps = vec![CapOverride::grant(
        Subject::Role(Role::Editor),
        Scope::Workspace,
        Verb::Admin,
    )];
    let doc = artifact_of("notes", ArtifactId::new());
    for (actor, want, why) in [
        (
            Actor::person(Some(Role::Editor), "bob"),
            CapDecision::Allow,
            "a person holding the role is covered",
        ),
        (
            Actor::app("org.tilo.Ebook"),
            CapDecision::DeniedByRole,
            "an app holds no role, so a role-wide grant is inert for it",
        ),
        (
            Actor::agent("assistant"),
            CapDecision::DeniedByRole,
            "same for an agent",
        ),
    ] {
        assert_eq!(decide(&actor, &doc, Verb::Admin, &caps), want, "{why}");
    }

    // Case: a bearer-token holder gets exactly the token's authority.
    let caps = vec![CapOverride::grant(
        Subject::token("inv-7f3a"),
        Scope::FsPath("/var/workspace/shared/report.md".into()),
        Verb::Read,
    )];
    let path = fs("/var/workspace/shared/report.md");
    let holder = Actor::default().with_principal(Principal::Token("inv-7f3a".into()));
    assert_eq!(
        decide(&holder, &path, Verb::Read, &caps),
        CapDecision::Allow
    );
    assert_eq!(
        decide(&holder, &path, Verb::Write, &caps),
        CapDecision::DeniedByRole,
        "a read link is not a write link"
    );
    let wrong_token = Actor::default().with_principal(Principal::Token("inv-0000".into()));
    assert_eq!(
        decide(&wrong_token, &path, Verb::Read, &caps),
        CapDecision::DeniedByRole,
        "presenting a different token grants nothing"
    );
    // A default Actor presents nothing at all — the fail-closed floor.
    assert_eq!(
        decide(&Actor::default(), &path, Verb::Read, &caps),
        CapDecision::DeniedByRole,
        "an actor presenting no principal holds nothing"
    );
}

/// A room subject covers exactly that room's members, and one person is one
/// subject whether named by key or by hex id.
#[test]
fn rooms_and_people_are_matched_by_identity() {
    // Case: a room subject covers everyone in that room and no one else.
    let caps = vec![CapOverride::grant(
        Subject::room("ws-design"),
        Scope::FsPath("/var/workspace/design".into()),
        Verb::Write,
    )];
    let path = fs("/var/workspace/design/logo.svg");
    let insider = Actor::person(Some(Role::Viewer), "bob").in_room("ws-design");
    let outsider = Actor::person(Some(Role::Owner), "carol").in_room("ws-finance");
    assert_eq!(
        decide(&insider, &path, Verb::Write, &caps),
        CapDecision::Allow,
        "membership of the granted room carries the capability"
    );
    assert_eq!(
        decide(&outsider, &path, Verb::Write, &caps),
        CapDecision::DeniedByRole,
        "being an Owner of a *different* room grants nothing here"
    );

    // Case: a person named by key and by hex id are the same subject.
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
        decide(&id_actor, &path, Verb::Read, &by_key),
        CapDecision::Allow
    );
    // Named by hex id, matched by an actor carrying only the key.
    let key_actor = Actor::signed(Role::Viewer, k);
    assert_eq!(
        decide(&key_actor, &path, Verb::Read, &by_id),
        CapDecision::Allow
    );
    // A different key is a different person, both ways round.
    let other = Actor::signed(Role::Viewer, key(4));
    assert_eq!(
        decide(&other, &path, Verb::Read, &by_key),
        CapDecision::DeniedByRole
    );
    assert_eq!(
        decide(&other, &path, Verb::Read, &by_id),
        CapDecision::DeniedByRole
    );
}

// The hard UI constraint, enforced on the data rather than trusted to the
// renderer: ad-hoc sharing is never room membership.

/// Only a room-scoped capability to a person classifies as membership, and
/// only a person subject classifies as a person share.
#[test]
fn sharing_is_classified_apart_from_membership_and_role_bundles() {
    // Case: sharing one file with a person is not room membership.
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

    // Case: a role-wide capability is never a person share.
    let role_cap = CapOverride::grant(Subject::Role(Role::Editor), Scope::Workspace, Verb::Write);
    assert!(
        !role_cap.subject_is_person(),
        "a role bundle is not a named person and must not render as one on a share list"
    );
    let app_cap = CapOverride::grant(
        Subject::app("org.tilo.Ebook"),
        Scope::FsPath("/var/workspace/books".into()),
        Verb::Read,
    );
    assert!(
        !app_cap.subject_is_person(),
        "an app grant is not a person share"
    );
    assert!(!app_cap.is_room_membership());
}

// Serialisation: capabilities persist and replicate, so their wire form has
// to round-trip.

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
