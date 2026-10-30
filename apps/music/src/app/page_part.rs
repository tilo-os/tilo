//! The painted page's embedded parts, asking for Music's own actions: the
//! framework's [`page_part`](ui::framework::page_part).

pub(super) use ui::framework::page_part::Placed;

use super::Action;
use super::published::Published;
use crate::model::Library;
use ui::framework::{ActionControl, Actions};

/// Where a part's widgets leave the actions they ask for.
pub(super) type Asked = ui::framework::page_part::Asked<Action>;

pub(super) type PagePart = ui::framework::page_part::PagePart<Action>;

/// A widget callback that asks for `action`.
pub(super) fn asking(asked: &Asked, action: Action) -> impl FnMut() + 'static {
    ui::framework::page_part::asking(asked, action)
}

/// `control`, pressed as what `action` is in the published surface
/// ([`Action::published`]): its effect or library edit. `Err` gives the
/// control back where the action is Music's own, or before the framework
/// attaches.
pub(super) fn bound<C: ActionControl<()>>(
    actions: Option<&Actions<Library>>,
    control: C,
    action: &Action,
) -> Result<C, C> {
    let Some(actions) = actions else {
        return Err(control);
    };
    match action.published() {
        Some(Published::Effect(effect)) => Ok(actions.bind_effect_invocation(control, &effect)),
        Some(Published::Edit(edit)) => match ui::tilo_resource::Invoke::edit(&edit) {
            Ok(invoke) => Ok(actions.bind_invocation(control, invoke)),
            Err(_) => Err(control),
        },
        None => Err(control),
    }
}

/// `action`'s binding on the published surface, for a widget that is not an
/// [`ActionControl`] and takes bindings of its own.
pub(super) fn binding(
    actions: Option<&Actions<Library>>,
    action: &Action,
) -> Option<tilo_actions::ActionBinding<()>> {
    let actions = actions?;
    match action.published()? {
        Published::Effect(effect) => actions.effect(&effect),
        Published::Edit(edit) => actions.invocation(ui::tilo_resource::Invoke::edit(&edit).ok()?),
    }
}

/// A button pressing `action`: bound where it is published ([`bound`]),
/// else published as its own view action or effect.
pub(super) fn press(
    actions: Option<&Actions<Library>>,
    asked: &Asked,
    button: ui::widget::Button,
    action: Action,
) -> ui::widget::Button {
    bound(actions, button, &action).unwrap_or_else(|button| {
        if action.only_shows() {
            button.on_view(asking(asked, action))
        } else {
            button.on_effect(asking(asked, action))
        }
    })
}
