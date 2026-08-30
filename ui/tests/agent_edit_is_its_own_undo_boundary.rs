//! An agent edits the document; the person then types; the person presses
//! Ctrl+Z once. Exactly one thing must disappear, and it must be theirs.
//!
//! `CollabEvent::ActionArrived` applies a change under somebody else's actor
//! without ever passing through a local commit. Left unrecorded, `last_heads`
//! stays behind that change and the next local edit brackets
//! `[stale_baseline, after]` — a range that *contains* the agent's work — so
//! one Ctrl+Z reverts both.
//!
//! The event reaches the app the way a real one does: down a Unix socket, as
//! the framed JSON the legacy broker speaks, drained by the same
//! `App::pump_collab_presence` a running window calls every frame.

use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ROOT, ReadDoc};

use ui::app::App;
use ui::collaboration::CollabEvent;
use ui::permissions::UserId;
use ui::state::State;
use ui::widget::{Container, Widget};

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
    fn apply_room_action(&mut self, doc: &mut AutoCommit, type_id: &str, payload: &[u8]) -> bool {
        if type_id != AGENT_ACTION {
            return false;
        }
        doc.put(&ROOT, "agent", String::from_utf8_lossy(payload).to_string())
            .expect("write the agent's edit");
        true
    }

    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.inner.sync(state, path);
    }
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &ui::theme::Theme,
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
        text_renderer: &mut ui::TextRenderer,
        theme: &ui::theme::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.inner
            .draw(scene, transform, text_renderer, theme, presence);
    }
    #[allow(clippy::too_many_arguments)]
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<taffy::NodeId>,
        node_id: taffy::NodeId,
        taffy: &taffy::TaffyTree<()>,
    ) {
        self.inner.handle_event(
            event,
            scale_factor,
            modifiers,
            state,
            path,
            handled,
            focus_target,
            node_id,
            taffy,
        );
    }
    fn name(&self) -> &'static str {
        "AgentTarget"
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.inner.get_rect()
    }
}

fn write_frame(s: &mut UnixStream, bytes: &[u8]) {
    let len = bytes.len() as u32;
    s.write_all(&len.to_le_bytes()).expect("frame length");
    s.write_all(bytes).expect("frame body");
    s.flush().expect("flush");
}

/// Stand up an `App` wired to a socket we hold the other end of, so we can
/// hand it any `CollabEvent` the broker could.
fn app_on_a_socket(tag: &str) -> (App, UnixStream) {
    let socket_path =
        std::env::temp_dir().join(format!("quick-undo-seam-{tag}-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path).expect("bind broker socket");

    // SAFETY: the `quick_collab_sock` serial group holds every test that
    // touches this variable, so no other thread reads it meanwhile.
    unsafe {
        std::env::set_var("QUICK_COLLAB_SOCK", &socket_path);
    }

    let app = App::new(State::new(), Box::new(AgentTarget::new()));
    let (broker_side, _) = listener.accept().expect("app connects on construction");

    unsafe {
        std::env::remove_var("QUICK_COLLAB_SOCK");
    }
    let _ = std::fs::remove_file(&socket_path);
    (app, broker_side)
}

/// Send the agent's action and pump until `done` reports it landed.
fn deliver_agents_action(app: &mut App, broker: &mut UnixStream, done: impl Fn(&mut App) -> bool) {
    let event = CollabEvent::ActionArrived {
        type_id: AGENT_ACTION.to_string(),
        payload: b"the agent wrote this".to_vec(),
        from_user_id: AGENT.to_string(),
    };
    write_frame(
        broker,
        &serde_json::to_vec(&event).expect("encode the event"),
    );
    for _ in 0..50 {
        app.pump_collab_presence();
        if done(app) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
#[serial_test::serial(quick_collab_sock)]
fn one_ctrl_z_after_an_agent_edit_reverts_only_the_keystroke() {
    let (mut app, mut broker) = app_on_a_socket("arrived");
    let me = app.local_author().clone();
    assert_ne!(me.0, AGENT, "precondition: the agent is somebody else");

    deliver_agents_action(&mut app, &mut broker, |a| {
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
        .undo_as(Some(&me), None, None)
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

/// The other half: the agent's edit is a boundary of its own, attributed to
/// the agent — not something unreachable with no entry at all.
#[test]
#[serial_test::serial(quick_collab_sock)]
fn an_agents_edit_lands_as_its_own_attributed_boundary() {
    let (mut app, mut broker) = app_on_a_socket("attributed");

    deliver_agents_action(&mut app, &mut broker, |a| {
        a.state_mut_for_test().peek_last_undo().is_some()
    });

    let entry = app
        .state_mut_for_test()
        .peek_last_undo()
        .expect("the agent's action is on the undo stack at all")
        .clone();
    assert_eq!(
        entry.author,
        Some(UserId(AGENT.into())),
        "the boundary names the agent that performed it, not the person here"
    );

    // And it is reversible under that name.
    let state = app.state_mut_for_test();
    state
        .undo_as(Some(&UserId(AGENT.into())), None, None)
        .expect("the agent's own action can be reverted as the agent");
    assert!(
        state.doc.get(&ROOT, "agent").unwrap().is_none(),
        "reverting the agent's boundary removes the agent's edit"
    );
}
