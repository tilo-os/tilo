//! Two people share a workspace hosted on the device of the person who made
//! it: invitations come from that device's host, the other person joins
//! through the join exchange there, and both edit the same windows and the
//! same app document.

use std::sync::Arc;

use tilo_user_identity::DeviceCapabilities;

use super::{Device, Person, Wakes, hosted_workspace, scratch, until, until_caught_up};

#[test]
#[serial_test::serial]
#[allow(
    clippy::too_many_lines,
    reason = "one sharing journey across three people's devices, told in order"
)]
fn an_invited_person_joins_the_workspace_on_its_makers_device_and_both_edit_it() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let (ada, grace, eve) = (Person::new(31), Person::new(32), Person::new(33));
    let mut p = Device::start(
        &ada,
        "Ada's laptop",
        &dir.path().join("ada"),
        0x0A01,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let mut q = Device::start(
        &grace,
        "Grace's laptop",
        &dir.path().join("grace"),
        0x0B01,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let r = Device::start(
        &eve,
        "Eve's laptop",
        &dir.path().join("eve"),
        0x0C01,
        DeviceCapabilities::primary(),
        &wakes,
    );
    // Each keeps its workspaces' layouts through its session's broker, as a
    // desktop session does.
    p.carry_layouts();
    q.carry_layouts();
    assert!(
        q.hosted_rooms().is_empty(),
        "a device that hosts no room runs no host"
    );

    // Ada makes a workspace; it is hosted on her laptop.
    let (local, room) = hosted_workspace(&p, &[&q, &r], "Garden");
    assert_eq!(p.workspace_rooms(), vec![room]);
    assert_eq!(p.hosted_rooms(), vec![room]);

    // She invites Grace twice: a link anyone holding it could use, and one
    // only Grace's devices can redeem.
    let link = p.invite(local, "Grace", None);
    let bound = p.invite(local, "Grace (bound)", Some(grace.user()));

    // Eve cannot redeem the invitation bound to Grace.
    r.join(&bound).unwrap();
    let refused = until(&[&p, &q, &r], "the bound invitation to refuse Eve", || {
        r.joins.lock().unwrap().pop()
    });
    assert!(
        refused.is_err(),
        "an invitation bound to Grace admits nobody else: {refused:?}"
    );
    assert!(!r.holds(room));

    // Grace joins with the bound invitation, through the join exchange at
    // Ada's laptop. Her workspace is Ada's room.
    q.join(&bound).unwrap();
    let joined = until(&[&p, &q, &r], "Grace to join the workspace", || {
        q.joins.lock().unwrap().pop()
    })
    .unwrap_or_else(|error| panic!("Grace joins: {error}"));
    assert_eq!(joined, room);
    until(&[&p, &q], "Grace's device to hold the workspace", || {
        q.holds(room).then_some(())
    });
    assert_eq!(
        q.workspace_rooms(),
        vec![room],
        "Grace's workspace is Ada's room"
    );
    assert!(
        q.hosted_rooms().is_empty(),
        "joining a room hosted elsewhere starts no host on Grace's device"
    );
    until(&[&p, &q], "both rosters to show both people", || {
        let both = [ada.user(), grace.user()];
        (both.iter().all(|person| p.roster(room).contains(person))
            && both.iter().all(|person| q.roster(room).contains(person)))
        .then_some(())
    });

    // Ada opens a window and writes in the notes app; Grace sees both.
    let ada_window = p.open_window(room, "Notes");
    p.open_notes_first(room);
    until(&[&p, &q], "the note's record on Ada's device", || {
        p.holds_note(room).then_some(())
    });
    p.write_note(room, "Plant the tulips");
    until_caught_up(
        &[&p, &q],
        "Ada's window and note to reach Grace",
        room,
        || {
            (q.shows_tab(room, ada_window) && q.note(room).as_deref() == Some("Plant the tulips"))
                .then_some(())
        },
    );

    // Grace edits; Ada sees it.
    q.write_note(room, "Plant the tulips and the crocuses");
    let grace_window = q.open_window(room, "Notes, Grace");
    until_caught_up(
        &[&p, &q],
        "Grace's edit and window to reach Ada",
        room,
        || {
            (p.note(room).as_deref() == Some("Plant the tulips and the crocuses")
                && p.shows_tab(room, grace_window))
            .then_some(())
        },
    );

    // Joining a second time, with the link, is the same workspace.
    q.join(&link).unwrap();
    let again = until(&[&p, &q], "the second join to answer", || {
        q.joins.lock().unwrap().pop()
    })
    .unwrap_or_else(|error| panic!("the second join answers: {error}"));
    assert_eq!(again, room);
    assert_eq!(q.workspaces_of(room), 1, "never a second workspace");
    assert_eq!(q.workspace_rooms(), vec![room]);

    // A cancelled invitation is refused.
    let cancelled = p.invite(local, "Eve", None);
    p.cancel_invitation(room, "Eve");
    r.join(&cancelled).unwrap();
    let refused = until(
        &[&p, &q, &r],
        "the cancelled invitation to refuse Eve",
        || r.joins.lock().unwrap().pop(),
    );
    assert!(
        refused.is_err(),
        "a cancelled invitation admits nobody: {refused:?}"
    );
    assert!(!r.holds(room));
    assert!(
        !p.roster(room).contains(&eve.user()),
        "Eve is not in Ada's roster"
    );
    drop((p, q, r));
}
