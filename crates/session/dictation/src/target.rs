//! The field side of a dictation, as the compositor offers it: which field the
//! person is typing into, and how committed text gets into it.
//!
//! Dictation never writes into a field it has not bound and re-checked: the
//! compositor names the field and the revision of what its client last
//! committed, and refuses the text if either moved in between.

/// The field the person typed into when they started dictating.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    /// The field's identity within the compositor's life.
    pub id: u64,
    /// The revision of its client's committed state: any commit since moves it.
    pub revision: u64,
    /// The field takes more than one line, so a line feed may be dictated.
    pub multiline: bool,
}

/// Why dictated text may not be written into the field the person is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldRefusal {
    /// No field has the focus and asks for typing.
    NoField,
    /// The field takes a secret: a password, a PIN, digits, hidden text.
    Secret,
    /// The field is a terminal's, where spoken words would be commands.
    Unsupported,
}

/// What became of committed text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Insertion {
    /// The field took the text, once.
    Inserted,
    /// The field is no longer the one dictation bound: nothing was inserted.
    Changed,
    /// The field may not be dictated into any more.
    Refused(FieldRefusal),
}

/// The session's fields, behind the compositor.
pub trait Target {
    /// The field the person types into, if dictation may write to it.
    fn field(&mut self) -> Result<Field, FieldRefusal>;

    /// Write `text` into `field` if it is still the field the person types
    /// into, and only then.
    fn insert(&mut self, field: Field, text: &str) -> Insertion;
}
