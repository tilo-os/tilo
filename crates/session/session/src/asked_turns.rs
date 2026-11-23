//! The turn an ask from the canvas opened, read back from Chat's record (canvas.md §3): the
//! agent's answer to the person's message, which is the turn. The answer is a message of its own,
//! naming the invocation it answers in (`turn`) and the person's message it answers
//! (`reply_to`); it stands where the person asked through Chat's `turn` view, which draws what
//! they wrote above the answer as it is written, and its handle's open stands the conversation
//! beside it.
//!
//! Everything here is read through the session's services, as any reader of an app's records
//! reads them: a message's reading names what holds it ([`HELD_IN`]) and what links to it, and
//! the answer links to the person's message by its `reply_to` field. Nothing passes between Chat
//! and the shell for it beyond those records.

use serde_json::Value;
use tilolang_tilo::resource_service::HELD_IN;
use tilolang_tilo::runtime::store::LINKS;

/// The app and record id a published id (`tl:<app>:<id>`) names.
fn published_parts(published: &str) -> Option<(&str, &str)> {
    let (app, id) = published.strip_prefix("tl:")?.split_once(':')?;
    (!app.is_empty() && !id.is_empty()).then_some((app, id))
}

/// The message of the person's message's own app that answers it, as `reading` (the person's
/// message's) reads what links to it: one naming it in a field, its `reply_to`. By its published
/// id.
fn answer_in<'a>(reading: &'a Value, app: &str) -> Option<&'a str> {
    let message = format!("{app}#Message");
    reading
        .get(LINKS)?
        .as_array()?
        .iter()
        .filter(|link| link["how"] == "field" && link["ty"].as_str() == Some(message.as_str()))
        .filter_map(|link| link.get("ref")?.as_str())
        .last()
}

/// Whether `resource` is `id` or holds it among its children, at any depth.
fn holds(resource: &tilo_actions::ResourceSpec, id: &str) -> bool {
    resource.id.as_str() == id || resource.children.iter().any(|child| holds(child, id))
}

/// The turn the person's message `message` (its published id, as Chat answered the ask's send
/// with it) opened, once Chat's record holds the agent's answer to it and the services publish
/// it: by the conversation and the answer, and the answer and the conversation as their app
/// publishes them, with the app's tree the services publish the answer in, for the layout to
/// place before the API's tree takes it in. `None` until then: the ask keeps waiting.
pub(crate) fn asked_turn(
    services: &tilo_service::Services,
    message: &str,
) -> Option<(
    tilo_session_core::layout::Asked,
    Vec<tilo_actions::ResourceSpec>,
)> {
    let (app, _) = published_parts(message)?;
    let reading = services.read(message)?;
    let answer = answer_in(&reading, app)?;
    let conversation = reading.get(HELD_IN)?.get("ref")?.as_str()?;
    let asked = tilo_session_core::layout::Asked {
        conversation: published_parts(conversation)?.1.to_owned(),
        message: published_parts(answer)?.1.to_owned(),
        turn: answer.to_owned(),
        opened_as: conversation.to_owned(),
    };
    let published = services
        .resources()
        .into_iter()
        .find(|tree| holds(tree, &asked.turn))?;
    Some((asked, vec![published]))
}
