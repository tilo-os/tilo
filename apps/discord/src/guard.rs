//! Who may call a verb. Checked inside the verb, so every replica refuses a
//! change by the wrong kind of caller on replay, not only the host that
//! admitted it (bots.md §6).

use ui::tilo_resource::{ActionCx, Refusal};

/// The caller is a bot.
///
/// # Errors
/// Anyone else.
pub fn bot(cx: &ActionCx) -> Result<(), Refusal> {
    if cx.invoked_by().is_some_and(|by| by.is_bot()) {
        Ok(())
    } else {
        Err(Refusal::new("only the bridge bot does this"))
    }
}

/// The caller is a person acting for themselves; returns their id.
pub fn person(cx: &ActionCx) -> Result<String, Refusal> {
    let by_person = cx.invoked_by().is_none_or(|by| by.is_person());
    match cx.author() {
        Some(author) if by_person => Ok(author.to_owned()),
        _ => Err(Refusal::new("only a person does this, for themselves")),
    }
}
