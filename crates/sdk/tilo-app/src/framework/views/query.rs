//! What a query view asks: the typed text and the chips that narrow it.
//!
//! The chips come from the types' filterable fields. A person picks them
//! under the search field, or types them as tokens (`artist:ada`,
//! `liked:yes`); both are the same [`Chip`]s.

use serde::{Deserialize, Serialize};

use super::kind::{FieldDecl, FilterKind, Item, KindDecl, filters};

/// One chip: the filterable field it narrows by, and the value for a value
/// chip (none for a toggle that is on).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Chip {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Typed into the field as a token rather than picked: the token in the
    /// field is what takes it away again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub typed: bool,
}

impl Chip {
    /// The value chip `key: value`.
    #[must_use]
    pub fn value(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: Some(value.into()),
            typed: false,
        }
    }

    #[must_use]
    pub fn toggle(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: None,
            typed: false,
        }
    }

    /// Whether `item` passes this chip. An item of a type without the field
    /// does not pass: narrowing by Artist leaves the artists' things.
    #[must_use]
    pub fn admits(&self, item: &Item) -> bool {
        match &self.value {
            None => item.is_on(&self.key),
            Some(wanted) => {
                let wanted = fold(wanted);
                item.values(&self.key)
                    .any(|value| value_matches(&fold(value), &wanted))
            }
        }
    }

    /// The chip as a person reads it, given its field: "Artist: Ada",
    /// "Liked".
    #[must_use]
    pub fn label(&self, field: Option<&FieldDecl>) -> String {
        let name = field.map_or(self.key.as_str(), |field| field.label.as_str());
        match &self.value {
            Some(value) => format!("{name}: {value}"),
            None => name.to_owned(),
        }
    }
}

/// A picked or typed value matches a field's value at the start of one of
/// its words: `ada` finds "Ada Lovelace" and "The Ada Trio", not "Canada".
fn value_matches(value: &str, wanted: &str) -> bool {
    if wanted.is_empty() {
        return true;
    }
    value.starts_with(wanted)
        || value.match_indices(wanted).any(|(at, _)| {
            value
                .get(..at)
                .is_some_and(|before| before.ends_with(|c: char| !c.is_alphanumeric()))
        })
}

fn fold(text: &str) -> String {
    tilo_search::fold(text.trim())
}

/// What a query view asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QuerySpec {
    /// The words typed, tokens taken out.
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chips: Vec<Chip>,
}

impl QuerySpec {
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            chips: Vec::new(),
        }
    }

    /// The query `input` asks, as typed into the field, with the chips
    /// picked beside it: a token naming a filterable field of `kinds`
    /// (`artist:ada`, `artist:"ada lovelace"`, `liked:yes`) becomes a typed
    /// chip, everything else is text. A token naming no such field, or one
    /// with no value yet (`artist:`), stays text.
    #[must_use]
    pub fn parse(input: &str, picked: &[Chip], kinds: &[KindDecl]) -> Self {
        let filters = filters(kinds);
        let mut text = Vec::new();
        let mut chips: Vec<Chip> = picked.iter().filter(|chip| !chip.typed).cloned().collect();
        for token in tokens(input) {
            match typed_chip(&token, &filters) {
                Some(chip) => {
                    if !chips.contains(&Chip {
                        typed: false,
                        ..chip.clone()
                    }) && !chips.contains(&chip)
                    {
                        chips.push(chip);
                    }
                }
                None => text.push(token.raw),
            }
        }
        Self {
            text: text.join(" "),
            chips,
        }
    }

    /// Nothing is asked: no text and no chip.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.chips.is_empty()
    }

    /// Whether `item` passes the chips: one of each filter's chosen values
    /// (Artist: Ada or Grace), and every filter.
    #[must_use]
    pub fn admits(&self, item: &Item) -> bool {
        self.chips.iter().all(|chip| {
            self.chips
                .iter()
                .filter(|other| other.key == chip.key)
                .any(|other| other.admits(item))
        })
    }

    /// The query with `chip` chosen too, or taken away when it is chosen. A
    /// filter may hold several values; a toggle is on or off.
    #[must_use]
    pub fn toggled(&self, chip: &Chip) -> Self {
        let mut next = self.clone();
        let same = |held: &Chip| held.key == chip.key && held.value == chip.value;
        if let Some(at) = next.chips.iter().position(same) {
            next.chips.remove(at);
        } else {
            next.chips.push(Chip {
                typed: false,
                ..chip.clone()
            });
        }
        next
    }

    /// The query without the chosen values of the filter `key`; one typed as
    /// a token stays, the token in the field takes it away.
    #[must_use]
    pub fn cleared(&self, key: &str) -> Self {
        let mut next = self.clone();
        next.chips.retain(|chip| chip.key != key || chip.typed);
        next
    }

    /// What two equal queries share, however they were typed: the folded
    /// words and the chips in a fixed order. Caches are keyed by it.
    #[must_use]
    pub fn key(&self) -> String {
        let mut chips: Vec<String> = self
            .chips
            .iter()
            .map(|chip| {
                format!(
                    "{}={}",
                    chip.key,
                    chip.value.as_deref().map(fold).unwrap_or_default()
                )
            })
            .collect();
        chips.sort();
        format!(
            "{}\u{1f}{}",
            words(&self.text).join(" "),
            chips.join("\u{1f}")
        )
    }

    /// Whether this query narrows `earlier`: the same chips, and text that
    /// goes on from its text. What `earlier` found that still matches is
    /// what this one finds first.
    #[must_use]
    pub fn refines(&self, earlier: &Self) -> bool {
        let (mine, theirs) = (fold(&self.text), fold(&earlier.text));
        !theirs.is_empty()
            && mine.len() > theirs.len()
            && mine.starts_with(&theirs)
            && same_chips(&self.chips, &earlier.chips)
    }

    /// Whether `item` matches the text of this query by its searchable
    /// fields: every word occurs at a word start in one of them. What a
    /// refined query keeps of an earlier answer before its own arrives.
    #[must_use]
    pub fn text_admits(&self, item: &Item, kind: Option<&KindDecl>) -> bool {
        let wanted = words(&self.text);
        if wanted.is_empty() {
            return true;
        }
        let fields: Vec<String> = item
            .fields
            .iter()
            .filter(|(key, _)| {
                kind.and_then(|kind| kind.field_of(key))
                    .is_none_or(|field| field.searchable)
            })
            .map(|(_, value)| fold(value))
            .collect();
        wanted
            .iter()
            .all(|word| fields.iter().any(|field| value_matches(field, word)))
    }
}

fn same_chips(a: &[Chip], b: &[Chip]) -> bool {
    let strip = |chips: &[Chip]| {
        let mut out: Vec<(String, Option<String>)> = chips
            .iter()
            .map(|chip| (chip.key.clone(), chip.value.as_deref().map(fold)))
            .collect();
        out.sort();
        out
    };
    strip(a) == strip(b)
}

fn words(text: &str) -> Vec<String> {
    fold(text).split_whitespace().map(str::to_owned).collect()
}

/// One whitespace-separated token of the input, with its key and value when
/// it has the `key:value` shape.
struct Token {
    raw: String,
    key: Option<String>,
    value: String,
}

/// The input's tokens. A value in double quotes may hold spaces
/// (`artist:"ada lovelace"`).
fn tokens(input: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut chars = input.char_indices().peekable();
    while let Some(&(start, c)) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        let mut end = start;
        let mut quoted = false;
        while let Some(&(at, c)) = chars.peek() {
            if c == '"' {
                quoted = !quoted;
            } else if c.is_whitespace() && !quoted {
                break;
            }
            end = at.saturating_add(c.len_utf8());
            chars.next();
        }
        let raw = input.get(start..end).unwrap_or_default().to_owned();
        let (key, value) = match raw.split_once(':') {
            Some((key, value)) if !key.is_empty() && !key.contains('"') => (
                Some(key.to_owned()),
                value.trim_matches('"').trim().to_owned(),
            ),
            _ => (None, String::new()),
        };
        out.push(Token { raw, key, value });
    }
    out
}

/// The chip a token names, when its key is a filterable field (by key or by
/// its label) and it has a value.
fn typed_chip(token: &Token, filters: &[&FieldDecl]) -> Option<Chip> {
    let key = fold(token.key.as_deref()?);
    let field = filters
        .iter()
        .find(|field| fold(field.key) == key || fold(&field.label) == key)?;
    if token.value.is_empty() {
        return None;
    }
    match field.filter? {
        FilterKind::Value => Some(Chip {
            key: field.key.to_owned(),
            value: Some(token.value.clone()),
            typed: true,
        }),
        FilterKind::Toggle => {
            let on = matches!(
                fold(&token.value).as_str(),
                "yes" | "true" | "on" | "1" | "y"
            );
            on.then(|| Chip {
                key: field.key.to_owned(),
                value: None,
                typed: true,
            })
        }
    }
}
