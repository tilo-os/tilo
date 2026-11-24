//! A reference shows as a chip that opens it where it lives: one action,
//! the resource's `open` in the app that keeps it, sent through the session's
//! action API and admitted there like any other.

use std::sync::{Arc, Mutex};
use tilo_app_test_host::TestApp;

use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending, ReceiptStatus,
};
use tilo_app::RemoteActions;
use tilo_app::a11y::Action as A11yAction;
use tilo_app::prelude::*;
use tilo_reference::ResourceRef;

const WORKSPACE: &str = "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a91";
const TASKS: &str = "so.tilo.tasks";

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Notes {
    pub title: String,
}

#[resource(kind = "test.reference-notes", catalogue = "tests/locales")]
impl Notes {}

fn issue() -> ResourceRef {
    ResourceRef::whole(
        "tasks.tracker.issues",
        "tasks.tracker/issues/a",
        "#12 Fix the API",
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

/// The session's API: records what it is asked and refuses with
/// `refusal`, else opens.
#[derive(Default)]
struct Api {
    refusal: Option<ActionCallError>,
    asked: Mutex<Vec<InvokeRequest>>,
}

impl ActionProvider for Api {
    fn query(
        &self,
        _: &Caller,
        _: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Err(ActionCallError::NotPermitted))
    }

    fn invoke(
        &self,
        _: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
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
    ) -> tilo_actions::remote::Pending<Result<(), ActionCallError>> {
        tilo_actions::remote::Pending::ready(Ok(()))
    }
}

fn open_chip(api: &Arc<Api>) -> TestApp<NotesApp> {
    let mut app = TestApp::<NotesApp>::new().unwrap();
    let chip = app
        .harness()
        .a11y_node_named("Open #12 Fix the API")
        .expect("the reference reads as what it opens");
    app.harness()
        .perform_a11y_action(chip, &A11yAction::Activate)
        .unwrap();
    app.settle();
    assert_eq!(api.asked.lock().unwrap().len(), 1, "one open per press");
    app
}

#[test]
fn a_reference_chip_opens_its_resource_in_its_own_app_and_tells_a_refusal() {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_WORKSPACE_ID", WORKSPACE);

    let api = Arc::new(Api::default());
    let _served = RemoteActions::enter(api.clone(), Caller::local_person("ada"));
    let mut app = open_chip(&api);
    let asked = api.asked.lock().unwrap().pop().unwrap();
    assert_eq!(asked.target, issue().open_target(WORKSPACE, TASKS, None));
    assert_eq!(
        tilo_actions::remote::application_provider(&asked.target.provider),
        Some((WORKSPACE, TASKS)),
        "the app that keeps it, in this window's workspace"
    );
    assert_eq!(asked.target.address.action, "open");
    assert_eq!(app.refused(), None);

    let api = Arc::new(Api {
        refusal: Some(ActionCallError::NotPermitted),
        ..Api::default()
    });
    let _served = RemoteActions::enter(api.clone(), Caller::local_person("ada"));
    let mut app = open_chip(&api);
    let refused = app.refused().expect("the refusal is told");
    assert_eq!(refused.action, "open");
    assert_eq!(refused.reason, ActionCallError::NotPermitted.to_string());
}
