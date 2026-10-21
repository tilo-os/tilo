//! When a generic field edit asks for confirmation.
//!
//! Every door decides it the same way, from the edit alone: no app code is
//! needed to read a field edit. Removing an item of a collection asks, as a
//! delete verb does, unless the record publishes that collection's removals
//! as quiet. Any edit of a field the record publishes as guarded asks: one
//! whose change can weaken the person's security or expose them. Otherwise
//! setting, splicing, adding, moving and an outbox's `settle` and `forget`
//! ask nothing.

/// The tag a published field edit carries.
pub const FIELD_EDIT_TAG: &str = "confirmation:field-edit";

/// The tag prefix naming one collection, by its dotted path from the edit's
/// root (`*` for an item), whose removals ask nothing.
pub const QUIET_REMOVAL_TAG: &str = "confirmation:quiet-removal:";

/// The tag prefix naming one field, by its dotted path from the edit's root
/// (`*` for an item), any edit at or below which asks.
pub const GUARDED_FIELD_TAG: &str = "confirmation:guarded-field:";

/// One step of a field edit's path, as the decision reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditStep<'a> {
    /// A field of a record, by its key.
    Field(&'a str),
    /// An item, an entry or a cell, whichever it is.
    Item,
}

/// `path` as the dotted pattern [`QUIET_REMOVAL_TAG`] names collections by.
#[must_use]
pub fn dotted(path: &[EditStep<'_>]) -> String {
    path.iter()
        .map(|step| match step {
            EditStep::Field(key) => *key,
            EditStep::Item => "*",
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// Whether the field edit `op` (its JSON name) at `path` asks for
/// confirmation before it runs, where `quiet` names the collections whose
/// removals ask nothing and `guarded` the fields whose every edit asks.
#[must_use]
pub fn field_edit_asks(op: &str, path: &[EditStep<'_>], quiet: &[&str], guarded: &[&str]) -> bool {
    guarded_at(path, guarded) || (op == "remove-item" && !quiet.contains(&dotted(path).as_str()))
}

/// Whether `path` is one of the `guarded` fields or lies below one.
#[must_use]
pub fn guarded_at(path: &[EditStep<'_>], guarded: &[&str]) -> bool {
    (1..=path.len())
        .filter_map(|len| path.get(..len))
        .any(|prefix| guarded.contains(&dotted(prefix).as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_loud_collections_removal_asks() {
        let notes = [EditStep::Field("notes")];
        let tags = [
            EditStep::Field("notes"),
            EditStep::Item,
            EditStep::Field("tags"),
        ];
        let quiet = ["notes.*.tags"];
        assert!(field_edit_asks("remove-item", &notes, &quiet, &[]));
        assert!(!field_edit_asks("remove-item", &tags, &quiet, &[]));
        for op in [
            "set",
            "splice",
            "insert-item",
            "move-to",
            "settle",
            "forget",
            "remove-key",
        ] {
            assert!(!field_edit_asks(op, &notes, &quiet, &[]), "{op}");
        }
        assert_eq!(dotted(&tags), "notes.*.tags");
    }

    #[test]
    fn every_edit_at_or_below_a_guarded_field_asks() {
        let lock = [EditStep::Field("lock")];
        let delay = [EditStep::Field("lock"), EditStep::Field("delay")];
        let other = [EditStep::Field("locked")];
        let guarded = ["lock"];
        for op in ["set", "splice", "insert-item", "remove-item"] {
            assert!(field_edit_asks(op, &lock, &[], &guarded), "{op}");
            assert!(field_edit_asks(op, &delay, &[], &guarded), "{op}");
        }
        assert!(!field_edit_asks("set", &other, &[], &guarded));
        assert!(
            field_edit_asks("remove-item", &lock, &["lock"], &guarded),
            "a quiet removal of a guarded field still asks"
        );
    }
}
