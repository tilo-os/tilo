//! A window's session opens another app for it: the press is the app's own call, handed to its
//! broker, and the session the window was given is asked to open the other app, once. Its
//! receipt finishes the press. The session is a provider in this process, its calls answered as
//! the runner that starts an app answers them.

use std::collections::HashMap;

use ::collaboration::CollabRequest;
use tilo_ui_base::taffy::{self, NodeId, TaffyTree};
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::{ResourceOperationStart, TaffyTreeExt as _, Widget};
use ui_core::headless::Driver;
use ui_core::state::State;
use ui_core::tilo_actions;

use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate, ResourceSpec};

/// A window that publishes `open-photos`, which opens Photos in the window's workspace.
#[derive(Default)]
struct Worker {
    rect: tilo_ui_base::kurbo::Rect,
}

impl Widget for Worker {
    tilo_ui_base::provides!(
        dyn ui_core::widget::resources::PublishesResources,
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
        "Worker"
    }
}

impl ui_core::AppWidget for Worker {
    fn app_kind(&self) -> ui_core::AppKind {
        ui_core::AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Worker"
    }
    fn app_icon(&self) -> ui_core::IconDescriptor {
        ui_core::IconDescriptor::Glyph("W")
    }
}

impl ui_core::widget::resources::PublishesResources for Worker {
    fn resources(&self, _doc: &automerge::AutoCommit) -> Vec<ResourceSpec> {
        vec![
            ResourceSpec::new("test.worker", "test.worker", "Worker").with_action(photos().spec(
                "open-photos",
                "Open Photos",
                "Open Photos beside this window",
            )),
        ]
    }
}

impl tilo_ui_base::widget::ResourceOperations for Worker {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        // Opening another app is the host's to route to the broker, as it is for every app.
        &[collab_types::app_actions::OPEN_APP_OPERATION]
    }

    fn start_operation(
        &mut self,
        _op: &tilo_ui_base::widget::ResourceOperation<'_>,
        _doc: &mut tilo_ui_base::widget::Edits<'_>,
        _path: &automerge::ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        Ok(None)
    }

    fn poll_resource_operation(
        &mut self,
        _invocation: InvocationId,
        _cancellation_requested: bool,
        _doc: &automerge::AutoCommit,
        _path: &automerge::ObjId,
    ) -> Option<InvocationUpdate> {
        None
    }
}

/// What `open-photos` launches.
fn photos() -> collab_types::app_actions::ApplicationLaunch {
    collab_types::app_actions::ApplicationLaunch::app("so.tilo.photos")
}

fn worker_driver(worker: Worker) -> Driver {
    let mut driver = Driver::new(State::new(), Box::new(worker), (400, 300));
    driver.connect_peer();
    driver
}

fn address(action: &str) -> ActionAddress {
    ActionAddress::new("test.worker", action)
}

fn no_arguments() -> serde_json::Map<String, serde_json::Value> {
    serde_json::Map::new()
}

/// The invocations of its own actions the app handed its broker, oldest first.
fn own_invocations(driver: &Driver) -> Vec<tilo_actions::remote::InvokeRequest> {
    driver
        .peer_requests()
        .into_iter()
        .filter_map(|request| match request {
            CollabRequest::RemoteAction {
                message: tilo_actions::remote::RemoteActionMessage::Invoke(request),
                ..
            } if request.target.provider == tilo_actions::remote::OWN_PROVIDER => Some(request),
            _ => None,
        })
        .collect()
}

#[test]
fn the_window_s_session_opens_another_app_and_its_receipt_comes_back() {
    tilo_app_test_host::answer_calls_in_process();
    let mut driver = worker_driver(Worker::default());
    let workspace = uuid::Uuid::new_v4();
    let session = std::sync::Arc::new(tilo_actions::remote::TestProvider::new());
    driver.set_session_api(
        ui_core::RemoteActions::with_provider(
            session.clone(),
            tilo_actions::remote::Caller::local_app("test.worker")
                .in_workspace(workspace.to_string()),
        ),
        Some(workspace.to_string()),
    );
    driver
        .invoke_action(&address("open-photos"), &no_arguments(), None)
        .expect("the window takes the press");
    driver.idle();
    let asked = session.take_asked();
    assert_eq!(asked.len(), 1, "the session is asked to open Photos once");
    assert_eq!(asked[0].target, photos().target(&workspace.to_string()));
    let [own] = own_invocations(&driver)
        .try_into()
        .unwrap_or_else(|asked: Vec<_>| panic!("the press went to the broker once: {asked:?}"));
    assert!(
        matches!(
            driver.job_updates(own.invocation).as_slice(),
            [InvocationUpdate::Succeeded { .. }]
        ),
        "the session's receipt finished the press: {:?}",
        driver.job_updates(own.invocation)
    );
    assert_eq!(driver.refusal_said(), None);
}
