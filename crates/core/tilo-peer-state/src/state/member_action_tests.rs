//! A member's device runs an action exactly where its role lets it: a commenter's verb under the
//! `comments` is theirs, the same kind at the record is not, and a viewer's is refused. The
//! decision is the node's, so an action its app declares under a field is judged there.
use super::*;

const DISCUSSION: &str = "so.tilo.lang.discussion";
const COMMENTS: &str = "tl:so.tilo.lang.discussion";
const TASKS: &str = "so.tilo.tasks";

/// What a call of an undoable action at `field` needs, where it declares one.
fn undoable(field: Option<&str>) -> tilo_actions::ActionNeed {
    tilo_actions::ActionNeed {
        kind: ActionKind::Undoable,
        field: field.map(str::to_owned),
        privileged: false,
    }
}

#[test]
fn a_commenters_device_runs_what_its_role_covers_where_the_action_sits() {
    let (mut peer, authority, _, _, now) = crate::admission_tests::seeded();
    let (_, cert, person) =
        crate::admission_tests::add_member_with(&mut peer, &authority, now, 7, 8);
    let device = cert.device_public_key;
    peer.governance_mut()
        .set_member_role(person, Role::Commenter, &authority, now)
        .unwrap();

    // Discussion's `comment` declares the `comments` field, and the Commenter's grant covers
    // that field at any depth: their device runs it.
    assert!(
        peer.permits_app_action(
            device,
            (DISCUSSION, COMMENTS, "comment"),
            &undoable(Some("comments")),
            now
        )
        .unwrap(),
        "a commenter comments"
    );
    // The same undoable kind at the record is what an editor holds, not a commenter: the field
    // is what their grant covers.
    assert!(
        !peer
            .permits_app_action(
                device,
                (DISCUSSION, COMMENTS, "comment"),
                &undoable(None),
                now
            )
            .unwrap(),
        "the field is what a commenter's grant covers"
    );
    // Tasks' own create is undoable at the record, which a commenter does not hold.
    assert!(
        !peer
            .permits_app_action(
                device,
                (TASKS, "tl:so.tilo.tasks", "new_task"),
                &undoable(None),
                now
            )
            .unwrap(),
        "a commenter creates no task"
    );

    // A viewer reads and writes nothing, the comments included.
    peer.governance_mut()
        .set_member_role(person, Role::Viewer, &authority, now)
        .unwrap();
    assert!(
        !peer
            .permits_app_action(
                device,
                (DISCUSSION, COMMENTS, "comment"),
                &undoable(Some("comments")),
                now
            )
            .unwrap(),
        "a viewer comments on nothing"
    );
}
