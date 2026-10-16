//! A nonvisual wake must retire on the model pump, without waiting for a paint.

use super::theme_repaint_tests::{layout, paint};
use super::*;
use crate::collaboration::{
    CollabEvent, CollabRequest, CollaborationManager, PeerTransport, PresencePayload,
};
use crate::presence::{PathSegment, PresenceKey};
use std::rc::Rc;

struct QueuedTransport(Rc<RefCell<Vec<CollabEvent>>>);

impl PeerTransport for QueuedTransport {
    fn is_connected(&self) -> bool {
        true
    }
    fn send(&mut self, _: CollabRequest) -> Result<(), String> {
        Ok(())
    }
    fn poll(&mut self) -> Vec<CollabEvent> {
        std::mem::take(&mut *self.0.borrow_mut())
    }
    fn publish_action_directory(&mut self, _: String) -> Result<(), String> {
        Ok(())
    }
}

struct Fixture {
    tile: EmbeddedApp,
    wake: crate::wake::WakeHandle,
    label: Rc<RefCell<String>>,
    wake_during_sync: Rc<Cell<bool>>,
    theme: Theme,
    events: Rc<RefCell<Vec<CollabEvent>>>,
}

impl Fixture {
    fn new() -> Self {
        let scope = crate::wake::WakeScope::new();
        let wake = scope.handle();
        let label = Rc::new(RefCell::new("Before".to_owned()));
        let wake_during_sync = Rc::new(Cell::new(false));
        let source = label.clone();
        let rearm = wake_during_sync.clone();
        let sync_wake = wake.clone();
        let mut app = App::from_scoped_parts_with_controlled_inputs(
            scope,
            crate::state::State::new(),
            Box::new(
                crate::widget::Text::dynamic(move || {
                    if rearm.replace(false) {
                        sync_wake.notify_events();
                    }
                    source.borrow().clone()
                })
                .with_presence(vec![PathSegment::Field("probe".into())]),
            ),
        );
        let events = Rc::new(RefCell::new(Vec::new()));
        app.collaboration.manager =
            CollaborationManager::with_peer_transport(QueuedTransport(events.clone()));
        let mut fixture = Self {
            tile: EmbeddedApp::new(app, |_| {}).unwrap(),
            wake,
            label,
            wake_during_sync,
            theme: Theme::light(),
            events,
        };
        fixture.settle();
        fixture
    }

    fn settle(&mut self) {
        for _ in 0..8 {
            layout(&mut self.tile, &self.theme);
            paint(&mut self.tile, &self.theme);
            if !self.tile.needs_redraw() {
                return;
            }
        }
        panic!("the real embedded app must settle");
    }

    fn pump(&mut self) -> bool {
        let model = automerge::AutoCommit::new();
        self.tile.sync(&model, &automerge::ROOT);
        self.tile.update(&model, &automerge::ROOT)
    }

    fn calls(&self) -> usize {
        self.tile.app.actions.hosted.calls
    }
}

fn painted_glyphs(tile: &EmbeddedApp) -> Vec<u32> {
    tile.painted
        .borrow()
        .encoding()
        .resources
        .glyphs
        .iter()
        .map(|glyph| glyph.id)
        .collect()
}

#[test]
fn an_event_only_wake_is_serviced_once_without_painting() {
    let mut fixture = Fixture::new();
    let before = fixture.calls();
    let painted = fixture.tile.painted_damage.to;
    fixture.wake.notify_events();
    assert!(!fixture.pump(), "a queue-only wake changes no pixels");
    assert_eq!(fixture.calls(), before + 1);
    for _ in 0..128 {
        assert!(!fixture.pump());
    }
    assert_eq!(fixture.calls(), before + 1, "the answered wake must retire");
    assert!(!fixture.tile.needs_redraw());
    assert_eq!(fixture.tile.painted_damage.to, painted);

    // Consuming a model wake must not consume the later visual change.
    let previous = painted_glyphs(&fixture.tile);
    *fixture.label.borrow_mut() = "After".to_owned();
    fixture.wake.wake();
    assert!(fixture.pump(), "the model change still requests a frame");
    assert!(fixture.tile.needs_redraw());
    assert!(
        !fixture.pump(),
        "a deferred paint need not repeat model work"
    );
    assert!(
        fixture.tile.needs_redraw(),
        "the frame stays pending until drawn"
    );
    paint(&mut fixture.tile, &fixture.theme);
    assert_ne!(
        painted_glyphs(&fixture.tile),
        previous,
        "the new label is actually painted"
    );
    fixture.settle();
    let settled = fixture.calls();
    for _ in 0..128 {
        assert!(!fixture.pump());
    }
    assert_eq!(fixture.calls(), settled);
}

#[test]
fn queued_presence_arrival_and_departure_repaint_without_widget_dirtiness() {
    let mut fixture = Fixture::new();
    let initial = paint(&mut fixture.tile, &fixture.theme);
    for present in [true, false] {
        fixture.events.borrow_mut().push(CollabEvent::Context {
            collaborators: if present {
                vec![crate::collaboration::Collaborator {
                    id: "peer".into(),
                    name: "Peer".into(),
                    colour: None,
                    is_agent: false,
                    is_self: false,
                }]
            } else {
                Vec::new()
            },
            permissions: Vec::new(),
        });
        if present {
            fixture
                .events
                .borrow_mut()
                .push(CollabEvent::RemotePresenceUpdate {
                    user_id: "peer".into(),
                    key: PresenceKey::Widget {
                        path: vec![PathSegment::Field("probe".into())],
                    },
                    payload: PresencePayload::Arrived,
                });
        }
        fixture.wake.notify_events();
        assert!(fixture.pump(), "presence changes the embedded scene");
        assert!(fixture.tile.needs_redraw());
        assert!(!fixture.pump());
        assert!(
            fixture.tile.needs_redraw(),
            "presence survives a deferred paint"
        );
        let current = paint(&mut fixture.tile, &fixture.theme);
        if present {
            assert_ne!(current, initial, "the peer focus ring is actually painted");
        } else {
            assert_eq!(current, initial, "the departed peer's ring is removed");
        }
        fixture.settle();
    }
}

#[test]
fn a_wake_raised_during_the_model_pump_is_not_lost() {
    let mut fixture = Fixture::new();
    fixture.wake_during_sync.set(true);
    fixture.wake.wake();
    let before = fixture.calls();
    fixture.pump();
    assert_eq!(fixture.calls(), before + 1);
    assert!(
        fixture
            .tile
            .woken
            .load(std::sync::atomic::Ordering::Acquire)
    );
    fixture.pump();
    assert_eq!(fixture.calls(), before + 2);
    fixture.settle();
    let settled = fixture.calls();
    for _ in 0..128 {
        assert!(!fixture.pump());
    }
    assert_eq!(fixture.calls(), settled);
}
