//! The session's canvas, said first to an agent the person asked from it
//! (`docs/designs/app-language/shell.md` §8.1): the arranging steps the session
//! publishes, the places standing now, how the canvas is shown, and the
//! resources there are to show. An agent asked on the canvas answers by showing
//! what answers the question beside the conversation, so these are its main
//! tools, ahead of every application's own operations.
//!
//! Nothing here names an application. The canvas is what the session publishes
//! on its layout's resources (`shell:layout`, a `shell:place:<key>`
//! per place, a `shell:strip:<key>` per strip) and on the record of how the
//! person views it (the style package's `Viewing`, whose `show_style` is
//! the style switch, and every action a style package publishes there).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::Revealed;
use crate::surface::{Catalogue, Offer};

/// The layout's own resource, whose `place` shows a resource on the canvas.
pub const LAYOUT: &str = "shell:layout";
/// The action that shows a resource on the canvas.
pub const PLACE: &str = "place";
/// The prefix of every resource of the layout: itself, its record, and
/// each row, strip and place.
const SHELL_PREFIX: &str = "shell:";
/// The kind of the record a person's view of their shell is kept in: the shell
/// style it shows, and the strip in front. Its actions switch the style.
const VIEWING_KIND_SUFFIX: &str = ".Viewing";
/// The prefix of the resources a place of the canvas is published as.
const PLACE_PREFIX: &str = "shell:place:";
/// The prefix of a tilolang app's resources and kinds.
const TILOLANG_PREFIX: &str = "tl:";

/// The tool that shows a resource on the canvas: the canvas' place as a step of its own, so a
/// call to show something names what it shows and nothing else. Most of what a person asks on
/// the canvas is answered by showing, and `invoke` for it was the call models got wrong most,
/// writing the place's parameters as the call (`{"resource": …}`) with no operation.
pub const SHOW: &str = "show";

/// The [`SHOW`] tool's definition. Byte-stable across turns and tasks, like every loop tool.
#[must_use]
pub fn show_definition() -> agent_provider_config::protocol::ToolDefinition {
    agent_provider_config::protocol::ToolDefinition {
        name: SHOW.into(),
        description: ("Show a resource on the canvas, as a person places it there: beside \
             this conversation, or, with `beside`, next to the place already showing another \
             resource. The place already showing it comes forward instead. `resource` is an id \
             the canvas lists under \"What there is to show\", or one a program answered: \
             {\"resource\": \"tl:so.tilo.lang.mail:root-su4h67i5\"} shows the inbox; \
             {\"resource\": \"tl:so.tilo.lang.notes:root-v7cub7aw\", \"beside\": \
             \"tl:so.tilo.lang.mail:root-su4h67i5\"} puts the notes next to it. Put your reply \
             on it to finish in the same response.")
            .into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "resource": {
                    "type": "string",
                    "description": "Required: the id of what to show."
                },
                "beside": {
                    "type": "string",
                    "description": "The id of a resource already shown, or of its place: this \
                        one stands next to it."
                },
                "view": {
                    "type": "string",
                    "description": "Which of its views, when it has several."
                },
                super::INVOKE_REPLY: {
                    "type": "string",
                    "description": "Your short reply to the person, shown once it is shown; \
                        it finishes the task with no turn after it."
                }
            },
            "required": ["resource"],
            "additionalProperties": false
        }),
    }
}

/// The id the canvas' place is published under, when the session publishes a canvas.
#[must_use]
pub fn place_id(catalogue: &Catalogue) -> Option<String> {
    catalogue
        .offers
        .iter()
        .find(|offer| offer.address().resource == LAYOUT && offer.address().action == PLACE)
        .map(Offer::id)
}

/// A [`SHOW`] call's input as the `invoke` of the canvas' place it is: the place by `place`, its
/// id ([`place_id`]; by its address when the session publishes no canvas, which the invoke then
/// names as no listed operation), with the call's resource, view and beside as its parameters and
/// its reply as the invoke's.
#[must_use]
pub fn show_as_invoke(input: &serde_json::Value, place: Option<&str>) -> serde_json::Value {
    let mut parameters = serde_json::Map::new();
    for key in ["resource", "view", "beside"] {
        if let Some(value) = input.get(key).filter(|value| !value.is_null()) {
            parameters.insert(key.to_owned(), value.clone());
        }
    }
    let mut invoke = serde_json::json!({
        "operation": place.map_or_else(|| format!("{LAYOUT}#{PLACE}"), str::to_owned),
        super::INVOKE_PARAMETERS: parameters,
    });
    if let Some(reply) = input
        .get(super::INVOKE_REPLY)
        .filter(|reply| reply.is_string())
    {
        invoke[super::INVOKE_REPLY] = reply.clone();
    }
    invoke
}

/// Whether `offer` is a host (non-Tilo) application's: the open the desktop
/// publishes for each installed host program. Opening one is all an agent
/// could do there — its window publishes nothing — so such opens are not
/// steps of an agent's task; the session's own `open-app` opens any app the
/// person asks for by name.
pub fn is_host_app(offer: &Offer) -> bool {
    offer.app.starts_with(super::HOST_APP_PREFIX)
        || super::opened_app(offer).is_some_and(|app| app.starts_with(super::HOST_APP_PREFIX))
}

/// Whether `offer` is one of the canvas' own steps: arranging (place, open,
/// close, move, focus, pin, resize) or switching how the canvas is shown.
pub fn is_canvas_step(offer: &Offer) -> bool {
    let resource = offer.address().resource.as_str();
    (resource.starts_with(SHELL_PREFIX) && offer.descriptor.address.action != "place-found")
        || (offer.descriptor.resource_kind.starts_with("tl.")
            && offer
                .descriptor
                .resource_kind
                .ends_with(VIEWING_KIND_SUFFIX))
}

/// The conversation a run answers in, by its address, read off its task id
/// (`chat.workspace/channels/<id>/messages/<message>`).
pub fn conversation_of(task_id: &str) -> Option<&str> {
    task_id
        .rsplit_once("/messages/")
        .map(|(conversation, _)| conversation)
        .filter(|conversation| conversation.contains("/channels/"))
}

/// A resource there is to show on the canvas: a tilolang app's record, not
/// one of the layout's own and not one of the style's.
fn showable(offer: &Offer) -> bool {
    let resource = offer.address().resource.as_str();
    resource.starts_with(TILOLANG_PREFIX)
        && !is_canvas_step(offer)
        && !offer.descriptor.resource_kind.is_empty()
}

/// What every record publishes about keeping it (its versions, its fields as raw edits, the
/// windows that open over it) rather than about how the canvas is arranged or shown: the style's
/// own record carries them like any record, and listing them as arranging steps cost the first
/// turn a few hundred tokens of ids no arranging uses.
const RECORD_KEEPING: &[&str] = &[
    "read",
    "edit-text",
    "edit-flag",
    "restore",
    "checkpoint",
    "history",
    "diff",
    "review-restore",
    "restore-reviewed",
    "retry-query",
];

fn record_keeping(offer: &Offer) -> bool {
    let action = offer.address().action.as_str();
    RECORD_KEEPING.contains(&action)
        || action.starts_with("open-")
        || (offer.address().resource.starts_with(TILOLANG_PREFIX)
            && matches!(action, "undo" | "redo" | "open"))
        || offer.address().resource.ends_with("layout-record")
}

/// The tilolang app whose record `resource` is (`tl:<app>:<record>`), whoever publishes it.
pub fn tilolang_app_of(resource: &str) -> Option<&str> {
    resource
        .strip_prefix(TILOLANG_PREFIX)
        .and_then(|rest| rest.split_once(':'))
        .map(|(app, _)| app)
        .filter(|app| !app.is_empty())
}

/// Whether `resource` is an app's root rather than one item in it.
fn is_root(resource: &str) -> bool {
    !resource.contains('/')
}

/// The canvas section of the first turn, or nothing when the session
/// publishes no canvas. Every operation it lists is revealed.
pub fn canvas_section(
    catalogue: &Catalogue,
    revealed: &mut Revealed,
    conversation: Option<&str>,
) -> String {
    let Some(place) = catalogue
        .offers
        .iter()
        .find(|offer| offer.address().resource == LAYOUT && offer.address().action == PLACE)
    else {
        return String::new();
    };
    let mut s = String::from("The canvas: where the person sees things.");
    if let Some(conversation) = conversation {
        let _ = write!(s, " This conversation stands on it, as `{conversation}`.");
    }
    s.push_str("\n\n");
    revealed.reveal(place.id());
    let _ = writeln!(
        s,
        "Show a resource: `{SHOW}` with `\"resource\": …` (an id below, or one a program \
         answered; add `\"view\"` when it has several). It stands beside this conversation, or \
         the place already showing it comes forward; `\"beside\": <id>` stands it next to the \
         place showing that resource instead (\"put my notes next to the inbox\")."
    );

    // The layout steps on the layout itself and on how it is shown,
    // then each place standing now with its own.
    let (places, steps): (Vec<&Offer>, Vec<&Offer>) = catalogue
        .offers
        .iter()
        .filter(|offer| is_canvas_step(offer) && offer.id() != place.id() && !record_keeping(offer))
        .partition(|offer| offer.address().resource.starts_with(PLACE_PREFIX));
    if !steps.is_empty() {
        s.push_str("\nArranging and how the canvas is shown:\n");
        for offer in &steps {
            revealed.reveal(offer.id());
            s.push_str(&super::summary::summary_line(catalogue, offer));
            // How a step's own value is written ("a package and a view
            // (`so.tilo.shell:strips`)"): its first sentence, so "switch to
            // strips" is one call rather than one that asks first.
            if let Some(said) = offer
                .descriptor
                .description
                .as_deref()
                .map(first_sentence)
                .filter(|said| !said.is_empty())
            {
                let _ = writeln!(s, "    {said}");
            }
        }
    }
    let mut by_place: BTreeMap<&str, Vec<&Offer>> = BTreeMap::new();
    for offer in places {
        by_place
            .entry(offer.address().resource.as_str())
            .or_default()
            .push(offer);
    }
    if !by_place.is_empty() {
        s.push_str("\nPlaces on the canvas now, each with its own steps:\n");
        for (resource, offers) in by_place {
            let label = offers
                .first()
                .map_or("", |offer| offer.descriptor.resource_label.as_str());
            let _ = writeln!(s, "{label} — {resource}");
            for offer in offers {
                revealed.reveal(offer.id());
                s.push_str(&super::summary::summary_line(catalogue, offer));
            }
        }
    }

    // What there is to show: each app's root, with how many of its items are
    // published; an item is placed by the id a program or listing answers.
    let mut roots: Vec<(&str, &str, &str, usize)> = Vec::new();
    let mut items: BTreeMap<&str, std::collections::BTreeSet<&str>> = BTreeMap::new();
    for offer in catalogue.offers.iter().filter(|offer| showable(offer)) {
        let resource = offer.address().resource.as_str();
        if is_root(resource) {
            if !roots.iter().any(|(seen, ..)| *seen == resource) {
                roots.push((
                    resource,
                    offer.descriptor.resource_label.as_str(),
                    offer.descriptor.resource_kind.as_str(),
                    0,
                ));
            }
        } else if let Some((root, _)) = resource.split_once('/') {
            items.entry(root).or_default().insert(resource);
        }
    }
    for (root, .., count) in &mut roots {
        *count = items.get(root).map_or(0, std::collections::BTreeSet::len);
    }
    if !roots.is_empty() {
        s.push_str("\nWhat there is to show (place it by its id):\n");
        for (resource, label, kind, count) in &roots {
            let _ = write!(s, "  {resource} — {label} ({kind})");
            if *count > 0 {
                let _ = write!(s, "; {count} items");
            }
            s.push('\n');
        }
    }
    s.push('\n');
    s
}

/// The first sentence of `text`, on one line.
fn first_sentence(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.find(". ").and_then(|end| line.get(..=end)) {
        Some(first) => first.to_owned(),
        None => line,
    }
}

/// The apps whose things a canvas step `offer` with `parameters` shows or
/// arranges: the app of the resource it places (`tl:<app>:…`), and the apps a
/// place it acts on is named by (its window's title and its app's name).
pub fn apps_shown(
    catalogue: &Catalogue,
    offer: &Offer,
    parameters: &serde_json::Value,
) -> Vec<String> {
    let parsed;
    let parameters = match parameters {
        serde_json::Value::String(text) => {
            parsed = serde_json::from_str(text).unwrap_or(serde_json::Value::Null);
            &parsed
        }
        other => other,
    };
    let mut apps: Vec<String> = Vec::new();
    // What it shows, and what it stands next to: "put my notes next to the inbox" is done in
    // Notes and in Mail by the one place.
    for key in ["resource", "beside"] {
        if let Some(app) = parameters
            .get(key)
            .and_then(serde_json::Value::as_str)
            .and_then(tilolang_app_of)
            .filter(|app| !apps.iter().any(|seen| seen == app))
        {
            apps.push(app.to_owned());
        }
    }
    if offer.address().resource.starts_with(PLACE_PREFIX) {
        let label = offer.descriptor.resource_label.to_lowercase();
        for (app, _) in super::named_applications(catalogue, &label) {
            if !apps.iter().any(|seen| seen == app) {
                apps.push(app.to_owned());
            }
        }
    }
    apps
}

/// `catalogue` without the canvas' own steps, which [`canvas_section`] lists:
/// what the rest of the first turn summarises.
pub fn without_canvas(catalogue: &Catalogue) -> Catalogue {
    Catalogue {
        offers: catalogue
            .offers
            .iter()
            .filter(|offer| !is_canvas_step(offer))
            .cloned()
            .collect(),
        facts: catalogue.facts.clone(),
        held: catalogue.held.clone(),
    }
}

#[cfg(test)]
mod tilolang_app_tests {
    /// A step on a tilolang app's record is a step in that app, whoever publishes the record: the
    /// session serves `tl:<app>:<record>`, and the part the model lists names the app.
    #[test]
    fn a_tilolang_record_names_its_app() {
        assert_eq!(
            super::tilolang_app_of("tl:so.tilo.lang.tasks:root-fjqi4yze/i358b46da-26"),
            Some("so.tilo.lang.tasks")
        );
        assert_eq!(super::tilolang_app_of("shell:layout"), None);
        assert_eq!(super::tilolang_app_of("tl::root"), None);
    }
}
