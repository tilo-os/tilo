//! Every app opened on a fresh record, the way a person first sees it: an
//! empty state's action button that is drawn must also be in the
//! accessibility tree, as a button with the drawn label where it is drawn,
//! and pressing it there must reach the app. A screen reader, a switch
//! user or an agent reading the tree otherwise sees a blank page whose only
//! way forward is invisible to them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

mod common;

use std::cell::RefCell;
use std::fmt::Write as _;
use std::rc::Rc;

use common::{ROOM, catalogue};
use ui::accesskit;
use ui::application::{ApplicationHost, ComponentRunnerInputs};
use ui::headless::FrameHarness;
use ui::permissions::{PermissionContext, Role, UserId};
use ui::record::{RecordCheckout, RecordId};
use ui::widget::empty_state::{PaintedAction, record_painted_actions, take_painted_actions};

const INSTANCE: u64 = 1;
const PERSON: &str = "empty-state-person";
/// How far a described button's bounds may sit from the drawn one: the
/// tree rounds to whole logical pixels.
const SLACK: f64 = 2.0;

/// `app_id` opened on a fresh record, settled.
fn open(app_id: &str) -> (FrameHarness, tempfile::TempDir) {
    let catalogue = catalogue();
    let definition = catalogue.get(app_id).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let permissions = Rc::new(RefCell::new(PermissionContext::member_of(
        ROOM,
        PERSON,
        Role::Owner,
    )));
    let mut host = ApplicationHost::new(
        directory.path().join("state"),
        "empty-state-device".into(),
        ROOM,
        UserId(PERSON.into()),
        permissions,
        tilo_actions::ActionContext::default(),
    )
    .unwrap();
    let prepared = host
        .prepare_default(
            definition,
            INSTANCE,
            RecordCheckout::new(
                RecordId::window_default(INSTANCE),
                definition.record_kind.clone(),
            ),
            None,
            ComponentRunnerInputs::from_native_process(),
        )
        .unwrap_or_else(|error| panic!("{app_id} did not prepare: {error}"));
    let component = definition
        .instantiate(prepared.context, prepared.target, prepared.restore)
        .unwrap_or_else(|error| panic!("{app_id} did not instantiate: {error}"));
    let mut harness = FrameHarness::from_component(component, (1280, 800), prepared.runner_inputs)
        .unwrap_or_else(|error| panic!("{app_id} did not mount: {error}"));
    harness.wait_until(ui::time::Duration::ZERO, |_| true);
    (harness, directory)
}

/// The tree's button for `action`: its label, where it is drawn.
fn described(
    action: &PaintedAction,
    nodes: &[(accesskit::NodeId, accesskit::Node)],
) -> Option<accesskit::NodeId> {
    nodes.iter().find_map(|(id, node)| {
        (node.role() == accesskit::Role::Button
            && node.label() == Some(action.label.as_str())
            && node.bounds().is_some_and(|bounds| {
                (bounds.x0 - action.rect.x0).abs() <= SLACK
                    && (bounds.y0 - action.rect.y0).abs() <= SLACK
                    && (bounds.x1 - action.rect.x1).abs() <= SLACK
                    && (bounds.y1 - action.rect.y1).abs() <= SLACK
            }))
        .then_some(*id)
    })
}

#[test]
fn every_apps_empty_state_action_is_a_button_in_the_tree() {
    let mut gaps = String::new();
    let mut drawn = 0;
    for definition in catalogue().iter() {
        let app = definition.app_id.as_ref();
        let (mut harness, _directory) = open(app);
        record_painted_actions();
        harness.relayout();
        let painted = take_painted_actions();
        drawn += painted.len();
        let nodes = harness.a11y_nodes();
        let mut pressable = None;
        for action in &painted {
            if let Some(id) = described(action, &nodes) {
                pressable.get_or_insert(id);
                continue;
            }
            let near = nodes
                .iter()
                .filter(|(_, node)| node.label() == Some(action.label.as_str()))
                .map(|(_, node)| format!("{:?} {:?}", node.role(), node.bounds()))
                .collect::<Vec<_>>();
            let _ = writeln!(
                gaps,
                "{app}: \"{}\" drawn at {:?}, described at {near:?}",
                action.label, action.rect
            );
        }
        // Pressed through the tree, the action reaches the app; what it
        // does is the app's own.
        if let Some(id) = pressable
            && let Err(error) = harness.perform_a11y_action(id, &ui::a11y::Action::Activate)
        {
            let _ = writeln!(
                gaps,
                "{app}: pressing its empty-state action failed: {error}"
            );
        }
    }
    assert!(
        gaps.is_empty(),
        "empty-state actions drawn but not described or pressable:\n{gaps}"
    );
    assert!(
        drawn > 0,
        "no app drew an empty-state action on a fresh record"
    );
}
