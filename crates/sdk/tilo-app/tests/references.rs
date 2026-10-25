#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! A reference shows as a chip that opens it where it lives: one action,
//! the resource's `open` in the app that keeps it, sent through the session's
//! action door and admitted there like any other.

use std::sync::{Arc, Mutex};

use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending, ReceiptStatus,
};
use tilo_app::RemoteActions;
use tilo_app::a11y::Action as A11yAction;
use tilo_app::prelude::*;
use tilo_reference::ResourceRef;

const ROOM: &str = "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a91";
const TASKS: &str = "so.tilo.tasks";

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Notes {
    pub title: String,
}

#[resource(kind = "test.reference-notes", label = "Notes")]
impl Notes {}

fn issue() -> ResourceRef {
    ResourceRef::whole(
        "tasks.tracker.issues",
        "tasks.tracker/issues/a",
        "#12 Fix the door",
    )
}

struct NotesApp;

impl ResourceApp for NotesApp {
    type Resource = Notes;
    const ID: &'static str = "so.tilo.test-reference-notes";
    const TITLE: &'static str = "Notes";

    fn view(cx: &mut ViewCx<Notes>) -> Element {
        Box::new(Column::new().push(Box::new(cx.reference(TASKS, None, &issue()))))
    }
}

/// The session's door: records what it is asked and refuses with
/// `refusal`, else opens.
#[derive(Default)]
struct Door {
    refusal: Option<ActionHostError>,
    asked: Mutex<Vec<InvokeRequest>>,
}

impl ActionHost for Door {
    fn query(
        &self,
        _: &Caller,
        _: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        Pending::ready(Err(ActionHostError::NotPermitted))
    }

    fn invoke(
        &self,
        _: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let invocation = request.invocation;
        self.asked.lock().unwrap().push(request);
        Pending::ready(match &self.refusal {
            Some(refusal) => Err(refusal.clone()),
            None => Ok(InvocationReceipt {
                invocation,
                status: ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput::default(),
                },
            }),
        })
    }

    fn observe(&self, _: &Caller, _: &tilo_actions::InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(
        &self,
        _: &Caller,
        _: &tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionHostError>> {
        tilo_actions::remote::Pending::ready(Ok(()))
    }
}

fn open_chip(door: &Arc<Door>) -> TestApp<NotesApp> {
    let mut app = TestApp::<NotesApp>::new().unwrap();
    let chip = app
        .harness()
        .a11y_node_named("Open #12 Fix the door")
        .expect("the reference reads as what it opens");
    app.harness()
        .perform_a11y_action(chip, &A11yAction::Activate)
        .unwrap();
    app.settle();
    assert_eq!(door.asked.lock().unwrap().len(), 1, "one open per press");
    app
}

#[test]
fn a_reference_chip_opens_its_resource_in_its_own_app_and_tells_a_refusal() {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_WORKSPACE_ID", ROOM);

    let door = Arc::new(Door::default());
    let _served = RemoteActions::enter(door.clone(), Caller::local_person("ada"));
    let mut app = open_chip(&door);
    let asked = door.asked.lock().unwrap().pop().unwrap();
    assert_eq!(asked.target, issue().open_target(ROOM, TASKS, None));
    assert_eq!(
        tilo_actions::remote::application_provider(&asked.target.provider),
        Some((ROOM, TASKS)),
        "the app that keeps it, in this window's workspace"
    );
    assert_eq!(asked.target.address.action, "open");
    assert_eq!(app.refused(), None);

    let door = Arc::new(Door {
        refusal: Some(ActionHostError::NotPermitted),
        ..Door::default()
    });
    let _served = RemoteActions::enter(door.clone(), Caller::local_person("ada"));
    let mut app = open_chip(&door);
    let refused = app.refused().expect("the refusal is told");
    assert_eq!(refused.action, "open");
    assert_eq!(refused.reason, ActionHostError::NotPermitted.to_string());
}
