//! A step's argument that takes a resource names it by its published address: the window answers
//! it with the reference the action's parameter asks for, so a plan writes no identity it cannot
//! know — a development build's app id is the run's, not its own.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixture: a failure is the assertion"
)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, TaffyTree};
use tilo_actions::{ActionOutput, ActionParam, ParamType, Pick, ResourceSpec};
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::TaffyTreeExt as _;
use tilo_ui_base::widget::{ResourceOperationStart, Widget};

use crate::headless::Driver;
use crate::state::State;

/// A window publishing a project, and a task action that takes one as `p`.
struct Projects {
    seen: Rc<RefCell<Vec<serde_json::Value>>>,
    rect: vello::kurbo::Rect,
}

impl Widget for Projects {
    tilo_ui_base::provides!(
        dyn crate::widget::resources::PublishesResources,
        dyn tilo_ui_base::widget::ResourceOperations
    );
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_node(taffy::Style::default(), &[], node_map, self)
    }
    tilo_ui_base::rect_geometry!();
    fn name(&self) -> &'static str {
        "Projects"
    }
}

impl crate::widget::resources::PublishesResources for Projects {
    fn resources(&self, _doc: &automerge::AutoCommit) -> Vec<ResourceSpec> {
        let mut project = ResourceSpec::new("project:root", "test.project", "A project");
        project.app_id = Some("test.projects".to_owned());
        let file = crate::widget::resource_actions::typed_resource(
            "tasks",
            "Tasks",
            "file",
            "File",
            vec![
                ActionParam::required("p", ParamType::Object, "The project").picking(
                    Pick::Resource {
                        kind: "test.project".into(),
                    },
                ),
            ],
        );
        vec![project, file]
    }
}

impl tilo_ui_base::widget::ResourceOperations for Projects {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::widget::resource_actions::OPERATION_TYPE]
    }

    fn start_operation(
        &mut self,
        op: &tilo_ui_base::widget::ResourceOperation<'_>,
        _doc: &mut tilo_ui_base::widget::Edits<'_>,
        _path: &automerge::ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        let request = op.request();
        if request.type_id != crate::widget::resource_actions::OPERATION_TYPE {
            return Ok(None);
        }
        let arguments = serde_json::from_slice(request.payload).unwrap_or(serde_json::Value::Null);
        self.seen.borrow_mut().push(arguments);
        Ok(Some(ResourceOperationStart::Completed(
            ActionOutput::default(),
        )))
    }
}

fn projects() -> (Driver, Rc<RefCell<Vec<serde_json::Value>>>) {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let root = Projects {
        seen: Rc::clone(&seen),
        rect: vello::kurbo::Rect::ZERO,
    };
    let mut driver = Driver::new(State::new(), Box::new(root), (400, 300));
    driver.relayout();
    (driver, seen)
}

/// The one step `payload` names, as a drive runs it.
fn step(payload: &str) -> crate::headless::Step {
    let spec = crate::headless::parse_spec(payload).expect("the step parses");
    crate::headless::steps_of(&spec)
        .into_iter()
        .next()
        .expect("one step")
}

// USER REQUIREMENT: a plan names a resource argument by its published address — the address a
// bound answer handed it — and the window passes the reference its parameter asks for.
#[test]
fn a_resource_argument_is_named_by_its_address_and_passed_as_its_reference() {
    let (mut driver, seen) = projects();
    let mut bindings = crate::headless::Bindings::new();
    let file = step(
        r#"{"actions":[{"invoke":{"resource":"tasks:root","action":"file","args":{"p":{"resource":"project:root"}}}}]}"#,
    );
    driver
        .perform_bound(&file, &mut bindings)
        .expect("the action runs with the project to file into");
    let arguments = seen.borrow();
    let [arguments] = arguments.as_slice() else {
        panic!("one call ran: {arguments:?}");
    };
    assert_eq!(
        arguments.get("p"),
        Some(&serde_json::json!({
            "app": "test.projects",
            "kind": "test.project",
            "id": "project:root",
            "label": "A project",
        })),
        "the reference the parameter takes, as the window publishes it"
    );
}

// USER REQUIREMENT: an address the window publishes nothing of fails the step, naming the
// argument and the address, rather than reaching the action as something else.
#[test]
fn a_resource_argument_the_window_publishes_nothing_of_fails_the_step() {
    let (mut driver, seen) = projects();
    let mut bindings = crate::headless::Bindings::new();
    let file = step(
        r#"{"actions":[{"invoke":{"resource":"tasks:root","action":"file","args":{"p":{"resource":"project:gone"}}}}]}"#,
    );
    let error = driver
        .perform_bound(&file, &mut bindings)
        .expect_err("a resource nothing publishes fails the step");
    assert!(
        error.contains("`p`") && error.contains("project:gone"),
        "the refusal names the argument and the address: {error}"
    );
    assert!(seen.borrow().is_empty(), "the action did not run");
}
