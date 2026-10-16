//! When a generic field edit asks for confirmation.
//!
//! Every door decides it the same way, from the edit alone: no app code is
//! needed to read a field edit. Only removing an item of a collection asks,
//! as a delete verb does, unless the record publishes that collection's
//! removals as quiet. Setting, splicing, adding, moving and an outbox's
//! `settle` and `forget` ask nothing.

/// The tag a published field edit carries.
pub const FIELD_EDIT_TAG: &str = "confirmation:field-edit";

/// The tag prefix naming one collection, by its dotted path from the edit's
/// root (`*` for an item), whose removals ask nothing.
pub const QUIET_REMOVAL_TAG: &str = "confirmation:quiet-removal:";

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
/// removals ask nothing.
#[must_use]
pub fn field_edit_asks(op: &str, path: &[EditStep<'_>], quiet: &[&str]) -> bool {
    op == "remove-item" && !quiet.contains(&dotted(path).as_str())
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
        assert!(field_edit_asks("remove-item", &notes, &quiet));
        assert!(!field_edit_asks("remove-item", &tags, &quiet));
        for op in [
            "set",
            "splice",
            "insert-item",
            "move-to",
            "settle",
            "forget",
            "remove-key",
        ] {
            assert!(!field_edit_asks(op, &notes, &quiet), "{op}");
        }
        assert_eq!(dotted(&tags), "notes.*.tags");
    }
}
