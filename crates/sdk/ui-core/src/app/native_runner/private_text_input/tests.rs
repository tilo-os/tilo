//! Production App binding and native dispatch; renderer replies are controlled.
use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::webview::text_input::Resolver;
use crate::widget::{Column, Decorator, TextInput};

#[derive(Default)]
struct Calls {
    starts: Vec<(Receipt, Resolver)>,
    finals: Vec<(String, Receipt, Resolver)>,
}
struct Deferred {
    inner: TextInput,
    calls: Rc<RefCell<Calls>>,
}
impl Decorator for Deferred {
    type Inner = TextInput;
    fn inner(&self) -> &TextInput {
        &self.inner
    }
    fn inner_mut(&mut self) -> &mut TextInput {
        &mut self.inner
    }
    fn ime_requires_deferred_ack(&self) -> bool {
        true
    }
    fn start_deferred_text(&mut self) -> Option<Receipt> {
        let (receipt, resolver) = Receipt::channel();
        self.calls
            .borrow_mut()
            .starts
            .push((receipt.clone(), resolver));
        Some(receipt)
    }
    fn commit_deferred_text(&mut self, text: &str) -> Option<Receipt> {
        let (receipt, resolver) = Receipt::channel();
        self.calls
            .borrow_mut()
            .finals
            .push((text.into(), receipt.clone(), resolver));
        Some(receipt)
    }
}
#[derive(Default)]
struct Wire {
    published: RefCell<Vec<Option<[u8; 32]>>>,
    acknowledgments: RefCell<Vec<(u32, u32)>>,
    redraws: RefCell<usize>,
    unavailable: bool,
}
impl Transport for Wire {
    fn publish(&self, snapshot: Option<[u8; 32]>) -> bool {
        self.published.borrow_mut().push(snapshot);
        !self.unavailable
    }
    fn acknowledge(&self, serial: u32, outcome: u32) {
        self.acknowledgments.borrow_mut().push((serial, outcome));
    }
    fn redraw(&self) {
        let mut redraws = self.redraws.borrow_mut();
        *redraws = redraws.saturating_add(1);
    }
}
struct Fixture {
    app: App,
    bridge: Bridge,
    wire: Wire,
    nodes: Vec<NodeId>,
    calls: Vec<Rc<RefCell<Calls>>>,
}
impl Fixture {
    fn new(secret: bool, disabled: bool) -> Self {
        let calls = vec![
            Rc::new(RefCell::new(Calls::default())),
            Rc::new(RefCell::new(Calls::default())),
        ];
        let mut column = Column::new();
        for (index, calls) in calls.iter().enumerate() {
            column = column.push(Box::new(Deferred {
                inner: TextInput::local()
                    .with_id(&format!("field-{index}"))
                    .with_secret(secret)
                    .disabled(disabled),
                calls: calls.clone(),
            }));
        }
        let mut app = App::new_with_controlled_inputs(crate::State::new(), Box::new(column));
        app.init_layout();
        let root = app.layout.root_node.expect("production root layout");
        crate::app::run_layout_pass(&mut app, root, 800, 600, 1.0, false, None)
            .expect("production App layout");
        let nodes = (0..2)
            .map(|index| {
                let id = format!("field-{index}");
                app.layout
                    .node_map
                    .keys()
                    .copied()
                    .find(|node| {
                        app.inspect_node(*node, |widget| widget.test_id() == Some(id.as_str()))
                            .unwrap_or(false)
                    })
                    .expect("bound shared control")
            })
            .collect::<Vec<_>>();
        app.set_focused_node(Some(nodes[0]));
        let mut fixture = Self {
            app,
            bridge: Bridge::default(),
            wire: Wire::default(),
            nodes,
            calls,
        };
        fixture.service();
        fixture
    }
    fn service(&mut self) {
        self.bridge.service(&self.app, &self.wire, true);
    }
    fn operation(&mut self, serial: u32, kind: PrivateImeKind, stamp: [u8; 32]) {
        assert!(self.bridge.event(
            &mut self.app,
            &self.wire,
            &WindowEvent::PrivateIme(PrivateIme {
                serial,
                kind,
                snapshot: stamp
            })
        ));
    }
    fn start(&mut self) -> [u8; 32] {
        let stamp = self.bridge.advertised.expect("eligible field").stamp;
        self.operation(1, PrivateImeKind::Prepare, stamp);
        self.operation(2, PrivateImeKind::Start, stamp);
        assert_eq!(self.wire.acknowledgments.borrow().as_slice(), &[(1, 0)]);
        let (_, resolver) = self.calls[0]
            .borrow_mut()
            .starts
            .pop()
            .expect("actual bound start callback");
        resolver.complete(Ok(None));
        self.service();
        assert_eq!(
            self.wire.acknowledgments.borrow().as_slice(),
            &[(1, 0), (2, 0)]
        );
        stamp
    }
}

#[test]
fn original_final_is_private_and_ack_waits_for_renderer_outcome() {
    let mut f = Fixture::new(false, false);
    let stamp = f.start();
    f.operation(3, PrivateImeKind::Final, stamp);
    assert!(f.bridge.event(
        &mut f.app,
        &f.wire,
        &WindowEvent::Ime(Ime::Commit("日本語 café العربية".into()))
    ));
    assert_eq!(
        f.wire.acknowledgments.borrow().as_slice(),
        &[(1, 0), (2, 0)]
    );
    let (text, _, resolver) = f.calls[0]
        .borrow_mut()
        .finals
        .pop()
        .expect("original final callback");
    assert_eq!(text, "日本語 café العربية");
    assert!(f.calls[1].borrow().finals.is_empty());
    resolver.complete(Err(Failure::Unknown));
    f.service();
    assert_eq!(f.wire.acknowledgments.borrow().last(), Some(&(3, 2)));
    assert!(!f.bridge.admitted());
}

#[test]
fn focus_move_after_final_metadata_consumes_text_without_successor_dispatch() {
    let mut f = Fixture::new(false, false);
    let stamp = f.start();
    f.operation(3, PrivateImeKind::Final, stamp);
    f.app.set_focused_node(Some(f.nodes[1]));
    f.service();
    assert!(f.bridge.event(
        &mut f.app,
        &f.wire,
        &WindowEvent::Ime(Ime::Commit("must not move".into()))
    ));
    assert!(f.calls.iter().all(|calls| calls.borrow().finals.is_empty()));
    assert_eq!(f.wire.acknowledgments.borrow().last(), Some(&(3, 1)));
    assert_eq!(f.app.focused_node, Some(f.nodes[1]));
}

#[test]
fn stale_start_does_not_acquire_successor_at_same_stamp_lifetime() {
    let mut f = Fixture::new(false, false);
    let stamp = f.bridge.advertised.expect("first field").stamp;
    f.operation(1, PrivateImeKind::Prepare, stamp);
    f.app.set_focused_node(Some(f.nodes[1]));
    f.operation(2, PrivateImeKind::Start, stamp);
    assert!(f.calls.iter().all(|calls| calls.borrow().starts.is_empty()));
    assert_eq!(f.wire.acknowledgments.borrow().last(), Some(&(2, 1)));
}

#[test]
fn late_cancel_cannot_revoke_repeated_session_and_unmarked_commit_is_consumed() {
    let mut f = Fixture::new(false, false);
    let stamp = f.start();
    f.operation(2, PrivateImeKind::Cancel, stamp);
    f.operation(4, PrivateImeKind::Prepare, stamp);
    f.operation(2, PrivateImeKind::Cancel, stamp);
    assert!(f.bridge.admitted());
    assert!(f.bridge.event(
        &mut f.app,
        &f.wire,
        &WindowEvent::Ime(Ime::Commit("unmarked".into()))
    ));
    assert!(f.calls.iter().all(|calls| calls.borrow().finals.is_empty()));
    f.operation(5, PrivateImeKind::Start, stamp);
    assert_eq!(f.calls[0].borrow().starts.len(), 1);
}

#[test]
fn protected_disabled_or_missing_protocol_fields_never_advertise_voice_admission() {
    for (secret, disabled) in [(true, false), (false, true)] {
        let f = Fixture::new(secret, disabled);
        assert!(f.bridge.advertised.is_none());
        assert!(!f.bridge.admitted());
    }
    let mut f = Fixture::new(false, false);
    f.wire.unavailable = true;
    f.bridge.advertised = None;
    f.service();
    assert!(f.bridge.advertised.is_none());
    assert!(!f.bridge.admitted());
}

#[test]
fn preparation_lease_is_not_a_successful_start_acknowledgment() {
    let mut f = Fixture::new(false, false);
    let stamp = f.bridge.advertised.expect("eligible field").stamp;
    f.operation(1, PrivateImeKind::Prepare, stamp);
    f.operation(2, PrivateImeKind::Start, stamp);
    let (_, resolver) = f.calls[0]
        .borrow_mut()
        .starts
        .pop()
        .expect("original start");
    let lease = crate::webview::text_input::Lease::parse("00000000000000000000000000000000")
        .expect("synthetic preparation lease");
    resolver.complete(Ok(Some(lease)));
    f.service();
    assert_eq!(f.wire.acknowledgments.borrow().last(), Some(&(2, 2)));
    assert!(!f.bridge.admitted());
}
