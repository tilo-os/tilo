//! The activity model served through the broker as an app reaches it: a
//! person follows the history and sees it move, a participant without the
//! permission is refused, and marking what was new as viewed moves it.

use std::sync::Arc;

use tilo_actions::InvocationUpdate;
use tilo_actions::remote::workspace_activity as wire;
use tilo_actions::remote::{Caller, InvokeArgument, InvokeRequest};

use crate::shell::overlay_host::ShellOverlayHost;
use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree};

const OWNER: &str = "owner";

struct Fixture {
    _home: tilo_test_env::TestHome,
    chrome: crate::shell::chrome_state::Chrome,
    host: ShellOverlayHost,
    broker: tilo_action_broker::collab_broker::AppActionBroker,
    room: String,
    /// Says the broker asked the activity model something.
    asked: tokio::sync::mpsc::UnboundedReceiver<()>,
}

fn fixture() -> Fixture {
    let home = tilo_test_env::TestHome::new().unwrap();
    let mut workspace = Workspace::new(WorkspaceId(31), "Design review");
    workspace.set_owner(OWNER);
    workspace.admit_agent_preset(
        "scribe",
        tilo_action_policy::roles::AgentPreset::Observer,
        &tilo_action_policy::Subject::person(OWNER.to_owned()),
    );
    let room_id = workspace.room();
    let room = room_id.to_string();
    let mut tree = WorkspaceTree::default();
    tree.desktop.active = workspace.id;
    tree.desktop.workspaces.push(workspace);
    // The room as it was set up here: setup is no change to show.
    tree.seed_doc_from_cache();
    // Activity reads each room's admitted document, not the whole-desktop
    // mirror; this room's document is what its own engine replicates.
    tree.sync_room_doc(room_id);
    let chrome = crate::shell::chrome_state::ChromeState::new(tree);
    let handle = chrome.borrow().tree_handle();
    let broker =
        tilo_action_broker::collab_broker_capnp::CapnpBrokerState::test_broker_with_authority(
            Arc::new(crate::shell::broker_services::NativeWorkspaceAuthority(
                handle,
            )),
            tilo_action_broker::services::HostServices {
                host_steps: vec![super::broker_door()],
                ..Default::default()
            },
        );
    let (ask, asked) = tokio::sync::mpsc::unbounded_channel();
    super::wake_with(Arc::new(move || {
        let _ = ask.send(());
    }));
    let host =
        ShellOverlayHost::new(Box::new(ui::widget::Container::new())).with_chrome(chrome.clone());
    Fixture {
        _home: home,
        chrome,
        host,
        broker,
        room,
        asked,
    }
}

fn request(action: &str, argument: InvokeArgument) -> InvokeRequest {
    InvokeRequest {
        on_behalf_of: None,
        invocation: tilo_actions::InvocationId::new(),
        target: wire::target(action),
        argument,
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
    }
}

impl Fixture {
    fn person(&self) -> Caller {
        Caller::local_person(OWNER).in_room(self.room.clone())
    }

    /// A change by someone else in the room, as the history reads it: a
    /// change of the room's admitted document by the peer's actor.
    fn rename(&self, name: &str) {
        use automerge::transaction::Transactable as _;
        let state = self.chrome.borrow();
        let mut guard = state.tree_mut();
        let tree = &mut *guard;
        let room = tree
            .active_workspace()
            .map(Workspace::room)
            .expect("the fixture's workspace has its room");
        let doc = tree.room_docs.get_mut(&room).expect("the room's document");
        let reader = doc.get_actor().clone();
        doc.set_actor(automerge::ActorId::from(b"ada-device".as_slice()));
        doc.put(automerge::ROOT, "name", name).unwrap();
        tilo_resource::history::commit_stamped(doc);
        doc.set_actor(reader);
        if let Some(workspace) = tree.desktop.workspaces.first_mut() {
            workspace.name = name.into();
        }
        drop(guard);
    }

    /// Bring the model up to date with the room, as a frame does.
    fn refresh(&mut self) {
        self.host.refresh_room_activity_for_test();
    }

    fn invoke(&self, caller: &Caller, request: &InvokeRequest) -> bool {
        self.broker
            .state()
            .action_invoke(caller, None, None, request)
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .expect("admission answers at once")
            .is_ok()
    }

    /// Run what the broker asked of the model until the invocation reads
    /// as `until` says, each round waiting for the broker to ask again.
    fn served(
        &mut self,
        caller: &Caller,
        request: &InvokeRequest,
        until: impl Fn(&InvocationUpdate) -> bool,
    ) -> InvocationUpdate {
        let mut changes = self.broker.invocation_changes();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        loop {
            self.host.serve_activity_for_test();
            if let Some(observed) = self
                .broker
                .state()
                .action_observe(caller, &request.invocation)
                && until(&observed.update)
            {
                return observed.update;
            }
            // The broker asks the model again, or the invocation moved:
            // whichever comes first is looked at.
            let asked = &mut self.asked;
            let woke = runtime.block_on(async {
                tokio::time::timeout(tilo_test_env::hang_bound(), async {
                    tokio::select! {
                        _ = changes.changed() => {}
                        _ = asked.recv() => {}
                    }
                })
                .await
            });
            assert!(woke.is_ok(), "the served view moves");
        }
    }
}

fn item<T: serde::de::DeserializeOwned>(update: &InvocationUpdate) -> Option<T> {
    match update {
        InvocationUpdate::Item { value, .. } => serde_json::from_value(value.clone()).ok(),
        _ => None,
    }
}

#[test]
#[serial_test::serial]
fn a_followed_feed_shows_each_change_to_the_history_as_it_lands() {
    let mut fixture = fixture();
    fixture.rename("Design review · draft");
    fixture.refresh();
    let person = fixture.person();
    let feed = request(wire::FEED, wire::argument(&wire::FeedQuery::default()));
    assert!(
        fixture.invoke(&person, &feed),
        "the person follows the feed"
    );
    let first = fixture.served(&person, &feed, |update| {
        item::<wire::Feed>(update).is_some_and(|feed| !feed.entries.is_empty())
    });
    let before: wire::Feed = item(&first).unwrap();
    assert_eq!(before.workspace, "Design review · draft");

    fixture.rename("Design review · final");
    fixture.refresh();
    let moved = fixture.served(&person, &feed, |update| {
        item::<wire::Feed>(update).is_some_and(|feed| feed.total > before.total)
    });
    drop(fixture);
    let after: wire::Feed = item(&moved).unwrap();
    assert_eq!(after.workspace, "Design review · final");
    assert!(
        after.entries.len() > before.entries.len(),
        "the new change is in the feed without asking again"
    );
}

#[test]
#[serial_test::serial]
fn a_participant_without_read_permission_gets_the_refusal_not_the_history() {
    let mut fixture = fixture();
    fixture.rename("Design review · private");
    fixture.refresh();
    let stranger = Caller::local_agent("intruder").in_room(fixture.room.clone());
    let feed = request(wire::FEED, InvokeArgument::None);
    assert!(
        !fixture.invoke(&stranger, &feed),
        "an agent the room never admitted reads nothing"
    );
    fixture.host.serve_activity_for_test();
    assert!(
        fixture
            .broker
            .state()
            .action_observe(&stranger, &feed.invocation)
            .is_none_or(|observed| item::<wire::Feed>(&observed.update).is_none()),
        "no history reached the refused caller"
    );

    let agent = Caller::local_agent("scribe").in_room(fixture.room.clone());
    let all = request(
        wire::FEED,
        wire::argument(&wire::FeedQuery {
            scope: wire::Scope::AllWorkspaces,
            ..wire::FeedQuery::default()
        }),
    );
    assert!(
        !fixture.invoke(&agent, &all),
        "every workspace's history is the person's own to read"
    );
    let viewed = request(wire::MARK_VIEWED, wire::argument(&wire::Viewed::default()));
    assert!(
        !fixture.invoke(&agent, &viewed) || {
            let ended = fixture.served(&agent, &viewed, InvocationUpdate::is_terminal);
            matches!(ended, InvocationUpdate::Failed { .. })
        },
        "only the person marks what they have seen"
    );
    drop(fixture);
}

#[test]
#[serial_test::serial]
fn marking_what_is_new_as_viewed_moves_the_summary() {
    let mut fixture = fixture();
    fixture.refresh();
    fixture.rename("Design review · v2");
    fixture.rename("Design review · v3");
    fixture.refresh();
    let person = fixture.person();
    let summary = request(wire::SUMMARY, InvokeArgument::None);
    assert!(fixture.invoke(&person, &summary));
    let shown = fixture.served(&person, &summary, |update| {
        item::<wire::Summary>(update).is_some_and(|summary| !summary.briefs.is_empty())
    });
    let shown: wire::Summary = item(&shown).unwrap();
    let entries = shown
        .briefs
        .iter()
        .flat_map(|brief| brief.entries.clone())
        .collect();

    let viewed = request(
        wire::MARK_VIEWED,
        wire::argument(&wire::Viewed {
            entries,
            later: false,
        }),
    );
    assert!(fixture.invoke(&person, &viewed));
    let done = fixture.served(&person, &viewed, InvocationUpdate::is_terminal);
    assert!(
        matches!(done, InvocationUpdate::Succeeded { .. }),
        "{done:?}"
    );
    let moved = fixture.served(&person, &summary, |update| {
        item::<wire::Summary>(update).is_some_and(|summary| summary.briefs.is_empty())
    });
    drop(fixture);
    assert!(item::<wire::Summary>(&moved).is_some());
}
