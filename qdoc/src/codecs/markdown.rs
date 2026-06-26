//! Built-in Markdown codec. Maps the modeled rich-text subset (headings,
//! paragraphs, bullet/ordered list items, **strong** / *em* / `code`) onto the
//! canonical document. Export normalizes whitespace; lossless preservation of
//! unmodeled constructs is a Phase-4 (odt) concern, not here.

use crate::document::MarkSpan;
use crate::format::{CodecError, Confidence, FormatProvider};
use crate::model::{DocMut, DocRef};
use crate::snapshot::{AttrVal, DocSnapshot};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};

pub struct MarkdownCodec;

impl FormatProvider for MarkdownCodec {
    fn schema_id(&self) -> &str {
        crate::schema::SCHEMA_ID
    }

    fn detect(&self, _head: &[u8]) -> Confidence {
        Confidence::Medium
    }

    fn import(&self, bytes: &[u8], doc: &mut DocMut) -> Result<(), CodecError> {
        let src = std::str::from_utf8(bytes).map_err(|e| CodecError::Parse(e.to_string()))?;
        import_md(src, doc);
        Ok(())
    }

    fn export(&self, doc: DocRef) -> Result<Vec<u8>, CodecError> {
        Ok(export_md(&doc.materialize()).into_bytes())
    }
}

/// Open a fresh top-level block, anchored after the previous one, and make it
/// the current text target.
fn start_block(
    doc: &mut DocMut,
    prev: &mut Option<String>,
    cur: &mut Option<String>,
    cur_len: &mut usize,
    kind: &str,
) -> String {
    let id = doc.insert_block(prev.as_deref(), kind);
    *prev = Some(id.clone());
    *cur = Some(id.clone());
    *cur_len = 0;
    id
}

fn import_md(src: &str, doc: &mut DocMut) {
    let mut prev: Option<String> = None; // previous top-level block (insertion anchor)
    let mut cur: Option<String> = None; // current block receiving text
    let mut cur_len: usize = 0; // char length of current block's text so far
    let mut in_item = false; // inside a list item (suppress the inner paragraph wrapper)
    let mut ordered_stack: Vec<bool> = Vec::new();
    let mut mark_stack: Vec<(&'static str, usize)> = Vec::new();
    // Apply marks after the full parse so trailing text doesn't expand them
    // (add_mark uses ExpandMark::After).
    let mut pending_marks: Vec<(String, usize, usize, &'static str)> = Vec::new();

    for ev in Parser::new(src) {
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "heading");
                doc.set_attr(&id, "level", level as i64);
            }
            Event::End(TagEnd::Heading(_)) => cur = None,
            Event::Start(Tag::Paragraph) => {
                if !in_item {
                    start_block(doc, &mut prev, &mut cur, &mut cur_len, "paragraph");
                }
            }
            Event::End(TagEnd::Paragraph) => {
                if !in_item {
                    cur = None;
                }
            }
            Event::Start(Tag::List(start)) => ordered_stack.push(start.is_some()),
            Event::End(TagEnd::List(_)) => {
                ordered_stack.pop();
            }
            Event::Start(Tag::Item) => {
                let ordered = *ordered_stack.last().unwrap_or(&false);
                let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "list_item");
                doc.set_attr(&id, "ordered", ordered);
                in_item = true;
            }
            Event::End(TagEnd::Item) => {
                cur = None;
                in_item = false;
            }
            Event::Start(Tag::Strong) => mark_stack.push(("strong", cur_len)),
            Event::End(TagEnd::Strong) => {
                if let (Some((name, start)), Some(b)) = (mark_stack.pop(), cur.as_ref()) {
                    pending_marks.push((b.clone(), start, cur_len, name));
                }
            }
            Event::Start(Tag::Emphasis) => mark_stack.push(("em", cur_len)),
            Event::End(TagEnd::Emphasis) => {
                if let (Some((name, start)), Some(b)) = (mark_stack.pop(), cur.as_ref()) {
                    pending_marks.push((b.clone(), start, cur_len, name));
                }
            }
            Event::Text(t) => {
                if let Some(b) = cur.as_ref() {
                    doc.splice_text(b, cur_len, 0, &t);
                    cur_len += t.chars().count();
                }
            }
            Event::Code(t) => {
                if let Some(b) = cur.as_ref() {
                    let start = cur_len;
                    doc.splice_text(b, cur_len, 0, &t);
                    cur_len += t.chars().count();
                    pending_marks.push((b.clone(), start, cur_len, "code"));
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(b) = cur.as_ref() {
                    doc.splice_text(b, cur_len, 0, " ");
                    cur_len += 1;
                }
            }
            _ => {}
        }
    }

    for (b, start, end, name) in pending_marks {
        doc.add_mark(&b, start, end, name, true);
    }
}

fn export_md(snap: &DocSnapshot) -> String {
    let mut out = String::new();
    for (i, b) in snap.blocks.iter().enumerate() {
        if i > 0 {
            let prev = &snap.blocks[i - 1];
            if prev.kind == "list_item" && b.kind == "list_item" {
                out.push('\n'); // keep consecutive list items tight
            } else {
                out.push_str("\n\n");
            }
        }
        let text = render_inline(&b.text, &b.marks);
        match b.kind.as_str() {
            "heading" => {
                let level = match b.attrs.get("level") {
                    Some(AttrVal::Int(n)) => (*n).clamp(1, 6),
                    _ => 1,
                };
                out.push_str(&"#".repeat(level as usize));
                out.push(' ');
                out.push_str(&text);
            }
            "list_item" => {
                let ordered = matches!(b.attrs.get("ordered"), Some(AttrVal::Bool(true)));
                out.push_str(if ordered { "1. " } else { "- " });
                out.push_str(&text);
            }
            _ => out.push_str(&text),
        }
    }
    out.push('\n');
    out
}

fn delim(name: &str) -> &'static str {
    match name {
        "strong" => "**",
        "em" => "*",
        "code" => "`",
        _ => "",
    }
}

/// Re-emit inline marks as Markdown delimiters. At each char boundary, close
/// marks ending there before opening marks starting there. Robust for
/// non-overlapping marks (the modeled case).
fn render_inline(text: &str, marks: &[MarkSpan]) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut open_at: Vec<Vec<&str>> = vec![Vec::new(); n + 1];
    let mut close_at: Vec<Vec<&str>> = vec![Vec::new(); n + 1];
    for m in marks {
        if delim(&m.name).is_empty() {
            continue;
        }
        let s = m.start.min(n);
        let e = m.end.min(n);
        if s >= e {
            continue;
        }
        open_at[s].push(m.name.as_str());
        close_at[e].push(m.name.as_str());
    }
    let mut out = String::new();
    for i in 0..=n {
        for name in &close_at[i] {
            out.push_str(delim(name));
        }
        for name in &open_at[i] {
            out.push_str(delim(name));
        }
        if i < n {
            out.push(chars[i]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Document;

    #[test]
    fn roundtrip_basic() {
        let src = "# Title\n\nHello **world** and *em*.\n\n- a\n- b\n";
        let mut d = Document::new();
        MarkdownCodec.import(src.as_bytes(), &mut d.as_mut()).unwrap();
        let snap = d.materialize();
        assert_eq!(snap.blocks[0].kind, "heading");
        assert_eq!(snap.blocks[0].text, "Title");
        assert!(snap
            .blocks
            .iter()
            .any(|b| b.kind == "paragraph" && b.marks.iter().any(|m| m.name == "strong")));
        assert_eq!(snap.blocks.iter().filter(|b| b.kind == "list_item").count(), 2);

        let out1 = MarkdownCodec.export(d.as_ref()).unwrap();
        let mut d2 = Document::new();
        MarkdownCodec.import(&out1, &mut d2.as_mut()).unwrap();
        let out2 = MarkdownCodec.export(d2.as_ref()).unwrap();
        assert_eq!(
            String::from_utf8(out1).unwrap(),
            String::from_utf8(out2).unwrap(),
            "export must be idempotent under re-import"
        );
    }

    #[test]
    fn heading_levels_preserved() {
        let mut d = Document::new();
        MarkdownCodec.import(b"## Sub", &mut d.as_mut()).unwrap();
        let out = String::from_utf8(MarkdownCodec.export(d.as_ref()).unwrap()).unwrap();
        assert!(out.starts_with("## Sub"), "got {out:?}");
    }

    #[test]
    fn ordered_list_marker() {
        let mut d = Document::new();
        MarkdownCodec.import(b"1. one\n2. two\n", &mut d.as_mut()).unwrap();
        let out = String::from_utf8(MarkdownCodec.export(d.as_ref()).unwrap()).unwrap();
        assert!(out.contains("1. one"), "got {out:?}");
    }

    use proptest::prelude::*;

    fn doc_strategy() -> impl Strategy<Value = String> {
        let word = "[a-z]{1,5}";
        let inline = prop_oneof![
            word.prop_map(|w| w),
            word.prop_map(|w| format!("**{w}**")),
            word.prop_map(|w| format!("*{w}*")),
        ];
        let line = prop_oneof![
            proptest::collection::vec(inline, 1..4).prop_map(|ws| ws.join(" ")),
            "[a-z]{1,5}".prop_map(|w| format!("# {w}")),
            "[a-z]{1,5}".prop_map(|w| format!("- {w}")),
        ];
        proptest::collection::vec(line, 0..6).prop_map(|lines| lines.join("\n\n") + "\n")
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

        #[test]
        fn export_is_idempotent(src in doc_strategy()) {
            let mut d = Document::new();
            MarkdownCodec.import(src.as_bytes(), &mut d.as_mut()).unwrap();
            let out1 = MarkdownCodec.export(d.as_ref()).unwrap();
            let mut d2 = Document::new();
            MarkdownCodec.import(&out1, &mut d2.as_mut()).unwrap();
            let out2 = MarkdownCodec.export(d2.as_ref()).unwrap();
            prop_assert_eq!(String::from_utf8(out1).unwrap(), String::from_utf8(out2).unwrap());
        }
    }
}
