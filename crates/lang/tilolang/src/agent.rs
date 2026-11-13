//! The agent library, `tilo.agent`: the words a person talks with a workspace through — `grep` over
//! the text of the apps and resources it names, `find` by a type and its filters, `read` of a
//! resource, `describe` of what an app declares, `print` of what the session says back, and `me`,
//! the person the session acts for (agents.md).
//!
//! Its words are typed tilolang declarations whose implementation is the host imports of
//! [`crate::abi::Imp`], which the checker lowers where a word is called; this module is the one
//! place they are named, typed and described. A session has the words in scope without a `use`, the
//! way every package has `std`; a package that wants them writes `use tilo.agent as agent`.

use std::sync::OnceLock;

use crate::meta::{FnMeta, PackageMeta, ParamMeta};

/// The library's package id: what a package `use`s.
pub const APP: &str = "tilo.agent";

pub const NS: &str = "agent";

/// The person the session acts for: a `Principal`, a value rather than a word.
pub const ME: &str = "me";

/// The kinds of block `grep(not: .kind)` leaves out: the text parsers' kinds, the same words `grep`'s
/// matches carry as `host.Match.kind`.
pub const BLOCK_KINDS: &[&str] = &["paragraph", "heading", "quote", "list", "code", "field"];

pub struct Function {
    pub name: &'static str,
    /// What it means, and the forms it is written in, for `describe` and the metadata.
    pub doc: &'static str,
    /// Its arguments in the order written, and each one's type as metadata spells it. A word that also
    /// takes the forms its doc names declares the type of the common one.
    pub params: &'static [(&'static str, &'static str)],
    pub result: &'static str,
}

impl Function {
    #[must_use]
    pub fn id(&self) -> String {
        format!("{APP}-{}", self.name)
    }
}

pub const FUNCTIONS: &[Function] = &[
    Function {
        name: "grep",
        doc: "The matches of a regular expression in the text of the apps and resources named, \
              each a `host.Match`: `grep(/Tide\\b/, in: notes + chat, not: [.quote], \
              changed: -14d, mentions: me)`. With no `in:` it reads every app the code may read.",
        params: &[
            ("pattern", "Text"),
            ("in", "List<host.Resource>"),
            ("not", "List<Text>"),
            ("changed", "Text"),
            ("mentions", "Principal"),
        ],
        result: "List<host.Match>",
    },
    Function {
        name: "find",
        doc: "What a search finds: `find(\"Note launch\")` the palette's query across apps, a \
              `List<host.Resource>` that `.of(T)` narrows; `find(Tag, pinned: true)` a type and \
              its filters, a `List<Tag>`.",
        params: &[("query", "Text")],
        result: "List<host.Resource>",
    },
    Function {
        name: "read",
        doc: "A resource as text, its size first, a page of it, only what matches when asked, \
              with the paragraphs around: `read(n, matching: /x/, around: 1)`, `read(n, page: \
              2)`, `read(n, around: \"¶4\")` the anchor to stand at.",
        params: &[
            ("resource", "host.Resource"),
            ("matching", "Text"),
            ("around", "Int"),
            ("page", "Int"),
        ],
        result: "Text",
    },
    Function {
        name: "describe",
        doc: "An app, a type, an action, a field or a resource as tilolang, and what the code \
              may do with it: `describe(notes)`, `describe(Tag)`, `describe(tag.done)`, \
              `describe(n3)`.",
        params: &[("what", "Text")],
        result: "Text",
    },
    Function {
        name: "print",
        doc: "The values on one line, separated by spaces, each as its type shows; a list with \
              `fields:` a row per item: `print(n.title)`, `print(xs, fields: [.title, .due])`.",
        params: &[("values", "List<Text>"), ("fields", "List<Text>")],
        result: "()",
    },
];

#[must_use]
pub fn function(name: &str) -> Option<&'static Function> {
    FUNCTIONS.iter().find(|f| f.name == name)
}

/// The library as a package: what `use tilo.agent` imports, `agent.grep` calls and `describe` reads. Its
/// words are lowered to host imports, so it carries no module of its own.
#[must_use]
pub fn meta() -> &'static PackageMeta {
    static META: OnceLock<PackageMeta> = OnceLock::new();
    META.get_or_init(|| PackageMeta {
        app: APP.to_owned(),
        title: "Agent".to_owned(),
        fns: FUNCTIONS
            .iter()
            .map(|f| FnMeta {
                id: f.id(),
                name: f.name.to_owned(),
                doc: f.doc.to_owned(),
                params: f
                    .params
                    .iter()
                    .map(|(name, ty)| ParamMeta {
                        name: (*name).to_owned(),
                        ty: (*ty).to_owned(),
                        default: None,
                        place: false,
                        entry: false,
                        label: None,
                    })
                    .collect(),
                result: f.result.to_owned(),
            })
            .collect(),
        ..PackageMeta::default()
    })
}

#[cfg(test)]
mod tests {
    use super::{APP, FUNCTIONS};

    /// Every word the table names is a word: lower case, unique, with the types metadata parses.
    #[test]
    fn the_words_are_typed_declarations() {
        assert_eq!(APP, "tilo.agent");
        let mut seen = Vec::new();
        for f in FUNCTIONS {
            assert!(
                f.name.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "`{}` is a word",
                f.name
            );
            assert!(!seen.contains(&f.name), "`{}` is declared once", f.name);
            seen.push(f.name);
            assert!(!f.doc.is_empty(), "`{}` says what it means", f.name);
            for (name, ty) in f.params {
                assert!(tilolang_ty(ty), "`{}`'s `{name}: {ty}` is a type", f.name);
            }
            assert!(tilolang_ty(f.result), "`{}`'s result is a type", f.name);
        }
        assert_eq!(super::meta().fns.len(), FUNCTIONS.len());
    }

    fn tilolang_ty(text: &str) -> bool {
        text == "()" || crate::types::Ty::parse_meta(text, APP).is_some()
    }
}
