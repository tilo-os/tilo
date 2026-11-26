//! The session's services' steps, as the session's API answers them on its pass: an action a
//! service publishes, invoked as the agent or person that asked and as a step of the run it names,
//! and a change of a record a service keeps for a package the session runs itself.

use tilo_actions::remote::ActionCallError;

/// The typed arguments a step's slot carries: none for none. A slot that is not a typed object
/// names no argument of a session action, which all take their arguments by name.
pub(super) fn arguments_of(
    slot: Option<&str>,
) -> Result<serde_json::Map<String, serde_json::Value>, ActionCallError> {
    let Some(slot) = slot else {
        return Ok(serde_json::Map::new());
    };
    let invalid = |reason: String| ActionCallError::InvalidRequest { reason };
    let json = slot
        .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
        .ok_or_else(|| invalid("a session action takes its arguments by name".into()))?;
    serde_json::from_str(json)
        .map_err(|error| invalid(format!("the step's arguments do not read: {error}")))
}

/// What an agent's step in `turn` stands what it opens beside: that turn, by the message that
/// asked it (`<conversation>/messages/<message>`), never its conversation's latest — a turn asked
/// on the canvas keeps its own room for every step taken in it. Empty outside a turn.
pub(crate) fn turn_beside(turn: Option<&tilo_action_policy::turn::Turn>) -> String {
    turn.map(|turn| tilo_session_core::layout::turn_key(&turn.conversation, &turn.message))
        .unwrap_or_default()
}

/// `arguments` of the system's `open` and `place`, with the place the asking turn is in filled in
/// when they name none: a place they make stands beside the conversation of the turn
/// (`docs/designs/app-language/one-app-model.md` §3) — a `place` only when it names no point of its
/// own, which the layout reads. Every other action's arguments stand as they were.
pub(crate) fn with_turn_beside(
    address: &tilo_actions::ActionAddress,
    arguments: &serde_json::Map<String, serde_json::Value>,
    beside: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let anchored = address.resource == tilo_session_core::layout::RESOURCE
        && matches!(address.action.as_str(), "open" | "place");
    if !anchored || beside.is_empty() || arguments.contains_key("beside") {
        return arguments.clone();
    }
    let mut arguments = arguments.clone();
    arguments.insert("beside".into(), beside.into());
    arguments
}

/// Run `address` as `actor` through the session's services, which publish the actions its own
/// APIs do not: what a service keeps (Chat's `record-work` on a turn's summons, the layout's
/// place read from a service) is invoked on the same pass, on the record the service writes, and
/// attributed to `actor` as it is at an app's record origin. A step of a `run` is that run's, as an app's action is. `beside` is the
/// place the ask is in — the conversation of the turn an agent asks in — or empty for the place in
/// front: a resource the step opens stands beside it
/// (`docs/designs/app-language/one-app-model.md` §3). `done` hears what it ran: at once, or when
/// the async work it started lands.
pub(crate) fn invoke_service_step(
    services: &std::rc::Rc<std::cell::RefCell<tilo_service::Services>>,
    actor: &tilo_actions::Actor,
    run: Option<&tilo_actions::WorkGroup>,
    address: &tilo_actions::ActionAddress,
    arguments: &serde_json::Map<String, serde_json::Value>,
    beside: &str,
    done: tilo_actions::remote::Completer<
        Result<Option<tilo_actions::ActionOutput>, ActionCallError>,
    >,
) {
    use tilo_service::Started;
    let invoker = tilo_actions::ActionInvoker::admitted(&actor.clone().into());
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
                    .map_err(|error| tilo_actions::remote::ActionCallError::from_api(&error)),
                None => Err(ActionCallError::Unavailable {
                    reason: "the step's work was given up".to_owned(),
                }),
            });
        }),
        Err(error) => done.complete(Err(tilo_actions::remote::ActionCallError::from_api(&error))),
    }
}
