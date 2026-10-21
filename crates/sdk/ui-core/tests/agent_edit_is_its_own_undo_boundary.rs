//! An agent edits the document; the person then types; the person presses
//! Ctrl+Z once. Exactly one thing must disappear, and it must be theirs.
//!
//! `CollabEvent::ActionArrived` applies a change under somebody else's actor
//! without ever passing through a local commit. Left unrecorded, `last_heads`
//! stays behind that change and the next local edit brackets
//! `[stale_baseline, after]` — a range that *contains* the agent's work — so
//! one Ctrl+Z reverts both.
//!
//! The event reaches the app through the typed action subscription, drained by the same
//! `App::pump_collab_presence` a running window calls every frame.

use automerge::transaction::Transactable;
use automerge::{ROOT, ReadDoc};
use tilo_ui::widget::{AppModel, ModelPath};

use crate::support::typed_collab::TypedBroker;
use tilo_ui::app::App;
use tilo_ui::permissions::UserId;
use tilo_ui::state::State;
use tilo_ui::widget::{Container, Widget};

const AGENT_ACTION: &str = "test.agent.write";
/// The other party: not the person sitting at this window.
const AGENT: &str = "an-agent-in-the-room";

/// A root widget that owns one document key and answers actions under
/// `AGENT_ACTION` by writing into it — standing in for the app whose registry
/// knows the type. Everything else is delegated to a plain `Container`.
struct AgentTarget {
    inner: Container,
}

impl AgentTarget {
    fn new() -> Self {
        Self {
            inner: Container::new(),
        }
    }
}

impl Widget for AgentTarget {
    fn apply_room_action(
        &mut self,
        doc: &mut tilo_ui::widget::Edits<'_>,
        type_id: &str,
        payload: &[u8],
    ) -> bool {
        if type_id != AGENT_ACTION {
            return false;
        }
        crate::support::application::write_fixture_string(
            doc,
            "agent",
            &String::from_utf8_lossy(payload),
        );
        true
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.inner.sync(state, path);
    }
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &tilo_ui::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        self.inner.build_layout(taffy, theme, node_map)
    }
    fn apply_layout(
        &mut self,
        taffy: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        self.inner.apply_layout(taffy, node, origin);
    }
    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut tilo_ui::TextRenderer,
        theme: &tilo_ui::theme::Theme,
        presence: &tilo_ui::presence::PresenceCtx,
    ) {
        self.inner
            .draw(scene, transform, text_renderer, theme, presence);
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut tilo_ui::widget::EventCtx<'_>,
    ) {
        self.inner.handle_event(event, ctx);
    }
    fn name(&self) -> &'static str {
        "AgentTarget"
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.inner.get_rect()
    }
}

fn typed_app() -> (App, TypedBroker) {
    let broker = TypedBroker::new();
    let app = App::with_collaboration_manager(
        State::new(),
        Box::new(AgentTarget::new()),
        broker.manager("person"),
    );
    broker.wait_action_subscribers(1);
    (app, broker)
}

/// Send the agent's action and pump until `done` reports it landed.
fn deliver_agents_action(app: &mut App, broker: &TypedBroker, done: impl Fn(&mut App) -> bool) {
    broker.inject_action(AGENT, AGENT_ACTION, b"the agent wrote this");
    tilo_test_env::wait_until("the agent's action to land", || {
        app.pump_collab_presence();
        done(app)
    });
}

#[test]
fn an_agent_edit_is_its_own_attributed_undo_boundary() {
    // Case `one_ctrl_z_after_an_agent_edit_reverts_only_the_keystroke`.
    {
        let (mut app, broker) = typed_app();
        let me = app.local_author().clone();
        assert_ne!(me.0, AGENT, "precondition: the agent is somebody else");

        deliver_agents_action(&mut app, &broker, |a| {
            a.state_mut_for_test()
                .doc
                .get(&ROOT, "agent")
                .unwrap()
                .is_some()
        });
        assert!(
            app.state_mut_for_test()
                .doc
                .get(&ROOT, "agent")
                .unwrap()
                .is_some(),
            "precondition: the agent's action reached the document"
        );

        // Then the person types. This is what `App::commit_action` does for a
        // handled keystroke: commit, stamped with who is sitting here.
        {
            let state = app.state_mut_for_test();
            state.doc.put(&ROOT, "typed", "and then I typed").unwrap();
            state.commit_action_authored(None, None, Some(me.clone()));
        }

        // And presses Ctrl+Z once. This is verbatim what the keybinding calls.
        let state = app.state_mut_for_test();
        state
            .undo_as(Some(&me), None, None, None)
            .expect("the keystroke is undoable");

        assert!(
            state.doc.get(&ROOT, "typed").unwrap().is_none(),
            "the person's own keystroke is reverted"
        );
        assert!(
            state.doc.get(&ROOT, "agent").unwrap().is_some(),
            "the agent's edit survives the person's undo — one Ctrl+Z, one action"
        );
    }

    // Case `an_agents_edit_lands_as_its_own_attributed_boundary`.
    // The other half: the agent's edit is a boundary of its own, attributed to
    // the agent — not something unreachable with no entry at all.
    {
        let (mut app, broker) = typed_app();

        deliver_agents_action(&mut app, &broker, |a| {
            a.state_mut_for_test().peek_last_undo().is_some()
        });

        let entry = app
            .state_mut_for_test()
            .peek_last_undo()
            .expect("the agent's action is on the undo stack at all");
        assert_eq!(
            entry.author,
            Some(UserId(AGENT.into())),
            "the boundary names the agent that performed it, not the person here"
        );

        // And it is reversible under that name.
        let state = app.state_mut_for_test();
        state
            .undo_as(Some(&UserId(AGENT.into())), None, None, None)
            .expect("the agent's own action can be reverted as the agent");
        assert!(
            state.doc.get(&ROOT, "agent").unwrap().is_none(),
            "reverting the agent's boundary removes the agent's edit"
        );
    }
}
