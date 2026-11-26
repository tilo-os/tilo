// Included in `semantic_tree_app_tests`, beside the counter it drives: a call the workspace's
// broker delivers to an app shown by the display-less driver runs on the app's own collaboration
// path, for the invoker it was delivered for, and the app answers its peer as it answers the
// broker: at once for an effect, as a job's progress and outcome for an operation that runs on,
// and with what it ended as when its caller cancels it.

fn driven_counter(card: CounterCard) -> crate::headless::Driver {
    let mut driver = crate::headless::Driver::new(State::new(), Box::new(card), (400, 300));
    driver
        .action_registry_mut()
        .register::<CounterAction>(COUNTER_TYPE);
    driver.connect_peer();
    driver
}

fn caller() -> tilo_actions::Actor {
    tilo_actions::Actor::Member(tilo_actions::MemberId::person("caller"))
}

#[test]
fn a_delivered_call_runs_for_its_invoker_and_answers_the_peer() {
    let mut driver = driven_counter(CounterCard::new());
    let invocation = driver.deliver_call(
        "test.counter",
        "bump",
        &serde_json::json!({}),
        caller(),
        None,
    );
    assert_eq!(
        counter_in_doc(&driver.state().doc),
        1,
        "the call ran: publication {:?}, peer heard {:?}",
        driver.app().action_publication_failure,
        driver.peer_requests()
    );
    assert!(
        matches!(
            driver.call_updates(invocation).as_slice(),
            [tilo_actions::InvocationUpdate::Succeeded { .. }]
        ),
        "its caller is answered once: {:?}",
        driver.call_updates(invocation)
    );
    assert!(driver.call_failures().is_empty());
}

#[test]
fn a_delivered_job_answers_when_it_finishes() {
    let mut driver = driven_counter(CounterCard::with_async_post_bump_reset());
    let invocation = driver.deliver_call(
        "test.counter",
        "async-bump",
        &serde_json::json!({}),
        caller(),
        None,
    );
    assert!(
        driver.wait_until(crate::time::Duration::from_secs(5), |driver| {
            driver
                .call_updates(invocation)
                .iter()
                .any(tilo_actions::InvocationUpdate::is_terminal)
        }),
        "the job finishes and answers: {:?}",
        driver.call_updates(invocation)
    );
    // Frames after the job's end run nothing again and answer nothing more.
    driver.idle();
    assert_eq!(counter_in_doc(&driver.state().doc), 1, "the job ran once");
    let updates = driver.call_updates(invocation);
    assert_eq!(
        updates
            .iter()
            .filter(|update| matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }))
            .count(),
        1,
        "one completion answers the job: {updates:?}"
    );
}

#[test]
fn a_call_the_app_cannot_run_fails_legibly_to_its_caller() {
    let mut driver = driven_counter(CounterCard::new());
    let invocation = driver.deliver_call(
        "test.counter",
        "missing",
        &serde_json::json!({}),
        caller(),
        None,
    );
    assert!(
        matches!(
            driver.call_updates(invocation).as_slice(),
            [tilo_actions::InvocationUpdate::Failed { .. }]
        ),
        "{:?}",
        driver.call_updates(invocation)
    );
    assert_eq!(
        driver.call_failures().len(),
        1,
        "{:?}",
        driver.call_failures()
    );
    assert_eq!(counter_in_doc(&driver.state().doc), 0, "nothing ran");
}

const WAITER_TYPE: &str = "test.waiter";

/// A resource whose one operation runs until its caller cancels it.
#[derive(Default)]
struct Waiter {
    rect: vello::kurbo::Rect,
    running: Vec<tilo_actions::InvocationId>,
}

impl Widget for Waiter {
    tilo_ui_base::provides!(
        dyn tilo_ui_base::widget::resources::PublishesResources,
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
        "Waiter"
    }
}

impl tilo_ui_base::widget::resources::PublishesResources for Waiter {
    fn resources(&self, _doc: &automerge::AutoCommit) -> Vec<ResourceSpec> {
        vec![
            ResourceSpec::new("test.waiter", "test.waiter", "Waiter").with_action(
                ActionSpec::operation("wait", "Wait", OperationKind::Effect, WAITER_TYPE, &())
                    .unwrap()
                    .execution(tilo_actions::ExecutionMode::Background { cancellable: true })
                    .returns(tilo_actions::ResultSpec::Job),
            ),
        ]
    }
}

impl tilo_ui_base::widget::ResourceOperations for Waiter {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[WAITER_TYPE]
    }

    fn start_operation(
        &mut self,
        op: &tilo_ui_base::widget::ResourceOperation<'_>,
        _doc: &mut tilo_ui_base::widget::Edits<'_>,
        _path: &automerge::ObjId,
    ) -> Result<Option<tilo_ui_base::widget::ResourceOperationStart>, String> {
        self.running.push(op.invocation());
        Ok(Some(tilo_ui_base::widget::ResourceOperationStart::Accepted))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        _doc: &automerge::AutoCommit,
        _path: &automerge::ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        let at = self
            .running
            .iter()
            .position(|running| *running == invocation)?;
        cancellation_requested.then(|| {
            self.running.remove(at);
            tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("stopped".into()),
            }
        })
    }
}

#[test]
fn a_cancelled_job_stops_and_says_so_to_its_caller() {
    let mut driver =
        crate::headless::Driver::new(State::new(), Box::new(Waiter::default()), (400, 300));
    driver.connect_peer();
    let invocation = driver.deliver_call(
        "test.waiter",
        "wait",
        &serde_json::json!({}),
        caller(),
        None,
    );
    driver.idle();
    assert!(
        driver
            .call_updates(invocation)
            .iter()
            .all(|update| !update.is_terminal()),
        "the job runs on until it is cancelled: {:?}",
        driver.call_updates(invocation)
    );
    driver.cancel_call(invocation);
    driver.idle();
    assert!(
        matches!(
            driver.call_updates(invocation).last(),
            Some(tilo_actions::InvocationUpdate::Cancelled { .. })
        ),
        "the caller hears it stopped: {:?}",
        driver.call_updates(invocation)
    );
    assert_eq!(driver.call_failures(), ["cancelled"]);
}
