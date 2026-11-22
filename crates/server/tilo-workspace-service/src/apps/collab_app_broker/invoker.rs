//! The invoker an application sees is set by the host, never by the caller.
use super::{BrokerActor, ExecutionScope};

/// The invocation context an application worker receives, with its invoker
/// set by the host. A caller cannot make an action look as though it came
/// from someone else by naming them in the envelope: whatever invoker the
/// caller put in `context` is discarded, the actor is the admitted `actor`,
/// and the presence is derived from what the host admitted:
/// - a host-minted execution: its private view, subordinate to its actor;
/// - anyone else: no presence (the actor's default view).
///
/// The invoker names the actor's kind and, for an agent or an app acting
/// for a person, that person, as the admitted `caller` says.
///
/// The application worker keeps the presence only for this exact actor
/// (`tilo_actions::ActionInvoker::delivered`), and the captured original
/// request binds it, so the worker's view of the invoker is exactly this.
pub(super) fn host_set_invoker(
    context: Option<tilo_actions::ActionContext>,
    actor: &BrokerActor,
    caller: Option<&tilo_actions::remote::Caller>,
    execution: Option<&ExecutionScope>,
) -> Result<Option<tilo_actions::ActionContext>, String> {
    let presence = execution
        .map(|execution| execution.presence(actor))
        .transpose()?;
    // A Shortcut's step is admitted as its person: the invoker stays them.
    let admitting = caller.filter(|caller| caller.acting().by.member_id() == Some(actor.member()));
    // Without a caller, the admitted actor says whom an agent acts for.
    let admitted = admitting.map_or_else(|| actor.shown(), |caller| caller.acting().clone());

    // A person acting for themselves needs no context to say so; anyone else is named with
    // their kind and the person they act for, so what they write is theirs, not that person's.
    if context.is_none() && presence.is_none() && admitted.direct() {
        return Ok(None);
    }
    let mut context = context
        .unwrap_or_default()
        .with_host_actor(&admitted, presence);
    // The name the admitting host knows an agent by, never one the request carries.
    if let Some(invoker) = context.invoker.as_mut() {
        invoker.name = admitting
            .and_then(tilo_actions::remote::Caller::agent_name)
            .map(str::to_owned);
    }
    Ok(Some(context))
}

/// `context` with the device the agent works from named on its invoker:
/// `origin`, the device the host verified its agent session's connection
/// came from, as the host's own record of the session keeps it. It follows
/// [`host_set_invoker`], which discarded whatever device the envelope named,
/// so an application is told of a device only by the host.
pub(super) fn working_from(
    mut context: Option<tilo_actions::ActionContext>,
    origin: Option<&tilo_actions::InvokerDevice>,
) -> Option<tilo_actions::ActionContext> {
    if let Some(invoker) = context
        .as_mut()
        .and_then(|context| context.invoker.as_mut())
    {
        invoker.device = origin.cloned();
    }
    context
}

/// `context` with the device that signed the call named on its invoker, when
/// [`working_from`] named none: `signed`, the device certificate the host
/// verified the call under. An invocation of this host's own processes names
/// none, and its application reads this host's device. An action's
/// requirement sees the device (`invoker.device()`), so a call from a
/// person's other device is never taken for one made here.
pub(super) fn signed_by(
    context: Option<tilo_actions::ActionContext>,
    actor: &BrokerActor,
    signed: Option<&tilo_user_identity::DeviceCert>,
) -> Option<tilo_actions::ActionContext> {
    let Some(signed) = signed else {
        return context;
    };
    // A person acting for themselves has no context yet: their invoker is the admitted actor.
    let mut context = context.unwrap_or_default();
    if context.invoker.is_none() {
        context = context.with_host_actor(&actor.shown(), None);
    }
    if let Some(invoker) = context.invoker.as_mut()
        && invoker.device.is_none()
    {
        invoker.device = Some(tilo_actions::InvokerDevice::certified(
            signed.device_public_key,
            &signed.device_label,
        ));
    }
    Some(context)
}

/// The presence a host-set context routes to (see [`host_set_invoker`]).
pub(super) fn invoker_presence(
    context: Option<&tilo_actions::ActionContext>,
) -> Option<tilo_actions::PresenceId> {
    context?.invoker.as_ref()?.presence.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forged() -> tilo_actions::ActionContext {
        tilo_actions::ActionContext::default()
            .with_group("issue:42", "Prepare launch", "issue")
            .with_invoker(
                tilo_actions::ActionInvoker::person("owner")
                    .with_presence(tilo_actions::PresenceId::new("owner-window").unwrap()),
            )
    }

    tilo_test_env::scenario! {
        fn an_invoker_is_recorded_as_who_acts;

        phase a_person_never_gets_a_presence_from_the_envelope;
        {
            let actor = BrokerActor::person("guest");
            let context = host_set_invoker(Some(forged()), &actor, None, None)
                .unwrap()
                .unwrap();
            assert_eq!(
                context.invoker,
                Some(tilo_actions::ActionInvoker::person("guest"))
            );
            assert_eq!(invoker_presence(Some(&context)), None);
            assert_eq!(
                tilo_actions::ActionInvoker::delivered(&actor.actor(), Some(&context)),
                context.invoker.clone().unwrap(),
                "the worker derives exactly the host-set invoker"
            );
            // No context stays no context: ordinary deliveries are unchanged.
            assert_eq!(host_set_invoker(None, &actor, None, None).unwrap(), None);
        }

        phase an_agent_gets_no_presence_from_the_envelope;
        {
            let actor = BrokerActor::agent("alpha");
            let context = host_set_invoker(Some(forged()), &actor, None, None)
                .unwrap()
                .unwrap();
            assert_eq!(
                context.invoker,
                Some(tilo_actions::ActionInvoker::agent("alpha"))
            );
            assert_eq!(invoker_presence(Some(&context)), None);
            // An agent acting for a person names them, with no caller to say so.
            let started = BrokerActor::agent("alpha").for_person("owner");
            let context = host_set_invoker(Some(forged()), &started, None, None).unwrap();
            let invoker = context.and_then(|context| context.invoker).unwrap();
            assert_eq!(invoker.acting.acted_for_person(), Some("owner"));
            let untasked = tilo_actions::ActionContext::default();
            let context = host_set_invoker(Some(untasked), &actor, None, None)
                .unwrap()
                .unwrap();
            assert_eq!(invoker_presence(Some(&context)), None);
        }

        phase an_agent_acting_for_a_person_is_recorded_as_such;
        {
            let caller = tilo_actions::remote::Caller::local_agent("alpha").acting_for("owner");
            let actor = BrokerActor::for_caller(&caller);
            let context = host_set_invoker(
                Some(tilo_actions::ActionContext::default()),
                &actor,
                Some(&caller),
                None,
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                context.invoker,
                Some(
                    tilo_actions::ActionInvoker::agent("alpha")
                        .acting_for("owner")
                )
            );

            // Asked with no context at all, it is still the agent's, by the name its admission gave
            // it, for the person: what it writes is never taken for theirs.
            let named = caller.named("Helper");
            let context = host_set_invoker(None, &actor, Some(&named), None)
                .unwrap()
                .expect("an agent's invocation always names it");
            assert_eq!(
                context.invoker,
                Some(
                    tilo_actions::ActionInvoker::agent("alpha")
                        .acting_for("owner")
                        .named(Some("Helper".into()))
                )
            );

            // A Shortcut's step is its person's: the invoker stays the person.
            let step = tilo_actions::remote::Caller::local_person("owner").running_shortcut("morning");
            let context = host_set_invoker(
                Some(tilo_actions::ActionContext::default()),
                &BrokerActor::for_caller(&step),
                Some(&step),
                None,
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                context.invoker,
                Some(tilo_actions::ActionInvoker::person("owner"))
            );
        }

        phase an_execution_cannot_borrow_another_actor_or_agent_view;
        {
            let caller = tilo_actions::remote::Caller::local_person("owner").in_new_execution();
            let execution =
                ExecutionScope::for_call(&caller, tilo_actions::InvocationId::new()).unwrap();
            for actor in [
                BrokerActor::person("other"),
                BrokerActor::agent("owner"),
            ] {
                assert!(host_set_invoker(Some(forged()), &actor, None, Some(&execution)).is_err());
            }
            let actor = BrokerActor::person("owner");
            let context = host_set_invoker(Some(forged()), &actor, None, Some(&execution))
                .unwrap()
                .unwrap();
            assert_eq!(
                invoker_presence(Some(&context)),
                Some(tilo_actions::PresenceId::for_execution(
                    caller.execution_id().unwrap()
                ))
            );
            caller.retire_execution();
            assert!(host_set_invoker(None, &actor, None, Some(&execution)).is_err());
        }

        phase an_invoker_is_on_the_device_its_session_names_and_never_on_one_the_envelope_names;
        {
            let laptop = tilo_actions::InvokerDevice::certified([0x5c; 32], "Studio laptop");
            let elsewhere = tilo_actions::InvokerDevice::certified([0x66; 32], "Somewhere else");
            let actor = BrokerActor::agent("alpha");
            let said = || {
                Some(
                    tilo_actions::ActionContext::default()
                        .with_group("issue:42", "Prepare launch", "issue")
                        .with_invoker(
                            tilo_actions::ActionInvoker::agent("alpha")
                                .from_device(Some(elsewhere.clone())),
                        ),
                )
            };
            let device = |context: Option<tilo_actions::ActionContext>| {
                context
                    .and_then(|context| context.invoker)
                    .and_then(|invoker| invoker.device)
            };
            // The host's rewrite alone leaves the invoker on no device.
            let hosted = host_set_invoker(said(), &actor, None, None).unwrap();
            assert_eq!(device(hosted.clone()), None);
            // The session's own record names the device, whatever the envelope said.
            assert_eq!(
                device(working_from(hosted.clone(), Some(&laptop))),
                Some(laptop)
            );
            // A session of this device's own names none, and an envelope that was
            // not rewritten keeps none either.
            assert_eq!(device(working_from(hosted, None)), None);
            assert_eq!(device(working_from(said(), None)), None);
        }
    }
}
