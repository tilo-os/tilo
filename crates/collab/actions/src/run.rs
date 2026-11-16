//! A person's run: the changes of one continuous act, which History lists as
//! one row and undo takes back as one step.
//!
//! Setting a value with a verb runs on: consecutive calls of one verb on one
//! item, each setting a different value, by one person within
//! [`TYPING_PAUSE_MS`] of each other (each keystroke of a title that renames
//! its note, each value a slider passes through). The executor writes such a
//! call into the gesture of the call it continues ([`continued_gesture`]), so
//! undo takes the run back as one step and History lists it as one row: both
//! read the gesture, neither decides the run again.
//!
//! Only a call that sets a different value continues: calling a verb again
//! with the same arguments (adding another note under the same one, a toggle
//! pressed twice) is a new act each time, never folded into the one before.
//! A call that names no value writes a field's text ([`EDITED_FIELD`]): its
//! keystrokes are the typing burst's to group, so it neither continues a run
//! nor starts one. Any change in between, by anyone, ends the run: a later
//! call no longer starts from what the run set.
//!
//! Typing into a field is the other run History reads: it lists a person's
//! words in one field as one row, while undo keeps a step per word
//! (`collaboration::history_source::typing`). It uses the same pause.

use crate::action::{ActionId, Doc};
use crate::context::ActionContext;
use crate::entry::{Did, VerbCall};

/// How long a person may stop and still be doing the same thing: long
/// enough to think through the rest of a formula, a sentence or a name,
/// short enough that coming back to it later is a change of its own.
pub const TYPING_PAUSE_MS: i64 = 30_000;

/// Whether a change at `next` is within the pause of one at `previous`
/// (both in milliseconds). A change committed without a time is no pause:
/// splitting on it would split differently per peer.
#[must_use]
pub fn within_pause(previous: i64, next: i64) -> bool {
    previous <= 0 || next <= 0 || next.saturating_sub(previous) <= TYPING_PAUSE_MS
}

/// Whether `context` is a change a person made as part of a gesture in a
/// window. A step of its own (an agent's call, a script's) runs on with
/// nothing.
#[must_use]
pub fn by_a_person(context: &ActionContext) -> bool {
    context.gesture.is_some()
        && context.compensation.is_none()
        && context
            .invoker
            .as_ref()
            .is_none_or(|invoker| invoker.acting.is_person())
}

/// The verb call `context` sets a value with and the item it sets it on,
/// when it is a person's call of a verb on a named item.
#[must_use]
pub fn setting(context: &ActionContext) -> Option<(&VerbCall, &str)> {
    if !by_a_person(context) {
        return None;
    }
    match (context.what.as_ref()?, context.item.as_ref()) {
        (Did::Verb(call), Some(item)) => Some((call, item.id.as_str())),
        _ => None,
    }
}

/// The argument a change that writes a field's text names the field it wrote with. Such a
/// change names no value: what the person typed is the change itself, so its keystrokes are
/// the typing burst's to group ([`TYPING_PAUSE_MS`]), never a run of setting one value.
pub const EDITED_FIELD: &str = "field";

/// Whether `call` names the value it sets: a call of a verb on an item whose arguments say
/// which value it is, rather than just the field it wrote.
fn names_value(call: &VerbCall) -> bool {
    call.args.keys().any(|name| name != EDITED_FIELD)
}

/// Whether `next`, made at `next_ms` right after `previous` (made at
/// `previous_ms`) by the same writer with nothing in between, continues its
/// run: the same verb on the same item, setting a different value, within
/// the pause.
#[must_use]
pub fn continues_setting(
    previous: &ActionContext,
    previous_ms: i64,
    next: &ActionContext,
    next_ms: i64,
) -> bool {
    let (Some((before, item)), Some((after, on))) = (setting(previous), setting(next)) else {
        return false;
    };
    names_value(before)
        && names_value(after)
        && before.action == after.action
        && item == on
        && before.args != after.args
        && within_pause(previous_ms, next_ms)
}

/// The gesture of the run the change `context`, written by `actor` at
/// `time_secs`, continues on `doc`: the gesture of the document's only head
/// when that head is the writer's own and `context` continues it
/// ([`continues_setting`]). A function of the document and the change, so
/// every device reads the same run.
pub(crate) fn continued_gesture(
    doc: &Doc,
    actor: &[u8],
    context: &ActionContext,
    time_secs: i64,
) -> Option<ActionId> {
    setting(context)?;
    let heads = doc.committed_heads();
    let [head] = heads.as_slice() else {
        return None;
    };
    let document = doc.committed_document();
    let meta = document.get_change_meta_by_hash(head)?;
    if meta.actor.to_bytes() != actor {
        return None;
    }
    let previous = crate::executor::entry_of(document, *head)?;
    continues_setting(
        &previous,
        meta.timestamp.saturating_mul(1000),
        context,
        time_secs.saturating_mul(1000),
    )
    .then_some(previous.gesture)
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{ActionItem, Scalar};
    use crate::key::ActionKey;

    fn rename(item: &str, title: &str) -> ActionContext {
        ActionContext {
            item: Some(ActionItem {
                id: item.into(),
                label: String::new(),
            }),
            what: Some(Did::verb(
                ActionKey::new("notes", "notebook", "rename"),
                [("title".to_owned(), Scalar::text(title))],
            )),
            gesture: Some(ActionId::new()),
            ..ActionContext::default()
        }
    }

    #[test]
    fn setting_one_value_again_runs_on_until_the_person_stops() {
        let first = rename("note", "R");
        assert!(continues_setting(
            &first,
            1_000,
            &rename("note", "Ro"),
            2_000
        ));
        assert!(
            !continues_setting(
                &first,
                1_000,
                &rename("note", "Ro"),
                2_000 + TYPING_PAUSE_MS
            ),
            "a pause ends it"
        );
        assert!(
            !continues_setting(&first, 1_000, &rename("other", "Ro"), 2_000),
            "another item is another act"
        );
        assert!(
            !continues_setting(&first, 1_000, &rename("note", "R"), 2_000),
            "the same call again is a new act"
        );
        let mut agents = rename("note", "Ro");
        agents.gesture = None;
        assert!(
            !continues_setting(&first, 1_000, &agents, 2_000),
            "a step of its own runs on with nothing"
        );
    }

    /// A keystroke of a bound field names the field it wrote and no value: two fields of one
    /// note typed one after the other are two typing bursts, not a run of setting one value.
    fn typed(field: &str) -> ActionContext {
        ActionContext {
            item: Some(ActionItem {
                id: "note".into(),
                label: String::new(),
            }),
            what: Some(Did::verb(
                ActionKey::new("notes", "notebook", "edit-text"),
                [(EDITED_FIELD.to_owned(), Scalar::text(field))],
            )),
            gesture: Some(ActionId::new()),
            ..ActionContext::default()
        }
    }

    #[test]
    fn typing_another_field_does_not_continue_the_run_of_the_one_before() {
        let keystroke = typed("title");
        let (call, _) = setting(&keystroke).expect("a keystroke is a person's setting");
        assert!(!names_value(call), "a keystroke names no value");
        assert!(
            !continues_setting(&typed("title"), 1_000, &typed("body"), 2_000),
            "another field is another burst"
        );
    }
}
