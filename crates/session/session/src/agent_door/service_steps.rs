//! The session's services' steps, as the session's door answers them on its pass: an action a
//! service publishes, invoked as the agent or person that asked and as a step of the run it names,
//! and a change of a record a service keeps for a package the session runs itself.

use tilo_actions::remote::ActionHostError;

/// The typed arguments a step's slot carries: none for none. A slot that is not a typed object
/// names no argument of a session action, which all take their arguments by name.
pub(super) fn arguments_of(
    slot: Option<&str>,
) -> Result<serde_json::Map<String, serde_json::Value>, ActionHostError> {
    let Some(slot) = slot else {
        return Ok(serde_json::Map::new());
    };
    let invalid = |reason: String| ActionHostError::InvalidRequest { reason };
    let json = slot
        .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
        .ok_or_else(|| invalid("a session action takes its arguments by name".into()))?;
    serde_json::from_str(json)
        .map_err(|error| invalid(format!("the step's arguments do not read: {error}")))
}

/// `arguments` of the system's `open`, with the place the asking turn is in filled in when it names
/// none: a place it opens stands beside the conversation of the turn
/// (`docs/designs/app-language/one-app-model.md` §3). Every other action's arguments stand as they
/// were.
pub(super) fn with_turn_beside(
    address: &tilo_actions::ActionAddress,
    arguments: &serde_json::Map<String, serde_json::Value>,
    beside: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let system_open = address.resource == crate::arrangement::RESOURCE && address.action == "open";
    if !system_open || beside.is_empty() || arguments.contains_key("beside") {
        return arguments.clone();
    }
    let mut arguments = arguments.clone();
    arguments.insert("beside".into(), beside.into());
    arguments
}

/// Run `address` as `actor` through the session's services, which publish the actions its own
/// doors do not: what a service keeps (Chat's `record-work` on a turn's summons, the arrangement's
/// place read from a service) is invoked on the same pass, on the record the service writes. The
/// `actor` an agent names is `agent:<id>`, attributed as it is at an app's record origin; a person
/// acts as themselves. A step of a `run` is that run's, as an app's action is. `beside` is the
/// place the ask is in — the conversation of the turn an agent asks in — or empty for the place in
/// front: a resource the step opens stands beside it
/// (`docs/designs/app-language/one-app-model.md` §3). `done` hears what it ran: at once, or when
/// the async work it started lands.
pub(crate) fn invoke_service_step(
    services: &std::rc::Rc<std::cell::RefCell<tilo_service::Services>>,
    actor: &str,
    run: Option<&tilo_actions::WorkGroup>,
    address: &tilo_actions::ActionAddress,
    arguments: &serde_json::Map<String, serde_json::Value>,
    beside: &str,
    done: tilo_actions::remote::Completer<
        Result<Option<tilo_actions::ActionOutput>, ActionHostError>,
    >,
) {
    use tilo_service::Started;
    let invoker = tilo_actions::ActionInvoker::actor(actor);
    let invoker = if actor.starts_with("agent:") {
        invoker.with_kind(tilo_actions::ActorKind::Agent)
    } else {
        invoker
    };
    let started = services.borrow_mut().invoke_as_beside(
        &invoker,
        run,
        &address.resource,
        &address.action,
        arguments,
        beside,
    );
    match started {
        Ok(Started::Done(output)) => done.complete(Ok(Some(output))),
        Ok(Started::Accepted) => done.complete(Ok(None)),
        Ok(Started::Later(later)) => later.then(move |landed| {
            done.complete(match landed {
                Some(ran) => ran
                    .map(Some)
                    .map_err(|error| crate::surfaces::refusal(&error)),
                None => Err(ActionHostError::Unavailable {
                    reason: "the step's work was given up".to_owned(),
                }),
            });
        }),
        Err(error) => done.complete(Err(crate::surfaces::refusal(&error))),
    }
}

/// Run the change `change` of a record `services` keep for a package the session runs itself (the
/// shell), as `actor` : the published action is the record's own, run on the pass that publishes
/// it, as the record's origin runs it. `None` when no service keeps that app's record, which is
/// then the workspace's, changed through the broker.
pub(super) fn change_through(
    services: &std::rc::Rc<std::cell::RefCell<tilo_service::Services>>,
    actor: &tilo_actions::remote::CallerPrincipal,
    change: &tilo_actions::remote::RecordChange,
) -> Option<Result<serde_json::Value, ActionHostError>> {
    use tilo_actions::remote::CallerPrincipal;
    // Who the change is attributed to: the agent acting for its person, or the person.
    let invoker = match actor {
        CallerPrincipal::Agent(agent) => {
            tilo_actions::ActionInvoker::actor(format!("agent:{agent}"))
                .with_kind(tilo_actions::ActorKind::Agent)
        }
        CallerPrincipal::Person(person) => tilo_actions::ActionInvoker::actor(person.clone()),
        _ => return None,
    };
    services.borrow().record(&change.app)?;
    let ran = services.borrow_mut().invoke_as(
        &invoker,
        None,
        &change.resource,
        &change.action,
        &change.arguments,
    );
    Some(match ran {
        Ok(output) => {
            let document = services
                .borrow()
                .record(&change.app)
                .map(|record| record.document);
            let output = match output {
                tilo_service::Started::Done(output) => serde_json::json!({
                    "value": output.value,
                    "resources": output.resources,
                }),
                tilo_service::Started::Accepted | tilo_service::Started::Later(_) => {
                    serde_json::Value::Null
                }
            };
            Ok(serde_json::json!({"ok": output, "change": document}))
        }
        Err(error) => Err(ActionHostError::Unavailable {
            reason: error.to_string(),
        }),
    })
}
