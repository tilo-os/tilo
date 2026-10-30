//! What the toolkit's sources declare about components, read from the sources themselves: the
//! build script generates the registry of described widgets from it (`component::registry`,
//! `component::parts`), and the coverage check (`tests/component_coverage.rs`) holds every public
//! widget to being described or saying why not. One reading, so the two cannot disagree.
//!
//! The declarations it reads, each where the component is written:
//!
//! - `impl Described for W` (with its `type Props = P;`): a described widget;
//! - `#[derive(Component)] struct P`: a component's props; one that no described widget takes is
//!   a part, standing only in a slot of another component;
//! - `pub fn entry() -> component::Entry` in a public module: a described widget whose entry is
//!   written by hand, naming its props by `P::NAME`;
//! - `impl Widget for X` of a `pub struct X` its top module re-exports: a public widget;
//! - `#[not_a_component(kind, "why")]` on a public widget: why it is not a component.
//!
//! Test code (`#[cfg(test)]` modules, `tests.rs`, `*_tests.rs`) is not read.

#![allow(
    clippy::string_slice,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "source text is cut at offsets found in that same text, at ASCII delimiters"
)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// A `pub struct` found: its module path, name, file and `not_a_component`.
type FoundStruct = (Vec<String>, String, PathBuf, Option<(String, String)>);

/// A described widget: the registry builds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DescribedWidget {
    /// Its top module under the crate root (`widget`, `search`), which re-exports it.
    pub top: String,
    /// The widget type as its impl names it.
    pub widget: String,
    /// The props type it is built from.
    pub props: String,
    pub file: PathBuf,
}

/// A component's props.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Props {
    pub top: String,
    pub name: String,
    pub file: PathBuf,
}

/// A hand-written entry: `crate::<module>::entry()`, for the props it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomEntry {
    /// The module path under the crate root, `::`-separated.
    pub module: String,
    pub props: Vec<String>,
    pub file: PathBuf,
}

/// A public widget, and what its declaration says it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicWidget {
    pub name: String,
    pub file: PathBuf,
    /// The kind and the reason of its `#[not_a_component]`, when it has one.
    pub exempt: Option<(String, String)>,
}

/// Everything the sources declare.
#[derive(Clone, Debug, Default)]
pub struct Scan {
    pub described: Vec<DescribedWidget>,
    pub props: Vec<Props>,
    pub custom: Vec<CustomEntry>,
    pub widgets: Vec<PublicWidget>,
    /// Every source file read, for the build script to watch.
    pub files: Vec<PathBuf>,
}

impl Scan {
    /// The props no described widget or hand-written entry is built from: parts.
    pub fn parts(&self) -> Vec<&Props> {
        self.props
            .iter()
            .filter(|p| {
                !self.described.iter().any(|d| d.props == p.name)
                    && !self.custom.iter().any(|c| c.props.contains(&p.name))
            })
            .collect()
    }

    /// Whether the widget `name` is described.
    pub fn is_described(&self, name: &str) -> bool {
        self.described
            .iter()
            .any(|d| d.widget.split('<').next().unwrap_or_default().trim() == name)
    }
}

/// Read the sources under `src`.
///
/// # Errors
/// A file or directory could not be read.
pub fn scan(src: &Path) -> std::io::Result<Scan> {
    let mut files = Vec::new();
    walk(src, &mut files)?;
    files.sort();
    let mut out = Scan::default();
    let mut reexports: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::default();
    // The modules each top module declares `pub mod`: what they declare is public there.
    let mut pub_mods: Vec<(String, String)> = Vec::new();
    let mut structs: Vec<FoundStruct> = Vec::new();
    let mut widget_impls: Vec<String> = Vec::new();
    for file in &files {
        let raw = std::fs::read_to_string(file)?;
        let text = without_tests(&without_comments(&raw));
        let rel = file.strip_prefix(src).unwrap_or(file);
        let modules = module_path(rel);
        let top = modules.first().cloned().unwrap_or_default();
        if is_top_module_file(rel) || rel == Path::new("lib.rs") {
            let entry = reexports.entry(top.clone()).or_default();
            entry.push_str(&pub_uses(&text));
            for (at, _) in text.match_indices("pub mod ") {
                let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
                if text[line_start..at].trim().is_empty() {
                    pub_mods.push((top.clone(), ident_at(&text[at + 8..])));
                }
            }
        }
        read_described(&text, &top, file, &mut out.described);
        read_props(&text, &top, file, &mut out.props);
        if let Some(custom) = read_custom(&text, &modules, file) {
            out.custom.push(custom);
        }
        for (name, exempt) in pub_structs(&text) {
            structs.push((modules.clone(), name, file.clone(), exempt));
        }
        widget_impls.extend(widget_impls_of(&text));
    }
    for (modules, name, file, exempt) in structs {
        let top = modules.first().cloned().unwrap_or_default();
        let reexported = reexports
            .get(&top)
            .is_some_and(|uses| has_word(uses, &name));
        let in_pub_mod = modules
            .get(1)
            .is_some_and(|m| pub_mods.iter().any(|(t, p)| *t == top && p == m));
        if (reexported || in_pub_mod) && widget_impls.contains(&name) {
            out.widgets.push(PublicWidget { name, file, exempt });
        }
    }
    out.widgets
        .sort_by(|a, b| a.name.cmp(&b.name).then(a.file.cmp(&b.file)));
    out.widgets
        .dedup_by(|a, b| a.name == b.name && a.file == b.file);
    out.files = files;
    Ok(out)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_owned();
        if path.is_dir() {
            if name != "tests" && name != "testing" {
                walk(&path, out)?;
            }
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("rs"))
            && name != "tests.rs"
            && !name.ends_with("_tests.rs")
        {
            out.push(path);
        }
    }
    Ok(())
}

/// `widget/card/component.rs` is `["widget", "card", "component"]`; `widget/mod.rs` and
/// `widget.rs` are `["widget"]`.
fn module_path(rel: &Path) -> Vec<String> {
    let mut parts: Vec<String> = rel
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(str::to_owned))
        .collect();
    if let Some(last) = parts.pop() {
        let stem = last.trim_end_matches(".rs").to_owned();
        if stem != "mod" && stem != "lib" {
            parts.push(stem);
        }
    }
    parts
}

fn is_top_module_file(rel: &Path) -> bool {
    match rel.components().count() {
        1 => true,
        2 => rel.file_name().is_some_and(|n| n == "mod.rs"),
        _ => false,
    }
}

/// The text with its `//` comments blanked (doc comments hold examples, not declarations).
/// String literals are kept as they are; a `//` inside one is rare enough in this crate that
/// the line's rest is taken as a comment.
fn without_comments(text: &str) -> String {
    text.lines()
        .map(|line| match line.find("//") {
            Some(at) if !line[..at].contains('"') => &line[..at],
            _ => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The text without its `#[cfg(test)]` items: a module's braces, or one item up to its `;`/`}`.
fn without_tests(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("#[cfg(test)]") {
        out.push_str(&rest[..at]);
        let after = &rest[at..];
        let Some(open) = after.find(['{', ';']) else {
            rest = "";
            break;
        };
        if after.as_bytes()[open] == b';' {
            rest = &after[open + 1..];
            continue;
        }
        let mut depth = 0usize;
        let mut end = after.len();
        for (i, c) in after[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

fn pub_uses(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("pub use ") {
        let after = &rest[at..];
        let end = after.find(';').unwrap_or(after.len());
        out.push_str(&after[..end]);
        out.push('\n');
        rest = &after[end..];
    }
    out
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn has_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        !before.is_some_and(is_ident_char) && !after.is_some_and(is_ident_char)
    })
}

fn ident_at(text: &str) -> String {
    text.trim_start()
        .chars()
        .take_while(|c| is_ident_char(*c))
        .collect()
}

/// The type an impl header names after `for`, up to its `{`: `Card`, `Foo<Bar>`.
fn type_after_for(header: &str) -> Option<String> {
    let at = header.rfind(" for ")?;
    Some(header[at + 5..].trim().to_owned())
}

/// Each `impl ... <trait> for T {` of the text: (header, body).
fn impls_of<'a>(text: &'a str, tr: &str) -> Vec<(&'a str, &'a str)> {
    let mut out = Vec::new();
    for (at, _) in text.match_indices("impl") {
        let before = text[..at].chars().next_back();
        if before.is_some_and(is_ident_char) {
            continue;
        }
        let after = &text[at..];
        let Some(open) = after.find(['{', ';']) else {
            continue;
        };
        let header = &after[..open];
        if !header.contains(" for ") {
            continue;
        }
        let Some(for_at) = header.rfind(" for ") else {
            continue;
        };
        let trait_part = header[..for_at].trim_end();
        let named = trait_part
            .rsplit([' ', ':', '>'])
            .next()
            .unwrap_or_default();
        if named != tr {
            continue;
        }
        let body_end = after[open..]
            .find("\n}")
            .map_or(after.len(), |e| open + e + 2);
        out.push((header, &after[open..body_end]));
    }
    out
}

fn read_described(text: &str, top: &str, file: &Path, out: &mut Vec<DescribedWidget>) {
    for (header, body) in impls_of(text, "Described") {
        let Some(widget) = type_after_for(header) else {
            continue;
        };
        let Some(at) = body.find("type Props") else {
            continue;
        };
        let props = body[at..]
            .split_once('=')
            .map(|(_, rhs)| rhs.split(';').next().unwrap_or_default().trim().to_owned())
            .unwrap_or_default();
        let props = props.rsplit("::").next().unwrap_or_default().to_owned();
        out.push(DescribedWidget {
            top: top.to_owned(),
            widget,
            props,
            file: file.to_owned(),
        });
    }
}

fn read_props(text: &str, top: &str, file: &Path, out: &mut Vec<Props>) {
    for (at, _) in text.match_indices("#[derive(") {
        let after = &text[at..];
        let Some(close) = after.find(")]") else {
            continue;
        };
        let derives = &after[..close];
        if !derives
            .split([',', '(', ' '])
            .any(|d| d.trim() == "Component")
        {
            continue;
        }
        let Some(item) = after.find("struct ") else {
            continue;
        };
        let name = ident_at(&after[item + 7..]);
        out.push(Props {
            top: top.to_owned(),
            name,
            file: file.to_owned(),
        });
    }
}

fn read_custom(text: &str, modules: &[String], file: &Path) -> Option<CustomEntry> {
    let at = text.find("pub fn entry() -> component::Entry")?;
    let body = &text[at..];
    let props = body
        .match_indices("::NAME")
        .map(|(i, _)| {
            body[..i]
                .chars()
                .rev()
                .take_while(|c| is_ident_char(*c))
                .collect::<String>()
                .chars()
                .rev()
                .collect()
        })
        .collect();
    Some(CustomEntry {
        module: modules.join("::"),
        props,
        file: file.to_owned(),
    })
}

/// The text with what its string literals hold blanked, each byte kept in its place.
fn blank_strings(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut inside = false;
    let mut escaped = false;
    for c in text.chars() {
        if inside {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                inside = false;
                out.push(c);
                continue;
            }
            for _ in 0..c.len_utf8() {
                out.push(' ');
            }
        } else {
            if c == '"' {
                inside = true;
            }
            out.push(c);
        }
    }
    out
}

/// Each `pub struct X` of the text, and the `#[not_a_component(kind, "why")]` among the
/// attributes above it (back to the end of the item before).
fn pub_structs(text: &str) -> Vec<(String, Option<(String, String)>)> {
    let mut out = Vec::new();
    for (at, _) in text.match_indices("pub struct ") {
        let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
        if !text[line_start..at].trim().is_empty() {
            continue;
        }
        let name = ident_at(&text[at + 11..]);
        let above = &text[..line_start];
        // The item before ends at a `}`, `;` or `{` outside a string (a reason may hold one).
        let item_start = blank_strings(above)
            .trim_end()
            .rfind(['}', ';', '{'])
            .map_or(0, |i| i + 1);
        let attrs = &above[item_start..];
        // `#[not_a_component(..)]`, or by its path: `#[crate::component::not_a_component(..)]`.
        let exempt = attrs.find("not_a_component(").map(|open| {
            let args = &attrs[open + "not_a_component(".len()..];
            let kind = ident_at(args);
            let reason = args
                .find('"')
                .and_then(|q| {
                    let rest = &args[q + 1..];
                    rest.find('"').map(|close| rest[..close].to_owned())
                })
                .unwrap_or_default();
            (kind, reason)
        });
        out.push((name, exempt));
    }
    out
}

/// The types the text makes widgets: `impl Widget for X`, and `impl Decorator for X` (every
/// decorator is a widget).
fn widget_impls_of(text: &str) -> Vec<String> {
    impls_of(text, "Widget")
        .into_iter()
        .chain(impls_of(text, "Decorator"))
        .filter_map(|(header, _)| type_after_for(header))
        .map(|ty| ident_at(ty.rsplit("::").next().unwrap_or_default()))
        .collect()
}

/// The Rust source of `component::registry`'s entries and `component::parts`, generated from
/// the scan.
pub fn registry_source(scan: &Scan) -> String {
    let mut entries: Vec<(PathBuf, String)> = scan
        .described
        .iter()
        .map(|d| {
            (
                d.file.clone(),
                format!(
                    "crate::component::entry::<crate::{}::{}>()",
                    d.top, d.widget
                ),
            )
        })
        .chain(
            scan.custom
                .iter()
                .map(|c| (c.file.clone(), format!("crate::{}::entry()", c.module))),
        )
        .collect();
    entries.sort();
    let parts = scan.parts();
    let mut out = String::from(
        "// Generated by the build script from the toolkit's sources (component_scan.rs).\n",
    );
    out.push_str("/// Every described widget's entry, in source order.\n");
    out.push_str("fn generated_entries() -> Vec<crate::component::Entry> {\n    vec![\n");
    for (_, entry) in &entries {
        out.push_str("        ");
        out.push_str(entry);
        out.push_str(",\n");
    }
    out.push_str("    ]\n}\n\n");
    out.push_str("/// The components that stand only in a slot of another, in source order.\n");
    let _ = writeln!(
        out,
        "const GENERATED_PARTS: [fn() -> crate::component::Description; {}] = [",
        parts.len()
    );
    for p in parts {
        let _ = writeln!(
            out,
            "    <crate::{}::{} as crate::component::Component>::describe,",
            p.top, p.name
        );
    }
    out.push_str("];\n");
    out
}
