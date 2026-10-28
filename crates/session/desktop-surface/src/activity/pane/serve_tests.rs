use std::rc::Rc;

use automerge::ActorId;
use collab_types::caps::{Access, CapOverride, Scope, Subject};
use tilo_actions::remote::workspace_activity as wire;

use super::super::{ActivityView, RoomActivityPane};
use crate::activity::testing::{TestHost, TestTree};

/// A room another person changed three times since this device arrived.
fn room_a_peer_changed() -> (TestTree, u64) {
    let mut tree = TestTree::bootstrap_demo();
    let reader = tree.doc.get_actor().clone();
    tree.doc.set_actor(ActorId::from(b"ada-device".as_slice()));
    let owner = "test-owner".to_owned();
    tree.adopt_ownerless_workspaces(&owner);
    let workspace = tree.desktop.active;
    for path in ["/home/ada/notes", "/home/ada/drafts"] {
        tree.grant_in_workspace(
            workspace,
            &owner,
            CapOverride::grant(
                Subject::app("files"),
                Scope::FsPath(path.into()),
                Access::Read,
            ),
        )
        .expect("the owner grants in their own room");
    }
    if let Some(workspace) = tree
        .desktop
        .workspaces
        .iter_mut()
        .find(|candidate| candidate.id == tree.desktop.active)
    {
        workspace.name = "Design review".into();
    }
    tree.seed_doc_from_cache();
    tree.doc.set_actor(reader);
    (tree, workspace.raw())
}

fn pane() -> RoomActivityPane {
    let mut pane = RoomActivityPane::new();
    pane.set_host(Rc::new(TestHost::default()));
    pane
}

fn shown(summary: &wire::Summary) -> Vec<String> {
    summary
        .briefs
        .iter()
        .flat_map(|brief| brief.entries.iter().cloned())
        .collect()
}

#[test]
fn marking_the_summary_viewed_moves_what_is_new_and_leaves_the_room_alone() {
    let (mut tree, workspace) = room_a_peer_changed();
    let heads = tree.doc.get_heads();
    let mut pane = pane();
    pane.refresh(&mut tree);
    pane.begin_summary(workspace);
    let before = pane.serve_summary();
    assert_eq!(before.workspace, "Design review");
    assert!(!before.briefs.is_empty(), "a peer's changes are new");
    let unread = pane.glance_signal(workspace).1;
    assert!(unread > 0);

    let outcome = pane
        .serve_action(
            wire::MARK_VIEWED,
            &serde_json::from_value(serde_json::json!({ "entries": shown(&before) })).unwrap(),
            &mut tree,
            None,
        )
        .expect("marking what was shown viewed");
    assert!(outcome.handled);
    let after = pane.serve_summary();
    assert!(after.briefs.is_empty(), "what was shown is no longer new");
    assert_eq!(pane.glance_signal(workspace).1, 0);
    assert_eq!(tree.doc.get_heads(), heads, "seeing history writes nothing");
}

/// Two people in one room, each with Activity open on their own device:
/// what is new is each person's own. One marking it viewed moves only their
/// summary and unread count; it writes nothing to the room the other could
/// receive.
#[test]
fn two_people_in_one_room_each_see_their_own_whats_new() {
    let (mut mine, workspace) = room_a_peer_changed();
    let mut theirs = mine.clone();
    theirs
        .doc
        .set_actor(ActorId::from(b"grace-device".as_slice()));
    let (mut my_pane, mut their_pane) = (pane(), pane());
    for (pane, tree) in [(&mut my_pane, &mut mine), (&mut their_pane, &mut theirs)] {
        pane.refresh(tree);
        pane.begin_summary(workspace);
    }
    let theirs_before = their_pane.serve_summary();
    let their_unread = their_pane.glance_signal(workspace).1;
    assert!(!theirs_before.briefs.is_empty() && their_unread > 0);

    let shown_to_me = shown(&my_pane.serve_summary());
    my_pane
        .serve_action(
            wire::MARK_VIEWED,
            &serde_json::from_value(serde_json::json!({ "entries": shown_to_me })).unwrap(),
            &mut mine,
            None,
        )
        .expect("marking what was shown viewed");
    assert!(my_pane.serve_summary().briefs.is_empty());
    assert_eq!(my_pane.glance_signal(workspace).1, 0);

    // Whatever my device has that theirs lacks reaches theirs.
    let sent = mine.export_changes_since(&theirs.doc.get_heads());
    assert!(sent.is_empty(), "marking viewed sends the room nothing");
    theirs.apply_changes(&sent).unwrap();
    their_pane.refresh(&mut theirs);
    assert_eq!(
        their_pane.serve_summary(),
        theirs_before,
        "their what's new is as it was"
    );
    assert_eq!(their_pane.glance_signal(workspace).1, their_unread);

    their_pane
        .serve_action(
            wire::MARK_VIEWED,
            &serde_json::from_value(serde_json::json!({ "entries": shown(&theirs_before) }))
                .unwrap(),
            &mut theirs,
            None,
        )
        .expect("they mark what they were shown viewed");
    assert!(their_pane.serve_summary().briefs.is_empty());
    my_pane.refresh(&mut mine);
    assert!(
        my_pane.serve_summary().briefs.is_empty(),
        "their reading changes nothing of mine"
    );
}

#[test]
fn what_is_shown_for_later_counts_as_seen_only_when_the_summary_ends() {
    let (mut tree, workspace) = room_a_peer_changed();
    let mut pane = pane();
    pane.refresh(&mut tree);
    pane.begin_summary(workspace);
    let summary = pane.serve_summary();
    pane.mark_viewed(
        workspace,
        &wire::Viewed {
            entries: shown(&summary),
            later: true,
        },
    );
    assert_eq!(
        pane.serve_summary(),
        summary,
        "the summary being read stays until it is closed"
    );
    pane.end_summary(workspace);
    assert!(pane.serve_summary().briefs.is_empty());
}

#[test]
fn the_feed_lists_newest_first_and_a_range_selects_by_id() {
    let (mut tree, _) = room_a_peer_changed();
    let mut pane = pane();
    pane.refresh(&mut tree);
    let feed = pane.serve_feed(&wire::FeedQuery::default(), &tree);
    assert_eq!(feed.total, pane.entries().len());
    assert!(feed.entries.len() >= 2);
    assert!(
        feed.entries
            .windows(2)
            .all(|pair| pair[0].at_ms >= pair[1].at_ms),
        "newest first"
    );
    assert!(feed.mutations);
    let newest = feed.entries[0].id.clone();
    let detail = pane.serve_detail(
        &wire::DetailQuery {
            range: wire::Range::one(&newest),
            compare: false,
        },
        &mut tree,
        None,
    );
    assert!(!detail.gone);
    assert!(!detail.title.is_empty());
    let gone = pane.serve_detail(
        &wire::DetailQuery {
            range: wire::Range::one(&"00".repeat(16)),
            compare: false,
        },
        &mut tree,
        None,
    );
    assert!(gone.gone, "a change not in the history has no detail");

    let first_page = pane.serve_feed(
        &wire::FeedQuery {
            limit: 1,
            ..wire::FeedQuery::default()
        },
        &tree,
    );
    assert_eq!(first_page.entries.len(), 1);
    assert_eq!(first_page.entries[0].id, newest);
    assert_eq!(first_page.total, feed.total);
}

#[test]
fn a_deep_link_is_offered_until_an_app_shows_it() {
    let (mut tree, _) = room_a_peer_changed();
    let mut pane = pane();
    pane.refresh(&mut tree);
    let entry = pane.entries()[0].id;
    pane.focus_deep_link(&crate::activity::ActivityDeepLink {
        entry_id: Some(*entry.as_bytes()),
        ..Default::default()
    });
    let first = pane.serve_focus().expect("the focus is offered");
    assert_eq!(
        first.entry.as_deref(),
        Some(wire::entry_hex(entry.as_bytes()).as_str())
    );
    assert_eq!(first.view, wire::View::History, "a change shows in History");
    let revision = pane.served_revision();
    pane.focus_deep_link(&crate::activity::ActivityDeepLink {
        author: Some("Ada".into()),
        ..Default::default()
    });
    let second = pane.serve_focus().unwrap();
    assert!(second.serial > first.serial);
    assert_eq!(second.author.as_deref(), Some("Ada"));
    assert_ne!(
        pane.served_revision(),
        revision,
        "a new focus is a new read"
    );

    pane.take_focus(first.serial);
    assert_eq!(
        pane.serve_focus(),
        Some(second.clone()),
        "showing an older focus leaves the newer one"
    );
    pane.serve_action(
        wire::FOCUSED,
        &serde_json::json!({ "serial": second.serial })
            .as_object()
            .cloned()
            .unwrap(),
        &mut tree,
        None,
    )
    .unwrap();
    assert_eq!(
        pane.serve_focus(),
        None,
        "a shown focus is not offered again"
    );
}

#[test]
fn a_view_asked_for_is_offered_without_narrowing_the_history() {
    let mut pane = pane();
    pane.focus_view(ActivityView::Replay);
    let focus = pane.serve_focus().expect("the view is offered");
    assert_eq!(focus.view, wire::View::Replay);
    assert_eq!((focus.author, focus.lens, focus.entry), (None, None, None));
}
