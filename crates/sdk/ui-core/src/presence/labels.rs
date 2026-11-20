//! What a member is called: the one rule every surface that names a member — chat lines,
//! cursor pills, people lists, the top bar, mentions — follows.
//!
//! A name is only ever the name. Two members of a workspace may go by the same one; each is then
//! still shown by it, and their faces and colours tell them apart, never a number or a tag joined
//! to the name. A person nobody named is someone without a name yet, never their key.

/// What a person who gave no name is called, on every surface that names them (people lists,
/// cards, cursors, chat): one phrase, never their key.
#[must_use]
pub fn unnamed_person_label() -> String {
    crate::i18n::widget_text("person-unnamed")
}

/// [`unnamed_person_label`] where only a few words fit (a card's chips, a holder's pill): the
/// same person, said shorter. A list wide enough says the whole phrase.
#[must_use]
pub fn unnamed_person_short_label() -> String {
    crate::i18n::widget_text("person-unnamed-short")
}

/// What `name` shows as where a person is named: the name they gave, else the words that stand
/// for a person who gave none ([`unnamed_person_label`]). The one place a person's shown name is
/// decided; their face beside it is the person glyph.
#[must_use]
pub fn person_label(name: &str) -> String {
    if is_unnamed_person(name) {
        unnamed_person_label()
    } else {
        name.trim().to_owned()
    }
}

/// Whether `name` is no name of a person's: blank, a key's hex, or the words that stand in for
/// a person who gave none ([`unnamed_person_label`], [`unnamed_person_short_label`]). None of
/// those is read as a name: nobody is shown a key, and two people nobody named are not namesakes
/// of each other. Their face is then the person glyph, never initials of those words.
#[must_use]
pub fn is_unnamed_person(name: &str) -> bool {
    shown_name(name)
        .is_none_or(|name| name == unnamed_person_label() || name == unnamed_person_short_label())
}

/// What `user` is called while nobody named them: a person is someone without a name yet, never
/// their key; an agent or a bot goes by its id.
#[must_use]
pub fn unnamed_label(user: &crate::permissions::MemberId) -> String {
    match user {
        crate::permissions::MemberId::Person(_) => unnamed_person_label(),
        crate::permissions::MemberId::Agent(id) | crate::permissions::MemberId::Bot(id) => {
            id.clone()
        }
    }
}

/// `name` where it is a name to show: not blank, and not the hex of a key, which is how a
/// person is known before they named themselves and is never read as their name. Whether it is
/// the words said in place of a name too is [`is_unnamed_person`]'s to say.
#[must_use]
pub fn shown_name(name: &str) -> Option<&str> {
    let name = name.trim();
    let key = name.len() >= 32 && name.bytes().all(|byte| byte.is_ascii_hexdigit());
    (!name.is_empty() && !key).then_some(name)
}
