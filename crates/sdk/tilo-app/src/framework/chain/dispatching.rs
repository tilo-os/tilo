//! A chain's dispatcher: what applies an action a view submits to the part
//! it changes, and several submitted as one, as the window's person or
//! whoever the invocation is for.

use std::any::Any;
use std::rc::Rc;

use tilo_actions::ActionError;

use super::super::dispatch::Dispatcher;
use super::{ChainState, Chained, Later, Write};

/// What a chain's dispatcher applies one action with.
pub(super) type Applies<P> =
    Box<dyn Fn(&<P as tilo_resource::Resource>::Action) -> Result<(), ActionError>>;

/// What it applies several actions invoked as one with.
pub(super) type AppliesGroup<P> = Box<
    dyn Fn(
        &tilo_resource::group::ActionGroup<<P as tilo_resource::Resource>::Action>,
    ) -> Result<(), ActionError>,
>;

/// The dispatcher of a chain outside any host, applying as `me`.
pub(super) fn detached_dispatch<P: Chained>(
    app_id: &'static str,
    state: &Rc<ChainState>,
    me: &tilo_resource::MemberId,
) -> Dispatcher<P> {
    dispatching(state, me, |apply, group| {
        Dispatcher::<P>::new(app_id, state.id(), false, Some(me)).for_workspace_resource::<P>(
            state.id(),
            false,
            apply,
            group,
        )
    })
}

/// What a chain's dispatcher refuses into: its
/// [`refused`](Dispatcher::refused), once the dispatcher is made.
type Told =
    Rc<std::cell::OnceCell<tilo_ui_base::LocalState<Option<super::super::dispatch::Refused>>>>;

/// The dispatcher `make` makes of what applies an action to `state`'s parts
/// as `me` ([`ChainState::apply_as`]), and of what applies several invoked as
/// one, as one change per part: one that waits for its part is taken,
/// as written; refused once its part arrived, it is told in the
/// dispatcher's [`refused`](Dispatcher::refused) as one the workspace
/// refused after this device showed it.
pub(crate) fn dispatching<P: Chained>(
    state: &Rc<ChainState>,
    me: &tilo_resource::MemberId,
    make: impl FnOnce(Applies<P>, AppliesGroup<P>) -> Dispatcher<P>,
) -> Dispatcher<P> {
    let applying = Rc::downgrade(state);
    let grouping = Rc::downgrade(state);
    let me = me.clone();
    let grouping_me = me.clone();
    let told: Told = Rc::default();
    let telling = Rc::clone(&told);
    let dispatch = make(
        Box::new(move |action| apply_dispatched(&applying, &me, &telling, action)),
        Box::new(move |group| {
            let state = grouping
                .upgrade()
                .ok_or_else(|| ActionError::Refused("the chain is closed".into()))?;
            let members: Vec<Box<dyn Any>> = group
                .actions
                .iter()
                .map(|member| Box::new(member.clone()) as Box<dyn Any>)
                .collect();
            let invoker = super::super::resources::acting(&grouping_me);
            match state.apply_group(&members, &invoker)? {
                Write::Done(written, _) => {
                    state.wrote(&invoker, written, true);
                    Ok(())
                }
                Write::Arriving => Ok(()),
            }
        }),
    );
    let _ = told.set(dispatch.refused());
    dispatch
}

/// Apply `action`, dispatched through a chain's dispatcher, to the chain
/// `applying` as whoever the invocation carried out here is for (an agent's
/// call of an effect that writes here), else `me`, the window's person.
fn apply_dispatched(
    applying: &std::rc::Weak<ChainState>,
    me: &tilo_resource::MemberId,
    told: &Told,
    action: &dyn Any,
) -> Result<(), ActionError> {
    let state = applying
        .upgrade()
        .ok_or_else(|| ActionError::Refused("the chain is closed".into()))?;
    let refused = told.get().cloned();
    let part = state.part();
    let named = (part.clone)(action);
    let later: Later = Box::new(move |written| {
        let Err(error) = written else {
            return;
        };
        tracing::warn!(kind = part.kind, %error, "a write that waited for its part was refused");
        if let Some(refused) = refused {
            refused.set(Some(super::super::dispatch::Refused {
                action: (part.verb)(&*named).unwrap_or_default(),
                item: None,
                reason: error.to_string(),
                by_workspace: true,
            }));
        }
    });
    let invoker = super::super::resources::acting(me);
    state.apply_as(action, &invoker, &mut Some(later)).map(drop)
}
