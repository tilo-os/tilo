//! Stateless, byte-indexed Typst completions for the source editor.
//!
//! The caller owns popup state and applies `insert` over `start..end`. Trigger
//! characters are deliberately outside that range; no filesystem reads or
//! compilation are needed, including for project bibliographies.

use std::collections::BTreeMap;

use typst::syntax::{LinkedNode, Side, SyntaxKind, SyntaxMode, SyntaxNode};

use crate::resource::Workspace;

/// One plain-text completion, without editor-specific snippet placeholders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    pub label: String,
    pub insert: String,
    pub detail: String,
    pub badge: String,
    pub glyph: Option<String>,
}

/// A filtered set of completions and the UTF-8 byte range they replace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionSession {
    pub start: usize,
    pub end: usize,
    pub items: Vec<Completion>,
}

/// Complete a `#` expression, an `@` reference, or a math identifier.
///
/// `caret` is a UTF-8 byte offset. Invalid offsets, literal/comment contexts,
/// ordinary prose, and prefixes without matches return `None`. When completing
/// in the middle of an identifier, the range includes its remaining suffix.
pub fn complete(source: &str, caret: usize, workspace: &Workspace) -> Option<CompletionSession> {
    let before = source.get(..caret)?;
    let after = source.get(caret..)?;
    let root = typst::syntax::parse(source);
    let linked = LinkedNode::new(&root);
    let leaf = linked.leaf_at(caret, Side::Before)?;
    if excluded(&leaf) {
        return None;
    }

    let start = word_start(before, reference_char);
    let marker = source.get(..start)?.chars().next_back();
    let (start, end, mut items) = match marker {
        Some('#') => {
            let trigger = linked.leaf_at(start, Side::Before)?;
            if trigger.kind() != SyntaxKind::Hash {
                return None;
            }
            let end = caret
                + after
                    .chars()
                    .take_while(|c| code_char(*c))
                    .map(char::len_utf8)
                    .sum::<usize>();
            (start, end, builtins(CODE, "fn"))
        }
        Some('@') => {
            let trigger = linked.leaf_at(start, Side::Before)?;
            if !matches!(trigger.kind(), SyntaxKind::RefMarker | SyntaxKind::Text)
                || excluded(&trigger)
            {
                return None;
            }
            // References are markup constructs, not code or math operators.
            if leaf.mode_after() != Some(SyntaxMode::Markup) {
                return None;
            }
            let end = caret
                + after
                    .chars()
                    .take_while(|c| reference_char(*c))
                    .map(char::len_utf8)
                    .sum::<usize>();
            // Typst leaves a reference's trailing period/colon in the prose.
            let end = start + source.get(start..end)?.trim_end_matches(['.', ':']).len();
            (start, end.max(caret), references(source, workspace))
        }
        _ if in_math(source, caret, &leaf) => {
            let start = word_start(before, math_char);
            let end = caret
                + after
                    .chars()
                    .take_while(|c| math_char(*c))
                    .map(char::len_utf8)
                    .sum::<usize>();
            (start, end, builtins(MATH, "Math"))
        }
        _ => return None,
    };
    let prefix = source.get(start..caret)?;
    items.retain(|item| item.label.starts_with(prefix));
    (!items.is_empty()).then_some(CompletionSession { start, end, items })
}

fn in_math(source: &str, caret: usize, leaf: &LinkedNode<'_>) -> bool {
    if leaf.mode_after() == Some(SyntaxMode::Math) {
        return true;
    }
    // The parser turns an unmatched opening `$` into an error and moves
    // trailing whitespace outside an otherwise empty equation. Probe with a
    // math identifier so `$|` and `$ |` work before there is any content.
    if leaf.mode_after() != Some(SyntaxMode::Markup)
        || !matches!(leaf.kind(), SyntaxKind::Error | SyntaxKind::Space)
    {
        return false;
    }
    let mut probe = source.to_owned();
    probe.insert(caret, 'a');
    let root = typst::syntax::parse(&probe);
    LinkedNode::new(&root)
        .leaf_at(caret + 1, Side::Before)
        .is_some_and(|node| node.mode_after() == Some(SyntaxMode::Math))
}

fn code_char(c: char) -> bool {
    typst::syntax::is_id_continue(c)
}

fn reference_char(c: char) -> bool {
    code_char(c) || matches!(c, ':' | '.')
}

fn math_char(c: char) -> bool {
    (code_char(c) && !matches!(c, '_' | '-')) || c == '.'
}

fn word_start(before: &str, accepts: fn(char) -> bool) -> usize {
    before
        .char_indices()
        .rev()
        .find(|(_, c)| !accepts(*c))
        .map_or(0, |(offset, c)| offset + c.len_utf8())
}

fn excluded(leaf: &LinkedNode<'_>) -> bool {
    let mut node = Some(leaf);
    while let Some(current) = node {
        if matches!(
            current.kind(),
            SyntaxKind::LineComment
                | SyntaxKind::BlockComment
                | SyntaxKind::Raw
                | SyntaxKind::Str
                | SyntaxKind::Label
        ) || (current.kind() == SyntaxKind::Error && current.leaf_text().starts_with(['"', '`']))
        {
            return true;
        }
        node = current.parent();
    }
    false
}

// label, insertion, explanation, optional mathematical preview
type Builtin = (
    &'static str,
    &'static str,
    &'static str,
    Option<&'static str>,
);

const CODE: &[Builtin] = &[
    (
        "set",
        "set text(size: 12pt)",
        "set function(properties)",
        None,
    ),
    (
        "show",
        "show heading: it => it",
        "show selector: transformation",
        None,
    ),
    (
        "figure",
        "figure([])",
        "figure(body, caption: content)",
        None,
    ),
    (
        "table",
        "table(columns: 2, [], [])",
        "table(columns, ..cells)",
        None,
    ),
    ("block", "block[]", "block(body, width, inset)", None),
    (
        "grid",
        "grid(columns: 2, [], [])",
        "grid(columns, rows, ..cells)",
        None,
    ),
    ("align", "align(center)[]", "align(alignment, body)", None),
    ("text", "text[]", "text(body, font, size, fill)", None),
    (
        "heading",
        "heading[]",
        "heading(body, level, numbering)",
        None,
    ),
    ("cite", "cite(<key>)", "cite(key, supplement, form)", None),
    (
        "bibliography",
        "bibliography(\"references.bib\")",
        "bibliography(path, style, title)",
        None,
    ),
    ("page", "page[]", "page(width, height, margin)", None),
];

const MATH: &[Builtin] = &[
    ("integral", "integral", "Integral", Some("∫")),
    (
        "integral.double",
        "integral.double",
        "Double integral",
        Some("∬"),
    ),
    (
        "integral.triple",
        "integral.triple",
        "Triple integral",
        Some("∭"),
    ),
    (
        "integral.cont",
        "integral.cont",
        "Contour integral",
        Some("∮"),
    ),
    ("sum", "sum", "Summation", Some("∑")),
    ("tau", "tau", "Greek tau", Some("τ")),
    ("alpha", "alpha", "Greek alpha", Some("α")),
    ("alpha.bold", "bold(alpha)", "Bold alpha", Some("𝛂")),
    (
        "alpha.upright",
        "upright(alpha)",
        "Upright alpha",
        Some("α"),
    ),
    ("beta", "beta", "Greek beta", Some("β")),
    ("gamma", "gamma", "Greek gamma", Some("γ")),
    ("delta", "delta", "Greek delta", Some("δ")),
    ("theta", "theta", "Greek theta", Some("θ")),
    ("lambda", "lambda", "Greek lambda", Some("λ")),
    ("sigma", "sigma", "Greek sigma", Some("σ")),
    ("omega", "omega", "Greek omega", Some("ω")),
    ("approx", "approx", "Approximately equal", Some("≈")),
    ("neq", "eq.not", "Not equal", Some("≠")),
    ("leq", "lt.eq", "Less than or equal", Some("≤")),
    ("geq", "gt.eq", "Greater than or equal", Some("≥")),
    ("infty", "infinity", "Infinity", Some("∞")),
    ("infinity", "infinity", "Infinity", Some("∞")),
    ("partial", "partial", "Partial differential", Some("∂")),
    ("sqrt", "sqrt(x)", "Square root", Some("√")),
    ("frac", "frac(1, 2)", "Fraction", Some("½")),
    ("vec", "vec(a, b)", "Column vector", Some("⋮")),
    ("mat", "mat(a, b; c, d)", "Matrix", Some("▦")),
];

fn builtins(entries: &[Builtin], badge: &str) -> Vec<Completion> {
    entries
        .iter()
        .map(|(label, insert, detail, glyph)| Completion {
            label: (*label).into(),
            insert: (*insert).into(),
            detail: (*detail).into(),
            badge: if matches!(*label, "set" | "show") {
                "kw"
            } else {
                badge
            }
            .into(),
            glyph: glyph.map(str::to_owned),
        })
        .collect()
}

fn references(source: &str, workspace: &Workspace) -> Vec<Completion> {
    let mut entries = BTreeMap::new();
    for file in workspace.files.values() {
        let path = file.project_path.as_ref().unwrap_or(&file.name);
        if path.0.to_ascii_lowercase().ends_with(".bib") {
            bibliography(&file.source, &mut entries);
        }
    }
    labels(&typst::syntax::parse(source), &mut entries);
    for file in workspace.files.values() {
        let path = file.project_path.as_ref().unwrap_or(&file.name);
        if path.0.to_ascii_lowercase().ends_with(".typ") && file.source != source {
            labels(&typst::syntax::parse(&file.source), &mut entries);
        }
    }
    entries.into_values().collect()
}

fn labels(node: &SyntaxNode, entries: &mut BTreeMap<String, Completion>) {
    if node.kind() == SyntaxKind::Label
        && let Some(key) = node
            .leaf_text()
            .strip_prefix('<')
            .and_then(|s| s.strip_suffix('>'))
    {
        entries.entry(key.into()).or_insert_with(|| Completion {
            label: key.into(),
            insert: key.into(),
            detail: format!("Document label <{key}>"),
            badge: "Label".into(),
            glyph: None,
        });
    }
    // Labels written in a raw block, string, or comment are not targets.
    if matches!(
        node.kind(),
        SyntaxKind::Raw | SyntaxKind::Str | SyntaxKind::LineComment | SyntaxKind::BlockComment
    ) {
        return;
    }
    for child in node.children() {
        labels(child, entries);
    }
}

/// A tolerant BibTeX reader: balanced braces and quotes, comments, multiline
/// fields, string macros, and concatenated values. A partial entry must not
/// prevent references from other project files from being offered.
fn bibliography(source: &str, entries: &mut BTreeMap<String, Completion>) {
    let mut input = BibReader::new(source);
    let mut strings = BTreeMap::<String, String>::new();
    while let Some(c) = input.bump() {
        if c == '%' {
            input.line_comment();
        } else if c == '@' {
            let kind = input
                .take_while(|c| c.is_ascii_alphabetic())
                .to_ascii_lowercase();
            input.whitespace();
            let Some(open @ ('{' | '(')) = input.bump() else {
                continue;
            };
            let body = input.delimited(open, true);
            if matches!(kind.as_str(), "comment" | "preamble") || kind.is_empty() {
                continue;
            }
            let mut fields = BibReader::new(&body);
            if kind == "string" {
                fields.whitespace();
                let key = fields
                    .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '-'))
                    .to_ascii_lowercase();
                fields.whitespace();
                if fields.bump() == Some('=') {
                    strings.insert(key, fields.value(&strings));
                }
                continue;
            }
            fields.whitespace();
            let key = fields.take_while(|c| c != ',').trim().to_owned();
            if fields.bump() != Some(',') || !typst::syntax::is_valid_label_literal_id(&key) {
                continue;
            }
            let mut metadata = BTreeMap::new();
            loop {
                fields.whitespace();
                while fields.peek() == Some(',') {
                    fields.bump();
                    fields.whitespace();
                }
                let field = fields
                    .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
                    .to_ascii_lowercase();
                fields.whitespace();
                if field.is_empty() || fields.bump() != Some('=') {
                    break;
                }
                let value = readable(&fields.value(&strings));
                let value = if field == "author" {
                    value.replace(" and ", "; ")
                } else {
                    value
                };
                metadata.insert(field, value);
            }
            let detail = ["author", "year", "title"]
                .into_iter()
                .filter_map(|field| metadata.get(field))
                .filter(|value| !value.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join(" · ");
            entries.entry(key.clone()).or_insert_with(|| Completion {
                insert: key.clone(),
                label: key,
                detail: if detail.is_empty() {
                    "Bibliography entry".into()
                } else {
                    detail
                },
                badge: "Citation".into(),
                glyph: None,
            });
        }
    }
}

fn readable(value: &str) -> String {
    value
        .replace(['{', '}'], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

struct BibReader<'a> {
    rest: &'a str,
}

impl<'a> BibReader<'a> {
    fn new(source: &'a str) -> Self {
        Self { rest: source }
    }

    fn peek(&self) -> Option<char> {
        self.rest.chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.rest = self.rest.get(c.len_utf8()..)?;
        Some(c)
    }

    fn take_while(&mut self, accepts: impl Fn(char) -> bool) -> &'a str {
        let end = self
            .rest
            .char_indices()
            .find(|(_, c)| !accepts(*c))
            .map_or(self.rest.len(), |(at, _)| at);
        let text = self.rest.get(..end).unwrap_or_default();
        self.rest = self.rest.get(end..).unwrap_or_default();
        text
    }

    fn line_comment(&mut self) {
        self.take_while(|c| c != '\n');
    }

    fn whitespace(&mut self) {
        loop {
            self.take_while(char::is_whitespace);
            if self.peek() != Some('%') {
                break;
            }
            self.line_comment();
        }
    }

    /// The opening delimiter has already been consumed. Preserve inner braces
    /// until display formatting, and never interpret escaped quotes as closers.
    fn delimited(&mut self, open: char, comments: bool) -> String {
        let close = match open {
            '{' => '}',
            '(' => ')',
            _ => '"',
        };
        let mut result = String::new();
        let mut depth = 0usize;
        while let Some(c) = self.bump() {
            if c == '\\' {
                result.push(c);
                if let Some(escaped) = self.bump() {
                    result.push(escaped);
                }
            } else if c == '%' && comments && depth == 0 {
                result.push(c);
                result.push_str(self.take_while(|c| c != '\n'));
            } else if c == close && depth == 0 {
                break;
            } else if c == '{' {
                depth += 1;
                result.push(c);
            } else if c == '}' && depth > 0 {
                depth -= 1;
                result.push(c);
            } else if c == '"' && open != '"' && depth == 0 {
                result.push('"');
                result.push_str(&self.delimited('"', false));
                result.push('"');
            } else {
                result.push(c);
            }
        }
        result
    }

    fn value(&mut self, strings: &BTreeMap<String, String>) -> String {
        let mut value = String::new();
        loop {
            self.whitespace();
            match self.peek() {
                Some(open @ ('{' | '"')) => {
                    self.bump();
                    value.push_str(&self.delimited(open, false));
                }
                Some(_) => {
                    let token =
                        self.take_while(|c| !c.is_whitespace() && !matches!(c, ',' | '#' | '%'));
                    value.push_str(
                        strings
                            .get(&token.to_ascii_lowercase())
                            .map_or(token, String::as_str),
                    );
                }
                None => break,
            }
            self.whitespace();
            if self.peek() != Some('#') {
                break;
            }
            self.bump();
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::TypstFile;
    use ui::tilo_resource::{Atom, Id};

    fn project(files: &[(&str, &str)]) -> Workspace {
        let mut workspace = Workspace::default();
        for (index, (name, source)) in files.iter().enumerate() {
            workspace.files.push(
                Id::from_raw(format!("file-{index}")),
                TypstFile {
                    name: Atom((*name).into()),
                    source: (*source).into(),
                    ..TypstFile::default()
                },
            );
        }
        workspace
    }

    fn at_end(source: &str) -> CompletionSession {
        complete(source, source.len(), &Workspace::default()).expect(source)
    }

    fn find<'a>(session: &'a CompletionSession, label: &str) -> &'a Completion {
        session
            .items
            .iter()
            .find(|item| item.label == label)
            .unwrap()
    }

    #[test]
    fn hash_prefix_and_replacement_keep_trigger_and_suffix() {
        let source = "é 🦀 #figment after";
        let caret = source.find("ment").unwrap();
        let session = complete(source, caret, &Workspace::default()).unwrap();
        assert_eq!(session.items.len(), 1);
        assert_eq!(source.get(session.start..session.end), Some("figment"));
        let mut edited = source.to_owned();
        edited.replace_range(session.start..session.end, &find(&session, "figure").insert);
        assert_eq!(edited, "é 🦀 #figure([]) after");
    }

    #[test]
    fn empty_hash_offers_every_required_directive_and_function() {
        let session = at_end("#");
        assert_eq!((session.start, session.end), (1, 1));
        for label in [
            "set",
            "show",
            "figure",
            "table",
            "block",
            "grid",
            "align",
            "text",
            "heading",
            "cite",
            "bibliography",
            "page",
        ] {
            let item = find(&session, label);
            assert!(!item.insert.starts_with('#'));
            assert!(!item.detail.is_empty());
            assert!(
                typst::syntax::parse(&format!("#{}", item.insert))
                    .errors_and_warnings()
                    .0
                    .is_empty(),
                "{}",
                item.insert
            );
        }
        assert_eq!(at_end("#bib").items.len(), 1);
    }

    #[test]
    fn common_prefixes_have_multiple_real_math_choices() {
        let parts = crate::compile::WorldParts::new();
        for prefix in ["a", "al", "in", "int"] {
            let session = at_end(&format!("$ {prefix}"));
            assert!(session.items.len() >= 3, "{prefix}: {session:?}");
            for item in &session.items {
                let out =
                    crate::compile::compile_source(&parts, &format!("$ {} $", item.insert), 1.0);
                assert!(out.ok, "{}: {:?}", item.insert, out.diagnostics);
            }
        }
    }

    #[test]
    fn math_context_glyphs_and_typst_aliases() {
        let session = at_end("$ ");
        assert_eq!((session.start, session.end), (2, 2));
        assert_eq!(at_end("$").items, session.items);
        for label in [
            "integral", "sum", "tau", "alpha", "beta", "gamma", "delta", "theta", "lambda",
            "sigma", "omega", "approx", "neq", "leq", "geq", "infty", "partial", "sqrt", "frac",
            "vec", "mat",
        ] {
            let item = find(&session, label);
            assert!(item.glyph.is_some());
            assert!(
                typst::syntax::parse(&format!("$ {} $", item.insert))
                    .errors_and_warnings()
                    .0
                    .is_empty(),
                "{}",
                item.insert
            );
        }
        assert_eq!(find(&at_end("$ ne"), "neq").insert, "eq.not");
        assert_eq!(find(&at_end("$ inft"), "infty").insert, "infinity");
        assert_eq!(find(&at_end("$ le"), "leq").insert, "lt.eq");
        assert_eq!(find(&at_end("$ ge"), "geq").insert, "gt.eq");
        assert_eq!(find(&at_end("$x_al"), "alpha").glyph.as_deref(), Some("α"));
        assert_eq!(at_end("$ #fig").items.first().unwrap().label, "figure");
    }

    #[test]
    fn closed_equation_and_prose_do_not_offer_math() {
        for source in [
            "alpha",
            "some text",
            "$ alpha $",
            "$ alpha $ al",
            "#not-a-function",
            "$ zzz",
        ] {
            assert!(
                complete(source, source.len(), &Workspace::default()).is_none(),
                "{source}"
            );
        }
        let source = "$ alphabeta $";
        let session = complete(source, 5, &Workspace::default()).unwrap();
        assert_eq!(source.get(session.start..session.end), Some("alphabeta"));
        assert_eq!(
            session
                .items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "alpha.bold", "alpha.upright"]
        );
    }

    #[test]
    fn comments_raw_strings_escapes_and_labels_are_excluded() {
        for source in [
            "// #fig",
            "/* #fig",
            "/* outer /* inner */ #fig",
            "`#fig`",
            "`#fig",
            "```typ\n#fig",
            "#text(\"#fig\")",
            "#text(\"#fig",
            "$ \"al",
            "\\#fig",
            "\\@fig",
            "<fig",
        ] {
            let caret = source.find("fig").map_or(source.len(), |at| at + 3);
            assert!(
                complete(source, caret, &Workspace::default()).is_none(),
                "{source}"
            );
        }
        assert!(complete("// ignored\n#fig", 15, &Workspace::default()).is_some());
        assert!(complete("/* ignored */ #fig", 18, &Workspace::default()).is_some());
    }

    #[test]
    fn citations_read_all_project_bibliographies_and_metadata() {
        let workspace = project(&[
            (
                "one.bib",
                "@article{flash, author={Flash, Tamar and Hogan, Neville}, year=1985, title={The {Coordination} of\n arm movements}}",
            ),
            (
                "two.BIB",
                "@book(öther:2024, AUTHOR=\"Zoë, Émile\", Year={2024}, TITLE=\"Readable, quoted title\")",
            ),
            ("not-bib.typ", "@article{not-a-reference, title={No}}"),
        ]);
        let session = complete("See @", 5, &workspace).unwrap();
        assert_eq!(session.items.len(), 2);
        assert_eq!((session.start, session.end), (5, 5));
        let flash = find(&session, "flash");
        assert_eq!(flash.insert, "flash");
        assert_eq!(flash.badge, "Citation");
        assert_eq!(
            flash.detail,
            "Flash, Tamar; Hogan, Neville · 1985 · The Coordination of arm movements"
        );
        assert!(
            find(&session, "öther:2024")
                .detail
                .contains("Zoë, Émile · 2024")
        );
        let session = complete("@fl", 3, &workspace).unwrap();
        assert_eq!(session.items.len(), 1);
        assert_eq!(session.items.first().unwrap().label, "flash");
    }

    #[test]
    fn bibliography_comments_macros_nested_braces_and_duplicate_keys() {
        let workspace = project(&[(
            "refs.bib",
            r#"
% @article{fake, title={Not an entry}}
@comment{ @article{also-fake, title={No}} }
@preamble{"ignored"}
@string{prefix = "A "}
@article{real,
  title = prefix # {nested {Title}, with commas and words},
  author = {Research {Group}}, % ignored } { \" @fake{no}, year=1900,
  year = 2025,
}
@book{real, title={Duplicate}}
@book{minimal,}
"#,
        )]);
        let session = complete("@", 1, &workspace).unwrap();
        assert_eq!(session.items.len(), 2);
        assert_eq!(
            find(&session, "real").detail,
            "Research Group · 2025 · A nested Title, with commas and words"
        );
        assert_eq!(find(&session, "minimal").detail, "Bibliography entry");
    }

    #[test]
    fn labels_come_from_source_and_project_but_not_literals() {
        let workspace = project(&[
            (
                "chapter.typ",
                "= Chapter <chapter>\n`<raw>`\n// <comment>\n#text(\"<string>\")",
            ),
            ("refs.bib", "@book{intro, title={A book}}"),
        ]);
        let source = "= Intro <intro>\n<local>\n@";
        let session = complete(source, source.len(), &workspace).unwrap();
        assert_eq!(
            session
                .items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            ["chapter", "intro", "local"]
        );
        assert_eq!(find(&session, "intro").badge, "Citation");
        assert_eq!(find(&session, "local").badge, "Label");
        assert_eq!(find(&session, "local").insert, "local");
    }

    #[test]
    fn references_respect_literal_contexts_and_preserve_sentence_punctuation() {
        let workspace = project(&[("refs.bib", "@book{flash, title={A book}}")]);
        for source in [
            "// @flash",
            "/* @flash",
            "`@flash`",
            "`@flash",
            "```\n@flash",
            "#text(\"@flash\")",
            "#text(\"@flash",
            "$ @flash",
            "\\@flash",
            "#let a = @flash",
        ] {
            let caret = source.find("flash").unwrap() + 5;
            assert!(complete(source, caret, &workspace).is_none(), "{source}");
        }
        let source = "🦀 @flotsam. Next";
        let caret = source.find("otsam").unwrap();
        let session = complete(source, caret, &workspace).unwrap();
        assert_eq!(source.get(session.start..session.end), Some("flotsam"));
        let mut edited = source.to_owned();
        edited.replace_range(session.start..session.end, &find(&session, "flash").insert);
        assert_eq!(edited, "🦀 @flash. Next");
    }

    #[test]
    fn project_path_identifies_bibliographies_after_a_rename() {
        let mut workspace = Workspace::default();
        workspace.files.push(
            Id::from_raw("references"),
            TypstFile {
                name: Atom("renamed.txt".into()),
                project_path: Some(Atom("literature/sources.bib".into())),
                source: "@book{key, title={Still a bibliography}}".into(),
                ..TypstFile::default()
            },
        );
        assert_eq!(complete("@k", 2, &workspace).unwrap().items.len(), 1);
    }

    #[test]
    fn unicode_byte_boundaries_and_out_of_bounds_are_safe() {
        let source = "🦀 é #fig";
        for caret in 0..=source.len() + 1 {
            let session = complete(source, caret, &Workspace::default());
            if !source.is_char_boundary(caret) || caret > source.len() {
                assert!(session.is_none());
            }
            if let Some(session) = session {
                assert!(source.get(session.start..session.end).is_some());
            }
        }
        assert!(complete(source, usize::MAX, &Workspace::default()).is_none());
        assert!(complete("", 0, &Workspace::default()).is_none());
        let source = "<étiquette> @éti";
        let session = at_end(source);
        assert_eq!(source.get(session.start..session.end), Some("éti"));
        assert_eq!(find(&session, "étiquette").insert, "étiquette");
    }
}
