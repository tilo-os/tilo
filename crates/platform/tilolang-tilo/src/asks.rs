//! Asking an agent by mentioning it: a run of rich text marked `mention` with an agent's id, in
//! any app's resource, asks that agent about the item the text is in, through the agent
//! package's own actions (`so.tilo.agent`). No app writes code for it.
//!
//! An ask is named by the item and the mention (an `Ask`'s `mention`): the item's text
//! mentioning an agent makes one ask however often it is edited, undone or redone. When the
//! mention goes while the ask still waits, it is withdrawn; when it comes back, a withdrawn ask
//! is asked again.
//!
//! The device of the person who wrote the item's last change acts, as that person: who asked is
//! who wrote the mention, and another member's device that receives the change makes nothing.
//! What was asked is copied into the ask only when the agent may read the item, as
//! `std.may_read` answers it ([`tilolang_runtime::Runtime::may_read`]); otherwise it is asked
//! with nothing said, which the package keeps as an ask the agent cannot read.

use std::collections::{BTreeMap, BTreeSet};

use tilolang_runtime::meta::Meta;
use tilolang_runtime::rich::{Reference, RichValue};
use tilolang_runtime::store::{LinkHow, Store as _};
use tilolang_runtime::value::{ResourceRef, Value};

use crate::host::Host;
use crate::resources::Watch;
use crate::runtime::Actor;

/// The package asks are made in.
pub const AGENT_APP: &str = "so.tilo.agent";

/// What the asks from mentions follow of the store.
pub(crate) struct Asking {
    moved: Watch,
    /// Whether asks are being made now: the asks' own writes move the store again.
    busy: std::cell::Cell<bool>,
}

impl Asking {
    pub(crate) fn new(moved: Watch) -> Self {
        Self {
            moved,
            busy: std::cell::Cell::new(false),
        }
    }
}

/// An ask already made from an item, as the agent package keeps it.
struct Made {
    at: ResourceRef,
    state: String,
}

/// Make, withdraw and ask again what the rich text that moved since the last call says of
/// agents. Returns how many actions it invoked.
pub(crate) fn run(host: &Host, asking: &Asking) -> usize {
    if asking.busy.get() {
        return 0;
    }
    let changes = asking.moved.take();
    // What the store derives of a change (`$versions`, `$updated`) is no text.
    let mut items: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (resource, field) in changes.fields.values().flatten() {
        if !field.starts_with('$') {
            items
                .entry(resource.clone())
                .or_default()
                .insert(field.clone());
        }
    }
    if items.is_empty() {
        return 0;
    }
    let Ok(agent) = host.inner.package_of(AGENT_APP) else {
        return 0;
    };
    asking.busy.set(true);
    let mut invoked: usize = 0;
    for (resource, fields) in items {
        invoked = invoked.saturating_add(item(host, &agent.meta, &resource, &fields));
    }
    asking.busy.set(false);
    invoked
}

/// [`run`] for the item `resource`, whose `fields` moved.
fn item(host: &Host, agent: &Meta, resource: &str, fields: &BTreeSet<String>) -> usize {
    let Some(at) = ResourceRef::parse_id(resource, String::new()) else {
        return 0;
    };
    let Ok(ty) = host.store().type_of(&at) else {
        return 0;
    };
    let at = ResourceRef { ty, ..at };
    // A text of an ask, or of an agent's reply, asks nothing of its own.
    let Some(texts) = rich_fields(host, &at.ty) else {
        return 0;
    };
    if texts.is_empty()
        || !fields
            .iter()
            .any(|f| f == tilolang_runtime::store::ANY_FIELD || texts.contains(f))
    {
        return 0;
    }
    // The device of whoever wrote the item's last change acts, as them.
    let viewer = host.viewer().clone();
    let wrote = host
        .store()
        .versions(&at)
        .ok()
        .and_then(|versions| versions.last().and_then(|v| v.who.clone()));
    if wrote.as_ref() != Some(&viewer) {
        return 0;
    }
    // Every agent the item's rich text mentions, by the mark that mentions it, with the text.
    let mut mentioned: BTreeMap<String, (Actor, RichValue)> = BTreeMap::new();
    for field in &texts {
        let Ok(Value::Rich(rich)) = host.runtime().read(&at, field) else {
            continue;
        };
        for reference in rich.references() {
            if let Reference::Member(member) = &reference
                && member.is_agent()
            {
                mentioned
                    .entry(reference.mark_value())
                    .or_insert_with(|| (Actor::from(member.clone()), rich.clone()));
            }
        }
    }
    let made = made_from(host, agent, &at);
    let mut invoked: usize = 0;
    for (mention, (who, said)) in &mentioned {
        let action = match made.get(mention) {
            None => {
                let Ok(all) = host.root_of(AGENT_APP) else {
                    return invoked;
                };
                // Nothing of the text goes to an agent that may not read where it was written.
                let said = if host.runtime().may_read(who, &at) {
                    said.clone()
                } else {
                    RichValue::default()
                };
                invoke(
                    host,
                    agent,
                    "ask",
                    vec![
                        Value::resource(&all),
                        Value::Actor(who.clone()),
                        Value::Rich(said),
                        Value::Text(mention.clone()),
                    ],
                    &at,
                )
            }
            Some(ask) if ask.state == "withdrawn" => {
                invoke(host, agent, "reopen", vec![Value::resource(&ask.at)], &at)
            }
            Some(_) => false,
        };
        if action {
            invoked = invoked.saturating_add(1);
        }
    }
    for (mention, ask) in &made {
        if ask.state == "waiting"
            && !mentioned.contains_key(mention)
            && invoke(host, agent, "withdraw", vec![Value::resource(&ask.at)], &at)
        {
            invoked = invoked.saturating_add(1);
        }
    }
    invoked
}

/// The rich text fields of the type `ty`, by id; `None` for a type of the agent package or one
/// no package here declares.
fn rich_fields(host: &Host, ty: &str) -> Option<Vec<String>> {
    host.find_in_packages(|package| {
        let declared = package.meta.types.iter().find(|t| t.id == ty)?;
        if package.meta.app == AGENT_APP {
            return Some(Vec::new());
        }
        Some(
            declared
                .fields
                .iter()
                .filter(|f| matches!(f.ty.trim(), "RichText" | "std.RichText"))
                .map(|f| f.id.clone())
                .collect(),
        )
    })
}

/// The asks made from `at`, by the mention that made each.
fn made_from(host: &Host, agent: &Meta, at: &ResourceRef) -> BTreeMap<String, Made> {
    let Some(ask) = agent.type_named("Ask") else {
        return BTreeMap::new();
    };
    let mut out = BTreeMap::new();
    let Ok(links) = host.store().linked_by(at, &ask.id) else {
        return out;
    };
    for link in links {
        if link.how != LinkHow::CreatedFrom || link.of.ty != ask.id {
            continue;
        }
        let Ok(Value::Text(mention)) = host.runtime().read(&link.of, "mention") else {
            continue;
        };
        let state = match host.runtime().read(&link.of, "state") {
            Ok(Value::Enum(state)) => variant_name(agent, &state.variant),
            _ => continue,
        };
        out.insert(mention, Made { at: link.of, state });
    }
    out
}

/// The name of the variant `variant` of an enum of `meta`, which a value may hold by its id.
fn variant_name(meta: &Meta, variant: &str) -> String {
    meta.enums
        .iter()
        .flat_map(|e| e.variants.iter())
        .find(|v| v.id == variant)
        .map_or_else(|| variant.to_owned(), |v| v.name.clone())
}

/// Invoke the agent package's action `name` with `args`, made from `from`, as the person at
/// this device. Whether it ran.
fn invoke(host: &Host, agent: &Meta, name: &str, args: Vec<Value>, from: &ResourceRef) -> bool {
    let Some(action) = agent.actions.iter().find(|a| a.name == name) else {
        tracing::warn!(action = name, "the agent package has no such action");
        return false;
    };
    match host
        .runtime()
        .invoke_action_from(host.viewer(), (action.id.as_str(), args), from)
    {
        Ok(tilolang_runtime::InvokeOutcome::Done(_)) => true,
        other => {
            tracing::warn!(action = name, ?other, "a mention's ask did not run");
            false
        }
    }
}
