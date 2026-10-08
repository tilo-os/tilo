//! The invoker an application sees is set by the host, never by the caller.
use super::{BrokerActor, ExecutionScope};

/// The invocation context an application worker receives, with its invoker
/// set by the host. A caller cannot make an action look as though it came
/// from someone else by naming them in the envelope: whatever invoker the
/// caller put in `context` is discarded, the actor is the admitted `actor`,
/// and the participant is derived from what the host admitted:
/// - a legacy agent task run: the participant of the host-built task
///   context's `task` group (the broker's own pending run record);
/// - a host-minted execution: its private view, subordinate to its principal;
/// - anyone else: no participant (the actor's default view).
///
/// The application worker keeps the participant only for this exact actor
/// (`tilo_actions::ActionInvoker::delivered`), and the captured original
/// request binds it, so the worker's view of the invoker is exactly this.
pub(super) fn host_set_invoker(
    context: Option<tilo_actions::ActionContext>,
    actor: &BrokerActor,
    execution: Option<&ExecutionScope>,
) -> Result<Option<tilo_actions::ActionContext>, String> {
    let participant = if let Some(execution) = execution {
        Some(execution.participant(actor)?)
    } else if actor.is_agent {
        context
            .as_ref()
            .and_then(|context| context.group.as_ref())
            .filter(|group| group.kind == "task")
            .map(|group| tilo_actions::ParticipantId::for_agent_task(&group.id))
            .transpose()?
    } else {
        None
    };
    Ok(match (context, participant) {
        (None, None) => None,
        (context, participant) => Some(
            context
                .unwrap_or_default()
                .with_host_invoker(actor.id.clone(), participant),
        ),
    })
}

/// The participant a host-set context routes to (see [`host_set_invoker`]).
pub(super) fn invoker_participant(
    context: Option<&tilo_actions::ActionContext>,
) -> Option<tilo_actions::ParticipantId> {
    context?.invoker.as_ref()?.participant.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TASK: &str = "11111111-1111-4111-8111-111111111111";

    fn forged() -> tilo_actions::ActionContext {
        tilo_actions::ActionContext::default()
            .with_group(TASK, TASK, "task")
            .with_invoker(
                tilo_actions::ActionInvoker::actor("person:owner")
                    .with_participant(tilo_actions::ParticipantId::new("owner-window").unwrap()),
            )
    }

    #[test]
    fn a_person_never_gets_a_participant_from_the_envelope() {
        let actor = BrokerActor::person("person:guest");
        let context = host_set_invoker(Some(forged()), &actor, None)
            .unwrap()
            .unwrap();
        assert_eq!(
            context.invoker,
            Some(tilo_actions::ActionInvoker::actor("person:guest"))
        );
        assert_eq!(invoker_participant(Some(&context)), None);
        assert_eq!(
            tilo_actions::ActionInvoker::delivered(&actor.id, Some(&context)),
            context.invoker.clone().unwrap(),
            "the worker derives exactly the host-set invoker"
        );
        // No context stays no context: ordinary deliveries are unchanged.
        assert_eq!(host_set_invoker(None, &actor, None).unwrap(), None);
    }

    #[test]
    fn a_legacy_agent_run_routes_to_its_host_task_participant() {
        let actor = BrokerActor::agent("agent:alpha");
        let context = host_set_invoker(Some(forged()), &actor, None)
            .unwrap()
            .unwrap();
        assert_eq!(
            context.invoker,
            Some(
                tilo_actions::ActionInvoker::actor("agent:alpha")
                    .with_participant(tilo_actions::ParticipantId::for_agent_task(TASK).unwrap())
            )
        );
        let untasked = tilo_actions::ActionContext::default();
        let context = host_set_invoker(Some(untasked), &actor, None)
            .unwrap()
            .unwrap();
        assert_eq!(invoker_participant(Some(&context)), None);
    }

    #[test]
    fn an_execution_cannot_borrow_another_principal_or_agent_view() {
        let caller = tilo_actions::remote::Caller::local_person("person:owner").in_new_execution();
        let execution =
            ExecutionScope::for_call(&caller, tilo_actions::InvocationId::new()).unwrap();
        for actor in [
            BrokerActor::person("person:other"),
            BrokerActor::agent("person:owner"),
        ] {
            assert!(host_set_invoker(Some(forged()), &actor, Some(&execution)).is_err());
        }
        let actor = BrokerActor::person("person:owner");
        let context = host_set_invoker(Some(forged()), &actor, Some(&execution))
            .unwrap()
            .unwrap();
        assert_eq!(
            invoker_participant(Some(&context)),
            Some(tilo_actions::ParticipantId::for_execution(
                caller.execution_id().unwrap()
            ))
        );
        caller.retire_execution();
        assert!(host_set_invoker(None, &actor, Some(&execution)).is_err());
    }
}
