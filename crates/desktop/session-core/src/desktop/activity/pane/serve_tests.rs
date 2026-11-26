use std::rc::Rc;

use crate::desktop::activity::vocabulary as wire;
use automerge::ActorId;
use collab_types::caps::{Access, CapOverride, Scope};
use tilo_action_policy::Grantee;

use super::super::{ActivityView, WorkspaceActivityPane};
use crate::desktop::activity::testing::{TestHost, TestTree};

/// A workspace another person changed three times since this device arrived.
fn workspace_a_peer_changed() -> (TestTree, tilo_desktop_model::WorkspaceId) {
    let mut tree = TestTree::bootstrap_demo();
    let reader = tree.doc.get_actor().clone();
    tree.doc.set_actor(ActorId::from(b"ada-device".as_slice()));
    let owner = "test-owner".to_owned();
    tree.adopt_ownerless_workspaces(&owner);
    let workspace = tree.desktop.active;
    // The grants and the rename are Ada's, as another of the workspace's devices writes them.
    let ada = ActorId::from(b"ada-device".as_slice());
    tree.written_as(workspace, &ada, |tree| {
        for path in ["/home/ada/notes", "/home/ada/drafts"] {
            tree.grant_in_workspace(
                workspace,
                &owner,
                CapOverride::grant(
                    Grantee::person("ada"),
                    Scope::FsPath(path.into()),
                    Access::Read,
                )
                .through("files"),
            )
            .expect("the owner grants in their own workspace");
        }
        if let Some(named) = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|candidate| candidate.id() == workspace)
        {
            named.name = "Design review".into();
        }
        tree.govern(workspace);
    });
    tree.doc.set_actor(reader);
    (tree, workspace)
}

fn pane() -> WorkspaceActivityPane {
    let mut pane = WorkspaceActivityPane::new();
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

tilo_test_env::scenario! {
    fn the_served_activity_keeps_each_readers_whats_new;

    phase marking_the_summary_viewed_moves_what_is_new_and_leaves_the_workspace_alone;
    {
        let (mut tree, workspace) = workspace_a_peer_changed();
        let heads = tree.doc.get_heads();
        let mut pane = pane();
        pane.refresh(&mut tree);
        pane.begin_summary(workspace);
        let before = pane.serve_summary();
        assert_eq!(before.workspace, "Design review");
        assert!(!before.briefs.is_empty(), "a peer's changes are new");
        let unread = pane.passive_unread;
        assert!(unread > 0);

        let outcome = pane
            .serve_action(
                wire::MARK_VIEWED,
                &serde_json::from_value(serde_json::json!({ "entries": shown(&before) })).unwrap(),
                &mut tree,
            )
            .expect("marking what was shown viewed");
        assert!(outcome.handled);
        let after = pane.serve_summary();
        assert!(after.briefs.is_empty(), "what was shown is no longer new");
        assert_eq!(pane.passive_unread, 0);
        assert_eq!(tree.doc.get_heads(), heads, "seeing history writes nothing");
    }

    phase two_people_in_one_workspace_each_see_their_own_whats_new;
    // Two people in one workspace, each with Activity open on their own device: what is new is each
    // person's own. One marking it viewed moves only their summary and unread count; it writes nothing
    // to the workspace the other could receive.
    {
        let (mut mine, workspace) = workspace_a_peer_changed();
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
        let their_unread = their_pane.passive_unread;
        assert!(!theirs_before.briefs.is_empty() && their_unread > 0);

        let shown_to_me = shown(&my_pane.serve_summary());
        my_pane
            .serve_action(
                wire::MARK_VIEWED,
                &serde_json::from_value(serde_json::json!({ "entries": shown_to_me })).unwrap(),
                &mut mine,
            )
            .expect("marking what was shown viewed");
        assert!(my_pane.serve_summary().briefs.is_empty());
        assert_eq!(my_pane.passive_unread, 0);

        // Whatever my device has that theirs lacks reaches theirs.
        let sent = mine.export_changes_since(&theirs.doc.get_heads());
        assert!(
            sent.is_empty(),
            "marking viewed sends the workspace nothing"
        );
        theirs.apply_changes(&sent).unwrap();
        their_pane.refresh(&mut theirs);
        assert_eq!(
            their_pane.serve_summary(),
            theirs_before,
            "their what's new is as it was"
        );
        assert_eq!(their_pane.passive_unread, their_unread);

        their_pane
            .serve_action(
                wire::MARK_VIEWED,
                &serde_json::from_value(serde_json::json!({ "entries": shown(&theirs_before) }))
                    .unwrap(),
                &mut theirs,
            )
            .expect("they mark what they were shown viewed");
        assert!(their_pane.serve_summary().briefs.is_empty());
        my_pane.refresh(&mut mine);
        assert!(
            my_pane.serve_summary().briefs.is_empty(),
            "their reading changes nothing of mine"
        );
    }

    phase what_is_shown_for_later_counts_as_seen_only_when_the_summary_ends;
    {
        let (mut tree, workspace) = workspace_a_peer_changed();
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
}

tilo_test_env::scenario! {
    fn the_served_feed_selects_ranges_and_offers_the_view_asked_for;

    phase the_feed_lists_newest_first_and_a_range_selects_by_id;
    {
        let (mut tree, _) = workspace_a_peer_changed();
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
        );
        assert!(!detail.gone);
        assert!(!detail.title.is_empty());
        let gone = pane.serve_detail(
            &wire::DetailQuery {
                range: wire::Range::one(&"00".repeat(16)),
                compare: false,
            },
            &mut tree,
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

        assert!(pane.select_range(&wire::Range::one(&newest)));
        let selected = pane.timeline.selection();
        let narrowed = pane.serve_feed(
            &wire::FeedQuery {
                author: Some("nobody-by-that-name".into()),
                ..wire::FeedQuery::default()
            },
            &tree,
        );
        assert_eq!(narrowed.total, 0);
        assert_eq!(
            pane.timeline.selection(),
            selected,
            "another reader's narrowing keeps what this one selected"
        );
        assert_eq!(pane.serve_feed(&wire::FeedQuery::default(), &tree).total, feed.total);
    }

    phase a_deep_link_is_offered_until_an_app_shows_it;
    {
        let (mut tree, _) = workspace_a_peer_changed();
        let mut pane = pane();
        pane.refresh(&mut tree);
        let entry = pane.entries()[0].id;
        pane.focus_deep_link(&crate::desktop::activity::ActivityDeepLink {
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
        pane.focus_deep_link(&crate::desktop::activity::ActivityDeepLink {
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
        )
        .unwrap();
        assert_eq!(
            pane.serve_focus(),
            None,
            "a shown focus is not offered again"
        );
    }

    phase a_view_asked_for_is_offered_without_narrowing_the_history;
    {
        let mut pane = pane();
        pane.focus_view(ActivityView::Replay);
        let focus = pane.serve_focus().expect("the view is offered");
        assert_eq!(focus.view, wire::View::Replay);
        assert_eq!((focus.author, focus.lens, focus.entry), (None, None, None));
    }
}

tilo_test_env::scenario! {
    fn the_served_feed_lists_the_newest_change_first;

    phase renames_stamped_apart_read_newest_first;
    {
        let (mut tree, workspace) = workspace_a_peer_changed();
        // Three renames, each committed a second after the last. Commits made in one test run can
        // share a second, and equal timestamps would hide an order that only reverses.
        let base = tilo_clock::unix_now_i64().saturating_add(60);
        for (offset, name) in ["Order test one", "Order test two", "Order test three"]
            .into_iter()
            .enumerate()
        {
            let Some(state) = tree
                .desktop
                .workspaces
                .iter_mut()
                .find(|candidate| candidate.id() == workspace)
            else {
                panic!("the workspace is the active one");
            };
            state.name = name.into();
            assert!(tree.write_governance(workspace), "a rename writes");
            let at = base.saturating_add(i64::try_from(offset).unwrap_or_default());
            assert!(
                tilo_resource::history::commit_stamped_at(tree.governance_of(workspace), at)
                    .is_some(),
                "the rename is committed"
            );
        }
        let mut pane = pane();
        pane.refresh(&mut tree);
        // The query the shell's Activity window reads the history with.
        let feed = pane.serve_feed(
            &wire::FeedQuery {
                limit: 0,
                ..wire::FeedQuery::default()
            },
            &tree,
        );
        let newest = feed.entries.first().expect("the history has changes");
        assert_eq!(
            newest.at_ms,
            base.saturating_add(2).saturating_mul(1_000),
            "the newest rename is the first entry: {:?}",
            feed.entries
                .iter()
                .map(|entry| (entry.at_ms, entry.summary.as_str()))
                .collect::<Vec<_>>()
        );
        assert!(
            newest.summary.contains("Order test three"),
            "the first entry is the newest change: {}",
            newest.summary
        );
        let renames: Vec<&str> = feed
            .entries
            .iter()
            .filter_map(|entry| {
                ["Order test one", "Order test two", "Order test three"]
                    .into_iter()
                    .find(|name| entry.summary.contains(name))
            })
            .collect();
        assert_eq!(
            renames,
            ["Order test three", "Order test two", "Order test one"],
            "the feed reads the newest change first"
        );
    }
}

/// An agent's write reaches an app as the agent: its member name and colour, as software, for
/// the person it acted for.
#[test]
fn an_agents_write_is_served_as_the_agent_for_its_person() {
    let agent = tilo_actions::MemberId::agent("agent-key-1234abcd", "ben");
    let mut authors = ui::AuthorDirectory::new();
    authors.register_member(
        &tilo_actions::MemberId::person("ben"),
        ui::widget::history::HistoryAuthor::new("Ben", 0.0, "ben00000"),
    );
    authors.register_member(
        &agent,
        ui::history_source::author_coloured("Planner", "agent", (1, 2, 3)),
    );
    let invoker = tilo_actions::ActionInvoker::admitted(
        &tilo_actions::Acting::agent(agent.id(), "ben")
            .in_session("s1")
            .acting_for(tilo_actions::Acting::person("ben")),
    )
    .named(Some("Draft the plan for the week".into()));
    let entry = tilo_actions::ActionContext::default().with_invoker(invoker);
    let served = super::author(&authors.resolve_entry("00", Some(&entry)));
    assert_eq!(served.name, "Planner");
    assert_eq!(served.rgb, [1, 2, 3]);
    assert_eq!(served.relation, wire::Relation::Software);
    assert_eq!(served.on_behalf_of.as_deref(), Some("Ben"));
}
