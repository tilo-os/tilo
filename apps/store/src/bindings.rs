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
    if action == ActionKind::AddBot {
        return add_bot(item);
    }
    let Some(effect) = StoreEffect::package(action, item) else {
        return Some(uninstall(item));
    };
    if let Some((code, reason)) = item.unavailable_action(action) {
        let spec = effect.bound().unavailable(code, reason);
        ActionBinding::static_json(&DOOR, &spec, effect.arguments()).ok()
    } else {
        self::effect(&effect)
    }
}

/// A control asking the system's one Uninstall to remove `item`: the same
/// action Files and search offer, which asks the person first ("Uninstall
/// `<name>`?", with "Remove its data" for a Flatpak).
pub fn uninstall(item: &StoreItem) -> ActionBinding<()> {
    ui::search::open_search_binding(&ui::search::SearchRequest::offering(
        UNINSTALL_ACTION,
        [("app", item.id.clone())],
    ))
}

/// The system's Uninstall action.
const UNINSTALL_ACTION: &str = "app.uninstall";

/// A control preparing the workspace's Add a bot for `item`, a directory
/// bot: the system search opens on it with the bot filled in, and the person
/// confirms it there. Bots run in workspaces, so the workspace adds them.
pub fn add_bot(item: &StoreItem) -> Option<ActionBinding<()>> {
    let bot = item.bot.as_ref()?;
    Some(ui::search::open_search_binding(
        &ui::search::SearchRequest::offering(
            ADD_BOT_ACTION,
            [("entry", item.id.clone()), ("version", bot.version.clone())],
        ),
    ))
}

/// The system's action that asks a workspace's host to run a bot.
const ADD_BOT_ACTION: &str = "bot.add";

/// A control starting a report about `item`.
pub fn draft_report(item: &StoreItem) -> Option<ActionBinding<()>> {
    bound("draft-report", arguments([("app_id", item.id.clone())]))
}

/// A control blocking `item` in the person's own block list.
pub fn block(item: &StoreItem) -> Option<ActionBinding<()>> {
    effect(&StoreEffect::BlockApp {
        app: item.id.clone(),
    })
}

/// A control lifting the person's own block on `item`; a policy's block
/// stays.
pub fn unblock(item: &StoreItem) -> Option<ActionBinding<()>> {
    effect(&StoreEffect::UnblockApp {
        app: item.id.clone(),
    })
}

/// The views' segmented control: segment `index` shows that view.
pub fn view() -> Option<ActionBinding<usize>> {
    let spec = DOOR.actions.iter().find(|spec| {
        spec.id
            == StoreEffect::ShowView {
                view: String::new(),
            }
            .id()
    })?;
    ActionBinding::new(&DOOR, spec, move |index: usize| {
        arguments([(
            "view",
            crate::app::VIEWS
                .get(index)
                .copied()
                .unwrap_or("home")
                .to_owned(),
        )])
    })
    .ok()
}

/// A control showing the view `name` (`home`, `updates`, ...).
pub fn view_named(name: &str) -> Option<ActionBinding<()>> {
    effect(&StoreEffect::ShowView {
        view: name.to_owned(),
    })
}

/// A control showing every app in `category`.
pub fn category_named(category: &str) -> Option<ActionBinding<()>> {
    effect(&StoreEffect::SetCategory {
        category: category.to_owned(),
    })
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
        for id in [
            "set-search",
            "show-view",
            "show-more",
            "set-category",
            "clear-filters",
        ] {
            let spec = DOOR
                .actions
                .iter()
                .find(|spec| spec.id == id)
                .unwrap_or_else(|| panic!("`{id}` is published"));
            assert_ne!(spec.kind, OperationKind::Document, "{id}");
        }
        assert!(search().is_some() && view().is_some() && category().is_some());
        assert!(view_named("updates").is_some() && category_named("Graphics").is_some());
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
        assert!(block(&item).is_some() && unblock(&item).is_some());
        let name = ui::LocalState::new("flathub".to_owned());
        let url = ui::LocalState::new("https://dl.flathub.org/repo/".to_owned());
        assert!(add_remote(&name, &url).is_some());
    }
}
