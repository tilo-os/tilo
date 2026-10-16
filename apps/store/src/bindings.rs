//! The Store's controls, bound to what the resource publishes: its
//! declarations and its effects. A button reaches the same handler, under the
//! same permission and confirmation, as the palette or an agent naming it.

use std::sync::LazyLock;

use ui::tilo_actions::{ActionBinding, Resource};
use ui::tilo_resource::Effect;

use crate::model::ActionKind;
use crate::resource::{StoreEffect, door};
use crate::{StoreItem, flathub_client::Remote};

/// The published resource every control is bound to.
static DOOR: LazyLock<Resource> = LazyLock::new(door);

fn arguments<const N: usize>(
    pairs: [(&str, String); N],
) -> serde_json::Map<String, serde_json::Value> {
    pairs
        .into_iter()
        .map(|(name, value)| (name.to_owned(), serde_json::Value::String(value)))
        .collect()
}

/// A control invoking `action` with fixed `arguments`.
fn bound(
    action: &str,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> Option<ActionBinding<()>> {
    let spec = DOOR.actions.iter().find(|spec| spec.id == action)?;
    ActionBinding::static_json(&DOOR, spec, arguments).ok()
}

/// A control taking a text value (a search box, a dropdown) that invokes
/// `action` with it as `param`.
fn bound_text(action: &str, param: &'static str) -> Option<ActionBinding<String>> {
    let spec = DOOR.actions.iter().find(|spec| spec.id == action)?;
    ActionBinding::new(&DOOR, spec, move |value: String| {
        arguments([(param, value)])
    })
    .ok()
}

/// A control invoking `effect`.
pub fn effect(effect: &StoreEffect) -> Option<ActionBinding<()>> {
    bound(effect.id(), effect.arguments())
}

/// A control carrying out `action` on `item`.
pub fn package(item: &StoreItem, action: ActionKind) -> Option<ActionBinding<()>> {
    let effect = StoreEffect::package(action, item);
    if let Some((code, reason)) = item.unavailable_action(action) {
        let spec = effect.bound().unavailable(code, reason);
        ActionBinding::static_json(&DOOR, &spec, effect.arguments()).ok()
    } else {
        self::effect(&effect)
    }
}

/// A control starting a report about `item`.
pub fn draft_report(item: &StoreItem) -> Option<ActionBinding<()>> {
    bound("draft-report", arguments([("app_id", item.id.clone())]))
}

pub fn show_details(item: &StoreItem) -> Option<ActionBinding<()>> {
    effect(&StoreEffect::ShowDetails {
        app: item.id.clone(),
    })
}

pub fn remove_remote(remote: &Remote) -> Option<ActionBinding<()>> {
    effect(&StoreEffect::RemoveRemote {
        name: remote.name.clone(),
    })
}

/// The Add remote button: the name and URL typed into the form when it is
/// pressed.
pub fn add_remote(
    name: &ui::LocalState<String>,
    url: &ui::LocalState<String>,
) -> Option<ActionBinding<()>> {
    let spec = DOOR.actions.iter().find(|spec| {
        spec.id
            == StoreEffect::AddRemote {
                name: String::new(),
                url: String::new(),
            }
            .id()
    })?;
    let (name, url) = (name.clone(), url.clone());
    ActionBinding::new(&DOOR, spec, move |()| {
        arguments([
            ("name", name.with(Clone::clone)),
            ("url", url.with(Clone::clone)),
        ])
    })
    .ok()
}

pub fn search() -> Option<ActionBinding<String>> {
    bound_text("set-search", "query")
}

pub fn source() -> Option<ActionBinding<String>> {
    bound_text("set-source", "source")
}

pub fn category() -> Option<ActionBinding<String>> {
    bound_text("set-category", "category")
}

/// A report control (`submit-report`, `appeal`, …) for `app_id`'s report.
pub fn report(effect_of: fn(String) -> StoreEffect, app_id: &str) -> Option<ActionBinding<()>> {
    effect(&effect_of(app_id.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_actions::OperationKind;

    /// Every control binds to something the resource publishes, and browsing
    /// binds to effects, never to a document change.
    #[test]
    fn controls_bind_to_published_actions() {
        for id in ["set-search", "set-source", "set-category", "clear-filters"] {
            let spec = DOOR
                .actions
                .iter()
                .find(|spec| spec.id == id)
                .unwrap_or_else(|| panic!("`{id}` is published"));
            assert_ne!(spec.kind, OperationKind::Document, "{id}");
        }
        assert!(search().is_some() && source().is_some() && category().is_some());
        assert!(effect(&StoreEffect::Refresh).is_some());
        assert!(effect(&StoreEffect::ShowCatalogue).is_some());
        let item = crate::fixture::fixture_catalog().remove(0);
        assert_eq!(
            package(&item, ActionKind::Open)
                .expect("open binds")
                .address()
                .action
                .as_str(),
            "open-app"
        );
        assert!(draft_report(&item).is_some());
        let name = ui::LocalState::new("flathub".to_owned());
        let url = ui::LocalState::new("https://dl.flathub.org/repo/".to_owned());
        assert!(add_remote(&name, &url).is_some());
    }
}
