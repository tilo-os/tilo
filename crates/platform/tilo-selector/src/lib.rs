//! Which resources a skill is for: a resource kind, and what the resource's
//! own words say about it.
//!
//! A skill is a person's own instruction for the work they do on kinds of
//! thing they have (reviewing a book, sorting a folder of invoices). It says
//! which things it is for as a structured value over the typed resource
//! model, never as text: a kind the model names, the fields its surface
//! publishes, and the words the resource says in them. The text form
//! ([`Selector::parse`], [`Selector::to_syntax`]) is what a person types and
//! reads; nothing stores it.
//!
//! ```
//! use tilo_selector::{Selector, Subject};
//!
//! struct Book;
//!
//! impl Subject for Book {
//!     fn kind(&self) -> &str {
//!         "ebook.book"
//!     }
//!
//!     fn says(&self, field: &str) -> Vec<&str> {
//!         match field {
//!             "content-type" => vec!["application/epub+zip"],
//!             _ => Vec::new(),
//!         }
//!     }
//! }
//!
//! let selector = Selector::parse(r#"ebook.book { content-type in ["application/epub+zip"] }"#)
//!     .unwrap();
//! assert!(selector.matches(&Book));
//! ```

use serde::{Deserialize, Serialize};

/// The most predicates one selector holds, so text a person (or an agent)
/// typed stays bounded.
pub const MAX_PREDICATES: usize = 16;

/// The most values one predicate holds.
pub const MAX_VALUES: usize = 32;

/// The longest a term, a field name or a value may be.
pub const MAX_TERM_BYTES: usize = 128;

/// One condition on a resource: the field `field` of its surface, by the key
/// the surface publishes, holds one of `values`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Predicate {
    /// The field's key in the resource's surface (`content-type`, `title`).
    pub field: String,
    /// The words the field holds; empty for a field that holds any.
    pub values: Vec<String>,
}

impl Predicate {
    /// `field` holds one of `values`.
    #[must_use]
    pub fn new(field: impl Into<String>, values: impl IntoIterator<Item = String>) -> Self {
        Self {
            field: field.into(),
            values: values.into_iter().take(MAX_VALUES).collect(),
        }
    }
}

/// A resource, read the way a selector reads it: the kind it is of, and the
/// words it says in the fields its surface publishes.
pub trait Subject {
    /// The resource kind, as the model names it (`ebook.book`).
    fn kind(&self) -> &str;

    /// The words this resource says in `field`; empty when its surface
    /// publishes no such field.
    fn says(&self, field: &str) -> Vec<&str>;
}

/// Which resources a skill is for: the resource kinds it names, and the
/// conditions a resource of those kinds meets. A selector naming no kind is
/// for every kind; one with no predicates is for every resource of its
/// kinds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Selector {
    /// The resource kinds, as the model names them (`ebook.book`,
    /// `files.file`). Empty for every kind.
    pub kinds: Vec<String>,
    /// Conditions every match meets.
    pub predicates: Vec<Predicate>,
}

impl Selector {
    /// The resources of `kinds`, whatever their fields.
    #[must_use]
    pub fn kinds(kinds: impl IntoIterator<Item = String>) -> Self {
        Self {
            kinds: kinds.into_iter().take(MAX_VALUES).collect(),
            predicates: Vec::new(),
        }
    }

    /// The same selector, also requiring `predicate`.
    #[must_use]
    pub fn where_(mut self, predicate: Predicate) -> Self {
        if self.predicates.len() < MAX_PREDICATES {
            self.predicates.push(predicate);
        }
        self
    }

    /// Whether a resource meets it. A condition on a field the resource's
    /// surface does not publish is not met: what a resource does not say
    /// about itself cannot make it a match.
    #[must_use]
    pub fn matches(&self, subject: &(impl Subject + ?Sized)) -> bool {
        let kind_matches = self.kinds.is_empty()
            || self
                .kinds
                .iter()
                .any(|kind| kind.as_str() == subject.kind());
        kind_matches
            && self.predicates.iter().all(|predicate| {
                subject
                    .says(&predicate.field)
                    .into_iter()
                    .any(|held| predicate.values.iter().any(|value| value == held))
            })
    }

    /// The selector as a person types and reads it: `ebook.book`,
    /// `files.file { content-type in [application/pdf] }`, several kinds
    /// joined by `|`, several conditions by `and`. Words with a space in them
    /// or a mark of the syntax are quoted.
    #[must_use]
    pub fn to_syntax(&self) -> String {
        let mut out = if self.kinds.is_empty() {
            "*".to_owned()
        } else {
            self.kinds
                .iter()
                .map(|kind| quoted(kind))
                .collect::<Vec<_>>()
                .join(" | ")
        };
        if !self.predicates.is_empty() {
            let predicates = self
                .predicates
                .iter()
                .map(|predicate| {
                    let values: Vec<String> = predicate.values.iter().map(|v| quoted(v)).collect();
                    format!("{} in [{}]", quoted(&predicate.field), values.join(", "))
                })
                .collect::<Vec<_>>()
                .join(" and ");
            out.push_str(" { ");
            out.push_str(&predicates);
            out.push_str(" }");
        }
        out
    }

    /// The selector a person (or an agent) typed, in the form
    /// [`Self::to_syntax`] writes.
    ///
    /// # Errors
    /// Why the text is no selector: what was expected where it stopped
    /// reading.
    pub fn parse(text: &str) -> Result<Self, String> {
        parse::parse(text)
    }
}

/// `word` as the syntax writes it: bare when it reads as itself, quoted
/// otherwise.
fn quoted(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '.' | '_' | ':' | '/' | '@'))
        // A word of digits alone reads back as itself, unquoted.
        && word.parse::<f64>().is_err();
    if plain {
        word.to_owned()
    } else {
        format!("\"{}\"", word.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// Reading [`Selector::to_syntax`] back.
mod parse {
    use super::{MAX_PREDICATES, MAX_TERM_BYTES, MAX_VALUES, Predicate, Selector};

    /// The text, as far as it has been read.
    struct Reader<'a> {
        text: &'a str,
        at: usize,
    }

    impl<'a> Reader<'a> {
        fn new(text: &'a str) -> Self {
            Self { text, at: 0 }
        }

        fn peek(&self) -> Option<char> {
            self.text[self.at..].chars().next()
        }

        fn skip_space(&mut self) {
            while self.peek().is_some_and(char::is_whitespace) {
                self.at += 1;
            }
        }

        fn eat(&mut self, expected: char) -> bool {
            self.skip_space();
            if self.peek() == Some(expected) {
                self.at += expected.len_utf8();
                true
            } else {
                false
            }
        }

        fn take(&mut self, word: &str) -> bool {
            self.skip_space();
            if self.text[self.at..].starts_with(word) {
                self.at += word.len();
                true
            } else {
                false
            }
        }

        /// A word: quoted, or bare up to a mark of the syntax.
        fn word(&mut self) -> Result<String, String> {
            self.skip_space();
            if self.eat('"') {
                let mut out = String::new();
                loop {
                    match self.peek() {
                        Some('"') => {
                            self.at += 1;
                            return Ok(out);
                        }
                        Some('\\') => {
                            self.at += 1;
                            let escaped = self.peek().ok_or("a quoted word ends with \\")?;
                            out.push(escaped);
                            self.at += escaped.len_utf8();
                        }
                        Some(c) => {
                            out.push(c);
                            self.at += c.len_utf8();
                        }
                        None => return Err("a quoted word has no closing \"".to_owned()),
                    }
                    if out.len() > MAX_TERM_BYTES {
                        return Err(format!("a word is at most {MAX_TERM_BYTES} bytes"));
                    }
                }
            }
            let start = self.at;
            while let Some(c) = self.peek() {
                if c.is_alphanumeric() || matches!(c, '-' | '.' | '_' | ':' | '/' | '@') {
                    self.at += c.len_utf8();
                } else {
                    break;
                }
            }
            if self.at == start {
                let rest: String = self.text[self.at..].chars().take(12).collect();
                return Err(format!("expected a name at `{rest}`"));
            }
            Ok(self.text[start..self.at].to_owned())
        }

        /// `[word, word]`.
        fn list(&mut self) -> Result<Vec<String>, String> {
            if !self.eat('[') {
                return Err("expected [ after in".to_owned());
            }
            let mut values = Vec::new();
            loop {
                values.push(self.word()?);
                if values.len() > MAX_VALUES {
                    return Err(format!("at most {MAX_VALUES} values"));
                }
                if self.eat(',') {
                    continue;
                }
                if self.eat(']') {
                    return Ok(values);
                }
                return Err("expected , or ] in a list of values".to_owned());
            }
        }
    }

    pub(super) fn parse(text: &str) -> Result<Selector, String> {
        if text.len() > MAX_TERM_BYTES.saturating_mul(MAX_VALUES) {
            return Err("the selector is too long".to_owned());
        }
        let mut reader = Reader::new(text);
        let mut kinds = Vec::new();
        if reader.eat('*') {
            reader.skip_space();
        } else {
            loop {
                kinds.push(reader.word()?);
                if !reader.eat('|') {
                    break;
                }
            }
        }
        let mut predicates = Vec::new();
        if reader.eat('{') {
            loop {
                let field = reader.word()?;
                if !reader.take("in") {
                    return Err(format!("expected in after the field {field}"));
                }
                let values = reader.list()?;
                predicates.push(Predicate::new(field, values));
                if predicates.len() > MAX_PREDICATES {
                    return Err(format!("at most {MAX_PREDICATES} conditions"));
                }
                if reader.eat(',') || reader.take("and") {
                    continue;
                }
                if reader.eat('}') {
                    break;
                }
                return Err("expected and or } after a condition".to_owned());
            }
        }
        reader.skip_space();
        if reader.at != text.len() {
            return Err(format!(
                "nothing more is expected after `{}`",
                &text[reader.at..]
            ));
        }
        Ok(Selector { kinds, predicates })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A resource of `kind` that says `fields`, and nothing else.
    struct Sample<'a> {
        kind: &'a str,
        fields: &'a [(&'a str, &'a str)],
    }

    impl Subject for Sample<'_> {
        fn kind(&self) -> &str {
            self.kind
        }

        fn says(&self, field: &str) -> Vec<&str> {
            self.fields
                .iter()
                .filter(|(key, _)| *key == field)
                .map(|(_, value)| *value)
                .collect()
        }
    }

    #[test]
    fn a_kind_alone_matches_every_resource_of_it() {
        let selector = Selector::parse("ebook.book").unwrap();
        assert_eq!(selector.kinds, ["ebook.book"]);
        assert!(selector.predicates.is_empty());
        assert!(selector.matches(&Sample {
            kind: "ebook.book",
            fields: &[],
        }));
        assert!(!selector.matches(&Sample {
            kind: "ebook.annotation",
            fields: &[],
        }));
    }

    #[test]
    fn a_condition_on_a_published_field_narrows_a_kind() {
        // What a person types, and what [`Selector::to_syntax`] writes back
        // for it: bare words where they read as themselves.
        let text =
            r#"files.file { "content-type" in ["application/pdf", "application/epub+zip"] }"#;
        let written = r#"files.file { content-type in [application/pdf, "application/epub+zip"] }"#;
        let selector = Selector::parse(text).unwrap();
        assert_eq!(selector.kinds, ["files.file"]);
        assert_eq!(
            selector.predicates,
            [Predicate::new(
                "content-type",
                [
                    "application/pdf".to_owned(),
                    "application/epub+zip".to_owned()
                ]
            )]
        );
        let pdf = Sample {
            kind: "files.file",
            fields: &[("content-type", "application/pdf")],
        };
        let png = Sample {
            kind: "files.file",
            fields: &[("content-type", "image/png")],
        };
        let quiet = Sample {
            kind: "files.file",
            fields: &[],
        };
        assert!(selector.matches(&pdf));
        assert!(!selector.matches(&png));
        assert!(
            !selector.matches(&quiet),
            "a resource that says nothing of the field is no match"
        );
        assert_eq!(selector.to_syntax(), written, "the text form reads back");
    }

    #[test]
    fn kinds_are_a_union_and_conditions_are_joined() {
        let text = r#"ebook.book | ebook.annotation { author in ["Ada"] and year in ["1815"] }"#;
        let written = r#"ebook.book | ebook.annotation { author in [Ada] and year in ["1815"] }"#;
        let selector = Selector::parse(text).unwrap();
        assert_eq!(selector.kinds, ["ebook.book", "ebook.annotation"]);
        assert_eq!(selector.predicates.len(), 2);
        let ada = Sample {
            kind: "ebook.annotation",
            fields: &[("author", "Ada"), ("year", "1815")],
        };
        let other = Sample {
            kind: "ebook.annotation",
            fields: &[("author", "Ada"), ("year", "1818")],
        };
        let book = Sample {
            kind: "ebook.book",
            fields: &[("author", "Ada"), ("year", "1815")],
        };
        assert!(selector.matches(&ada));
        assert!(!selector.matches(&other));
        assert!(
            selector.matches(&book),
            "a kind in the union matches whatever its fields say"
        );
        assert_eq!(selector.to_syntax(), written);
    }

    #[test]
    fn a_star_names_every_kind() {
        let selector = Selector::parse("*").unwrap();
        assert!(selector.kinds.is_empty());
        assert!(selector.matches(&Sample {
            kind: "anything.at-all",
            fields: &[],
        }));
        assert_eq!(selector.to_syntax(), "*");
    }

    #[test]
    fn text_that_is_no_selector_says_what_was_expected() {
        for text in [
            "",
            "ebook.book {",
            "ebook.book { type }",
            r#"ebook.book { type in ["a""#,
            "ebook.book { type in [pdf] } trailing",
        ] {
            assert!(Selector::parse(text).is_err(), "{text:?} is no selector");
        }
    }

    #[test]
    fn words_with_spaces_survive_a_round_trip() {
        let selector = Selector::kinds(["files.file".to_owned()])
            .where_(Predicate::new("title", ["Notes for Monday".to_owned()]));
        let text = selector.to_syntax();
        assert_eq!(Selector::parse(&text).unwrap(), selector);
    }
}
