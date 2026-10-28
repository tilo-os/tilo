//! What each presence visibility lets leave a session: presence is per
//! resource, layout is per person.
use super::*;

/// Every kind of presence the desktop and its apps publish, at each
/// visibility. Under "That I'm here" only the desktop's word that the
/// person is in the workspace leaves, and what they do inside a shared
/// resource (an app's caret, selection, typing, pointer or arrival in the
/// record it shows): never which window or tab is in front, the desktop
/// pointer over a window, whom they follow, cursor chat, a status
/// inferred from what they do, or an app's word keyed to the workspace
/// rather than a resource. Under "Where I am" all of it leaves; under
/// "Nothing" none of it but leaving.
#[test]
#[allow(clippy::too_many_lines, reason = "one table read top to bottom")]
fn each_visibility_lets_out_exactly_its_presence() {
    use PresencePayload as P;
    use ui::PresenceVisibility::{Nothing, ThatImHere, WhereIAm};
    let mut pair = pair("presence-audit");
    let room = pair.room.to_string();
    let workspace = PresenceKey::Workspace { id: room.clone() };
    let tab = PresenceKey::Tab {
        id: format!("{:x}", 0x0042_0000_0000_0007_u64),
    };
    let note = PresenceKey::Resource {
        app: "so.tilo.notes".into(),
        id: "note:groceries".into(),
        kind: "note".into(),
    };
    let paragraph = PresenceKey::Widget {
        path: vec![ui::presence::PathSegment::Field("body".into())],
    };
    let in_note = PresenceKey::Pointer {
        space: ui::presence::PointerSpace::Surface {
            surface: "so.tilo.notes".into(),
        },
    };
    let over_window = PresenceKey::tile_pointer(room.clone(), 7);
    let frame = |key: &PresenceKey, payload: &P| {
        crate::shell::collab_presence::to_peer(key, payload).expect("presence encodes")
    };
    let inferred = P::Status {
        text: "Editing the budget in Notes".into(),
        automatic: true,
    };
    let typed = P::Status {
        text: "back in 5".into(),
        automatic: false,
    };
    // (what, app, frame, "That I'm here" lets it out)
    let table = [
        (
            "arriving",
            "tilo-shell",
            frame(&workspace, &P::Arrived),
            true,
        ),
        (
            "a typed status",
            "tilo-shell",
            frame(&workspace, &typed),
            true,
        ),
        (
            "how available",
            "tilo-shell",
            frame(
                &workspace,
                &P::Availability {
                    state: ui::Availability::Focusing,
                    call: None,
                },
            ),
            true,
        ),
        (
            "the tab in front",
            "tilo-shell",
            frame(&tab, &P::Arrived),
            false,
        ),
        (
            "an inferred status",
            "tilo-shell",
            frame(&workspace, &inferred),
            false,
        ),
        (
            "whom they follow",
            "tilo-shell",
            frame(
                &workspace,
                &P::Following {
                    target: Some("peer@1".into()),
                },
            ),
            false,
        ),
        (
            "the desktop pointer",
            "tilo-shell",
            frame(&over_window, &P::Pointer { x: 1.0, y: 2.0 }),
            false,
        ),
        (
            "an app keyed to the workspace",
            "so.tilo.notes",
            frame(&workspace, &P::Arrived),
            false,
        ),
        (
            "an app keyed to a window",
            "so.tilo.notes",
            frame(&tab, &P::Arrived),
            false,
        ),
        (
            "arriving in a note",
            "so.tilo.notes",
            frame(&note, &P::Arrived),
            true,
        ),
        (
            "a caret in a note",
            "so.tilo.notes",
            frame(&paragraph, &P::Caret { pos: 3 }),
            true,
        ),
        (
            "a selection in a note",
            "so.tilo.notes",
            frame(&paragraph, &P::Selection { start: 1, end: 4 }),
            true,
        ),
        (
            "typing in a note",
            "so.tilo.notes",
            frame(&paragraph, &P::Typing { start: 3, end: 5 }),
            true,
        ),
        (
            "a pointer in a note",
            "so.tilo.notes",
            frame(&in_note, &P::Pointer { x: 10.0, y: 20.0 }),
            true,
        ),
    ];
    let sent = |pair: &mut Pair, app: &str, presence: collab_types::PeerPresence| {
        pair.guest_net
            .publish_app_presence(app.into(), room.clone(), presence);
        matches!(
            pair.guest_cmds.try_recv(),
            Ok(NetCmd::Publish {
                msg: NeighbourMsg::AppPresenceV2 { .. },
                ..
            })
        )
    };
    for visibility in [WhereIAm, ThatImHere, Nothing] {
        pair.guest_net.set_presence_visibility(&room, visibility);
        for (what, app, presence, here) in table.clone() {
            let expected = match visibility {
                WhereIAm => true,
                ThatImHere => here,
                Nothing => false,
            };
            assert_eq!(
                sent(&mut pair, app, presence),
                expected,
                "{what} under {visibility:?}"
            );
        }
        let left = frame(&workspace, &P::Left);
        assert!(
            sent(&mut pair, "tilo-shell", left),
            "leaving is always said ({visibility:?})"
        );
    }
    let _ = std::fs::remove_dir_all(&pair.dir);
}

/// Two members at "That I'm here" (a public room's default; whether the
/// room shares its layout plays no part in presence) with the same note
/// open: the guest's caret in the note reaches the host's Notes, and
/// nothing of the guest's other windows leaves its device.
#[test]
fn at_that_im_here_the_people_in_a_note_see_each_others_caret_and_no_other_window() {
    use PresencePayload as P;
    let mut pair = pair("personal-note");
    let room = pair.room.to_string();
    for net in [&pair.guest_net, &pair.host_net] {
        net.set_presence_visibility(&room, ui::PresenceVisibility::ThatImHere);
    }
    let paragraph = PresenceKey::Widget {
        path: vec![ui::presence::PathSegment::Field("body".into())],
    };
    let caret = crate::shell::collab_presence::to_peer(&paragraph, &P::Caret { pos: 12 })
        .expect("a caret encodes");
    pair.guest_net
        .publish_app_presence("so.tilo.notes".into(), room.clone(), caret);
    let NeighbourMsg::AppPresenceV2 {
        peer,
        app_id,
        cluster_id,
        presence,
        ..
    } = published(&mut pair.guest_cmds)
    else {
        panic!("the caret goes out as app presence");
    };
    pair.host_net
        .handle_typed_app_presence_message(peer, None, app_id, cluster_id, presence);
    let reached: Vec<(String, collab_types::PeerPresencePayload)> = pair
        .host_net
        .inbound_app_presence_handle()
        .lock()
        .expect("inbound presence")
        .iter()
        .map(|inbound| (inbound.app_id.clone(), inbound.presence.payload.clone()))
        .collect();
    assert!(
        reached.contains(&(
            "so.tilo.notes".to_owned(),
            collab_types::PeerPresencePayload::Caret { pos: 12 }
        )),
        "the host's Notes draws the guest's caret: {reached:?}"
    );

    // The guest brings another window to the front, and points over it.
    let other = crate::shell::collab_presence::to_peer(
        &PresenceKey::Tab {
            id: format!("{:x}", 0x0042_0000_0000_0009_u64),
        },
        &P::Arrived,
    )
    .expect("an arrival encodes");
    pair.guest_net
        .publish_app_presence("tilo-shell".into(), room.clone(), other);
    pair.guest.set_pointer(Some(on_desk(300.0, 200.0)));
    assert!(!pair.guest.publish_local_cursor_at(&room, Instant::now()));
    assert!(
        pair.guest_cmds.try_recv().is_err(),
        "nothing of the guest's other windows leaves"
    );
    let _ = std::fs::remove_dir_all(&pair.dir);
}

/// The voice channel someone is in (its id and name) says where they
/// are: "That I'm here" sends how available they are but not their
/// call; "Where I am" sends both.
#[test]
fn a_call_reaches_only_those_who_may_see_where_you_are() {
    let mut pair = pair("call-visibility");
    let room = pair.room.to_string();
    let key = PresenceKey::Workspace { id: room.clone() };
    let call = ui::CallPresence {
        channel: "c_voice".into(),
        name: "voice".into(),
    }
    .encode();
    let available = PresencePayload::Availability {
        state: ui::Availability::Available,
        call: Some(call.clone()),
    };
    let said = |pair: &mut Pair| match published(&mut pair.guest_cmds) {
        NeighbourMsg::AppPresenceV2 { presence, .. } => match presence.payload {
            collab_types::PeerPresencePayload::Availability { call, .. } => call,
            other => panic!("expected availability, got {other:?}"),
        },
        other => panic!("expected room presence, got {other:?}"),
    };
    for (visibility, heard) in [
        (ui::PresenceVisibility::ThatImHere, None),
        (ui::PresenceVisibility::WhereIAm, Some(call)),
    ] {
        pair.guest_net.set_presence_visibility(&room, visibility);
        let frame =
            crate::shell::collab_presence::to_peer(&key, &available).expect("availability encodes");
        pair.guest_net
            .publish_app_presence("tilo-shell".into(), room.clone(), frame);
        assert_eq!(said(&mut pair), heard, "{visibility:?}");
    }
    let _ = std::fs::remove_dir_all(&pair.dir);
}
