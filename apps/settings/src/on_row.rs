//! Controls acting on a live row: a button or a switch on an item's page (a
//! saved network, a paired device) invoking an operation that item's row of
//! the published surface offers, rather than an effect of the whole app.
//! What the row does not offer now, or the person may not do, leaves the
//! control unavailable.

use ui::framework::{ActionControl, Dispatcher};
use ui::tilo_actions::{ActionBinding, ResourceSpec};
use ui::tilo_resource::{Binds, Effect as _};

use crate::effects::SettingsEffect;
use crate::resource::Settings;

/// The binding of `effect` for a control with input `I` on `row`, which
/// offers it; `None` where the row does not offer it now, or `dispatcher`
/// may not invoke it. Only the arguments the row's operation asks for are
/// sent: the row binds the rest.
pub fn binding<I: 'static>(
    dispatcher: &Dispatcher<Settings>,
    row: &ResourceSpec,
    effect: impl Binds<SettingsEffect, I>,
) -> Option<ActionBinding<I>> {
    let spec = row
        .actions
        .iter()
        .find(|spec| spec.id == effect.action_id())?;
    if !dispatcher.may(&spec.contract.need()) {
        return None;
    }
    let asked: Vec<String> = spec
        .params()
        .iter()
        .map(|param| param.name.to_string())
        .collect();
    ActionBinding::fallible(row, spec, move |input| {
        let mut args = effect.arguments(input)?;
        args.retain(|name, _| asked.contains(name));
        Ok(args)
    })
    .ok()
}

/// The binding of one fully specified `effect` that `row` offers, for a
/// button or a row's own press; `None` where the row does not offer it
/// now, or `dispatcher` may not invoke it.
pub fn invocation(
    dispatcher: &Dispatcher<Settings>,
    row: &ResourceSpec,
    effect: impl Into<SettingsEffect>,
) -> Option<ActionBinding<()>> {
    let effect = effect.into();
    let spec = row.actions.iter().find(|spec| spec.id == effect.id())?;
    if !dispatcher.may(&spec.contract.need()) {
        return None;
    }
    let mut args = effect.arguments();
    let asked: Vec<String> = spec
        .params()
        .iter()
        .map(|param| param.name.to_string())
        .collect();
    args.retain(|name, _| asked.contains(name));
    ActionBinding::new(row, spec, move |()| args.clone()).ok()
}

/// `control`, invoking `effect` with each committed value on `row`
/// ([`binding`]); unavailable where it cannot.
#[must_use]
pub fn bind<I: 'static, C: ActionControl<I>>(
    dispatcher: &Dispatcher<Settings>,
    row: &ResourceSpec,
    control: C,
    effect: impl Binds<SettingsEffect, I>,
) -> C {
    match binding(dispatcher, row, effect) {
        Some(binding) => control.bind_action(binding),
        None => control.unavailable(),
    }
}

/// `control` (a button), invoking the fully specified `effect` on `row`
/// ([`invocation`]); unavailable where it cannot.
#[must_use]
pub fn bind_invocation<C: ActionControl<()>>(
    dispatcher: &Dispatcher<Settings>,
    row: &ResourceSpec,
    control: C,
    effect: impl Into<SettingsEffect>,
) -> C {
    match invocation(dispatcher, row, effect) {
        Some(binding) => control.bind_action(binding),
        None => control.unavailable(),
    }
}
