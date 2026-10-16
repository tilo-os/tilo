//! Who may call a verb. Checked inside the verb, so every replica refuses a
//! change by the wrong kind of caller on replay, not only the host that
//! admitted it (bots.md §6).

use ui::tilo_actions::ActorKind;
use ui::tilo_resource::{ActionCx, Refusal};

/// The caller is a bot.
///
/// # Errors
/// Anyone else.
pub fn bot(cx: &ActionCx) -> Result<(), Refusal> {
    if cx.actor_kind() == Some(ActorKind::Bot) {
        Ok(())
    } else {
        Err(Refusal::new("only the bridge bot does this"))
    }
}

/// The caller is a person acting for themselves; returns their id.
///
/// # Errors
/// A bot, agent, Shortcut or app, or no author.
pub fn person(cx: &ActionCx) -> Result<String, Refusal> {
    match (cx.actor_kind(), cx.author()) {
        (None | Some(ActorKind::Person), Some(author)) => Ok(author.to_owned()),
        _ => Err(Refusal::new("only a person does this, for themselves")),
    }
}
