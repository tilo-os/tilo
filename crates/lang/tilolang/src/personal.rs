//! A field declared `personal` holds one value per person, in a record everyone shares: the
//! day a person picked a shared task for, the order they put their place in, the day they
//! snoozed a shared mail to. Reading it gives the value of the person looking, a filter or an
//! order reads their value, and an action writes the value of the one who runs it.
//!
//! The record stores them in the field's own key, a map from a person's id to their value:
//! `{ "person:ada": true }`. It is an ordinary register of the record, so it merges, comes back
//! through undo, stands in history as the change of the person who made it, and syncs as every
//! other field does. What a person never set reads as the field's default.
//!
//! A value read for one person is addressed by the field's id and the person's: `id@person`. The
//! host's index keeps an entry per person under that id, so a filter, a count or an order on a
//! personal field is answered from the index for whoever looks, and the values of everyone else
//! stay readable ([`crate::abi::Imp::Others`]).
//!
//! The field's id never holds a `@`: ids are drawn from the alphanumeric alphabet of
//! `app.ids`, and a name in a column is the id it is kept under.

/// What separates a personal field's id from the person it names.
pub const PERSON: char = '@';

/// The id a personal field `field` reads for `person`.
#[must_use]
pub fn id(field: &str, person: &str) -> String {
    format!("{field}{PERSON}{person}")
}

/// The field and the person an id names, for one a personal value is kept under.
#[must_use]
pub fn split(id: &str) -> Option<(&str, &str)> {
    match id.split_once(PERSON) {
        Some((field, person)) if !field.is_empty() && !person.is_empty() => Some((field, person)),
        _ => None,
    }
}

/// The field an id names: the id itself, or the field a personal value is kept under.
#[must_use]
pub fn field_of(id: &str) -> &str {
    split(id).map_or(id, |(field, _)| field)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_personal_id_names_its_field_and_person() {
        assert_eq!(id("g6kykjp2", "person:ada"), "g6kykjp2@person:ada");
        assert_eq!(
            split("g6kykjp2@person:ada"),
            Some(("g6kykjp2", "person:ada"))
        );
        assert_eq!(split("g6kykjp2"), None);
        assert_eq!(field_of("g6kykjp2@person:ada"), "g6kykjp2");
        assert_eq!(field_of("g6kykjp2"), "g6kykjp2");
    }
}
