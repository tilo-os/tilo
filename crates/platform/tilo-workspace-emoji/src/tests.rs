use super::{EmojiImage, MAX_EMOJI, MAX_IMAGE_BYTES, WorkspaceEmoji, workspace_emoji};
use tilo_action_policy::{ActionKind, Answer, Authority, Context, Role, RoomPolicy, roles};
use tilo_resource::{Model as _, Replica, Resource as _};

const ALICE: &str = "a11ce";
const BOB: &str = "b0b";
const CAROL: &str = "ca401";

/// A small PNG: the signature and a few bytes past it are all the record
/// checks.
fn png(fill: u8, len: usize) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.resize(len.max(bytes.len()), fill);
    bytes
}

fn webp() -> Vec<u8> {
    let mut bytes = b"RIFF\0\0\0\0WEBPVP8L".to_vec();
    bytes.resize(64, 1);
    bytes
}

fn add(name: &str, image: &[u8]) -> tilo_resource::Invoke<WorkspaceEmoji> {
    WorkspaceEmoji::ADD_EMOJI.with(workspace_emoji::AddEmoji {
        name: name.into(),
        image: EmojiImage(image.to_vec()).to_base64(),
    })
}

fn remove(name: &str) -> tilo_resource::Invoke<WorkspaceEmoji> {
    WorkspaceEmoji::REMOVE_EMOJI.with(workspace_emoji::RemoveEmoji { name: name.into() })
}

fn remove_any(name: &str) -> tilo_resource::Invoke<WorkspaceEmoji> {
    WorkspaceEmoji::REMOVE_ANY_EMOJI.with(workspace_emoji::RemoveAnyEmoji { name: name.into() })
}

fn as_person(person: &str) -> Replica<WorkspaceEmoji> {
    Replica::<WorkspaceEmoji>::new().as_author(person)
}

#[test]
fn an_emoji_is_added_under_a_free_valid_name_with_who_and_when() {
    let mut record = as_person(ALICE);
    record.set_now_ms(1_700_000_000_000);
    record.apply(add("party_parrot", &png(1, 300))).unwrap();
    record.apply(add("tilo", &webp())).unwrap();
    let state = record.state().unwrap();
    assert_eq!(state.names(), ["party_parrot", "tilo"]);
    let parrot = state.get("party_parrot").unwrap();
    assert_eq!(parrot.added_by, ALICE);
    assert_eq!(parrot.added_at, 1_700_000_000_000);
    assert_eq!(parrot.image.bytes(), png(1, 300));

    for name in [
        "x",
        "has space",
        "too_long_a_name_for_any_emoji_here",
        "dash-ed",
        ":tilo:",
    ] {
        assert!(
            record.apply(add(name, &png(1, 100))).is_err(),
            "{name:?} names no emoji"
        );
    }
    assert!(
        record.apply(add("tilo", &png(2, 100))).is_err(),
        "a name taken stays its emoji's"
    );
    assert_eq!(
        record.state().unwrap().get("tilo").unwrap().image.bytes(),
        webp()
    );
}

#[test]
fn only_small_png_or_webp_pictures_are_kept() {
    let mut record = as_person(ALICE);
    record
        .apply(add("largest", &png(3, MAX_IMAGE_BYTES)))
        .unwrap();
    assert!(
        record
            .apply(add("too_big", &png(3, MAX_IMAGE_BYTES + 1)))
            .is_err()
    );
    assert!(
        record
            .apply(add("a_jpeg", b"\xff\xd8\xff\xe0 not kept"))
            .is_err()
    );
    assert!(record.apply(add("nothing", b"")).is_err());
    let not_base64 = WorkspaceEmoji::ADD_EMOJI.with(workspace_emoji::AddEmoji {
        name: "garbled".into(),
        image: "%%%".into(),
    });
    assert!(record.apply(not_base64).is_err());
    assert_eq!(record.state().unwrap().names(), ["largest"]);
}

/// The largest picture travels inside its action's own change: below the
/// size at which a payload's run is kept apart as the invoking app's
/// content, which another app of the room could not read to replay it.
#[test]
fn the_largest_picture_stays_inside_its_action() {
    let image = EmojiImage(png(4, MAX_IMAGE_BYTES)).to_base64();
    assert!(image.len() < tilo_actions::CONTENT_PART_THRESHOLD);
}

#[test]
fn a_workspace_keeps_at_most_its_limit() {
    let mut record = as_person(ALICE);
    for n in 0..MAX_EMOJI {
        record.apply(add(&format!("e{n}"), &png(1, 16))).unwrap();
    }
    assert!(record.apply(add("one_more", &png(1, 16))).is_err());
    assert_eq!(record.state().unwrap().emoji.len(), MAX_EMOJI);
}

#[test]
fn people_remove_their_own_and_moderators_anyones() {
    let mut alice = as_person(ALICE);
    alice.apply(add("alices", &png(1, 64))).unwrap();
    let mut bob = alice.fork().as_author(BOB);
    bob.apply(add("bobs", &png(2, 64))).unwrap();
    assert!(
        bob.apply(remove("alices")).is_err(),
        "nobody removes another's emoji as their own"
    );
    bob.apply(remove("bobs")).unwrap();
    assert!(bob.apply(remove("bobs")).is_err(), "it is gone");
    bob.apply(remove_any("alices")).unwrap();
    assert!(bob.state().unwrap().emoji.is_empty());
}

#[test]
fn removing_anyones_emoji_sits_under_the_moderation_node() {
    let field = |id: &str| {
        WorkspaceEmoji::declarations()
            .into_iter()
            .find(|decl| decl.id == id)
            .map(|decl| (decl.field, decl.kind))
    };
    assert_eq!(
        field("remove-any-emoji"),
        Some((Some(tilo_actions::MODERATION_FIELD), ActionKind::Undoable))
    );
    assert_eq!(field("add-emoji"), Some((None, ActionKind::Undoable)));
    assert_eq!(field("remove-emoji"), Some((None, ActionKind::Undoable)));
}

/// The room's roles decide them as they decide any app's actions: Editors
/// add and remove their own, only Moderators and the owner remove anyone's,
/// and someone who may only view or comment adds none.
#[test]
fn the_rooms_roles_decide_who_adds_and_who_moderates() {
    let mut room = RoomPolicy::new("r", ALICE);
    for (person, role) in [
        (BOB, Role::Editor),
        (CAROL, Role::Moderator),
        ("v", Role::Viewer),
        ("c", Role::Commenter),
    ] {
        room.members.insert(person.to_owned(), role);
    }
    let node = |id: &str| {
        let declared = WorkspaceEmoji::declarations()
            .into_iter()
            .find(|decl| decl.id == id)
            .unwrap();
        tilo_actions::action_node(
            &tilo_action_policy::TreePath::room("r"),
            super::OWNER,
            WorkspaceEmoji::KIND,
            "workspace-emoji",
            declared.field,
            id,
        )
    };
    let answer = |person: &str, id: &str| {
        Authority::person(room.party(person))
            .decide(&node(id), ActionKind::Undoable, &Context::at(1))
            .answer
    };
    for person in [ALICE, BOB, CAROL] {
        assert_eq!(answer(person, "add-emoji"), Answer::Allow, "{person}");
        assert_eq!(answer(person, "remove-emoji"), Answer::Allow, "{person}");
    }
    for person in ["v", "c"] {
        assert_eq!(answer(person, "add-emoji"), Answer::Deny, "{person}");
    }
    assert_eq!(answer(ALICE, "remove-any-emoji"), Answer::Allow);
    assert_eq!(answer(CAROL, "remove-any-emoji"), Answer::Allow);
    assert_eq!(answer(BOB, "remove-any-emoji"), Answer::Deny);
    let _ = roles::MODERATION_FIELD;
}

#[test]
fn concurrent_additions_converge() {
    let mut alice = as_person(ALICE);
    alice.apply(add("shared", &png(1, 32))).unwrap();
    let mut bob = alice.fork().as_author(BOB);
    alice.apply(add("from_alice", &png(2, 32))).unwrap();
    bob.apply(add("from_bob", &png(3, 32))).unwrap();
    // The same name added on both sides at once: one picture wins
    // everywhere.
    alice.apply(add("same", &png(4, 32))).unwrap();
    bob.apply(add("same", &png(5, 32))).unwrap();
    bob.apply(remove_any("shared")).unwrap();
    alice.sync_with(&mut bob).unwrap();
    let (a, b) = (alice.state().unwrap(), bob.state().unwrap());
    assert_eq!(a, b);
    assert_eq!(a.names(), ["from_alice", "from_bob", "same"]);
    assert!(
        a.get("same").unwrap().image.bytes() == png(4, 32)
            || a.get("same").unwrap().image.bytes() == png(5, 32)
    );
    assert!(a.valid().is_ok());
}

#[test]
fn a_stored_picture_reads_back_as_bytes_and_base64() {
    let image = EmojiImage(png(9, 40));
    assert_eq!(EmojiImage::from_json(&image.to_json()).unwrap(), image);
    let mut record = as_person(ALICE);
    record.apply(add("kept", image.bytes())).unwrap();
    let saved = record.save();
    let loaded = Replica::<WorkspaceEmoji>::load(&saved).unwrap();
    assert_eq!(loaded.state().unwrap().get("kept").unwrap().image, image);
}

#[test]
fn a_free_name_is_made_from_a_files_name() {
    let mut record = as_person(ALICE);
    record.apply(add("party_parrot", &png(1, 16))).unwrap();
    record.apply(add("party_parrot_2", &png(1, 16))).unwrap();
    let state = record.state().unwrap();
    assert_eq!(
        state.free_name("Party Parrot").as_deref(),
        Some("party_parrot_3")
    );
    assert_eq!(
        state.free_name("cat-wave (1)").as_deref(),
        Some("cat_wave_1")
    );
    assert_eq!(state.free_name("x").as_deref(), Some("emoji_x"));
    assert_eq!(state.free_name("日本").as_deref(), Some("emoji"));
    let long = state.free_name(&"a".repeat(80)).unwrap();
    assert_eq!(long.len(), 32);
    assert!(tilo_resource::valid_custom_emoji_name(&long));
}
