//! A page of [`Segment`]s as Markdown, the form `read-text` answers by
//! default: one line per block, ending in the block's `{#id}`.
//!
//! ```text
//! ## Standup {#b1}
//! - accessibility review {#b2}
//!   1. screen reader pass {#b3}
//! | Person {#c1} | Blocked {#c2} | {#table t7}
//! | --- | --- |
//! | Maya {#c3} | {#c4} |
//! ```
//!
//! Headings carry their level, list items their numbering and depth (qdoc's
//! `level`, the block editor's `nesting`), and a run of cells becomes a
//! Markdown table tagged with its table id (the id `EditTable` takes), each
//! cell ending in its own block id, empty or not, so a caller can select
//! and fill it. A line break inside a block reads as `↵`, one
//! character as in the document, so a block's character offsets are its
//! text's. A block the page ends inside closes with `…`; the next page
//! continues it.

use super::Segment;

/// `segments` as Markdown.
pub fn render(segments: &[Segment]) -> String {
    let mut out = String::new();
    let mut index = 0;
    while let Some(segment) = segments.get(index) {
        if segment.kind == "cell" {
            let rest = segments.get(index..).unwrap_or_default();
            let run = rest
                .iter()
                .take_while(|cell| cell.kind == "cell" && cell.shape.table == segment.shape.table)
                .count();
            table(&mut out, rest.get(..run).unwrap_or_default());
            index += run;
            continue;
        }
        let depth = usize::try_from(
            segment
                .shape
                .level
                .or(segment.shape.nesting)
                .unwrap_or(0)
                .clamp(0, 9),
        )
        .unwrap_or(0);
        let prefix = match segment.kind.as_str() {
            "paragraph" => String::new(),
            "heading" => {
                let level = usize::try_from(segment.shape.level.unwrap_or(1).clamp(1, 6));
                format!("{} ", "#".repeat(level.unwrap_or(1)))
            }
            "list_item" if segment.shape.ordered == Some(true) => {
                format!("{}1. ", "  ".repeat(depth))
            }
            "list_item" => format!("{}- ", "  ".repeat(depth)),
            other => format!("[{other}] "),
        };
        let line = prefix + &inline(segment);
        if !line.is_empty() {
            out.push_str(&line);
            out.push(' ');
        }
        out.push_str("{#");
        out.push_str(&segment.block);
        out.push_str("}\n");
        index += 1;
    }
    out
}

fn inline(segment: &Segment) -> String {
    let mut text = segment.text.replace('\n', "↵");
    if !segment.complete {
        text.push('…');
    }
    text
}

/// One table's cells on this page, in document order, as rows by `row`.
fn table(out: &mut String, cells: &[Segment]) {
    let mut rows: Vec<(i64, Vec<&Segment>)> = Vec::new();
    for cell in cells {
        let row = cell.shape.row.unwrap_or(0);
        match rows.iter_mut().find(|(at, _)| *at == row) {
            Some((_, row)) => row.push(cell),
            None => rows.push((row, vec![cell])),
        }
    }
    rows.sort_by_key(|(row, _)| *row);
    for (index, (_, row)) in rows.iter_mut().enumerate() {
        row.sort_by_key(|cell| cell.shape.col.unwrap_or(0));
        out.push('|');
        for cell in row.iter() {
            out.push(' ');
            let text = inline(cell).replace('|', "\\|");
            if !text.is_empty() {
                out.push_str(&text);
                out.push(' ');
            }
            out.push_str("{#");
            out.push_str(&cell.block);
            out.push_str("} |");
        }
        if index == 0 {
            if let Some(table) = cells.first().and_then(|cell| cell.shape.table.as_deref()) {
                out.push_str(" {#table ");
                out.push_str(table);
                out.push('}');
            }
            out.push_str("\n|");
            out.push_str(&" --- |".repeat(row.len()));
        }
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Segment, Shape};
    use super::render;

    fn block(id: &str, kind: &str, text: &str, shape: Shape) -> Segment {
        Segment {
            block: id.into(),
            kind: kind.into(),
            text: text.into(),
            complete: true,
            shape,
        }
    }

    fn cell(id: &str, text: &str, row: i64, col: i64) -> Segment {
        block(
            id,
            "cell",
            text,
            Shape {
                table: Some("t7".into()),
                row: Some(row),
                col: Some(col),
                ..Shape::default()
            },
        )
    }

    #[test]
    fn headings_lists_and_tables_read_as_markdown_with_their_ids() {
        let mut cut = block("p2", "paragraph", "first\nsecond", Shape::default());
        cut.complete = false;
        let page = [
            block(
                "h1",
                "heading",
                "Standup",
                Shape {
                    level: Some(2),
                    ..Shape::default()
                },
            ),
            block("l1", "list_item", "review", Shape::default()),
            block(
                "l2",
                "list_item",
                "ship",
                Shape {
                    ordered: Some(true),
                    level: Some(1),
                    ..Shape::default()
                },
            ),
            block(
                "l3",
                "list_item",
                "tag",
                Shape {
                    nesting: Some(2),
                    ..Shape::default()
                },
            ),
            cell("c1", "Person", 0, 0),
            cell("c2", "Blocked", 0, 1),
            cell("c4", "no | maybe", 1, 1),
            cell("c3", "Maya", 1, 0),
            cell("c5", "", 2, 0),
            cell("c6", "", 2, 1),
            block("p1", "paragraph", "", Shape::default()),
            cut,
        ];
        assert_eq!(
            render(&page),
            "## Standup {#h1}\n\
             - review {#l1}\n  1. ship {#l2}\n    - tag {#l3}\n\
             | Person {#c1} | Blocked {#c2} | {#table t7}\n| --- | --- |\n\
             | Maya {#c3} | no \\| maybe {#c4} |\n| {#c5} | {#c6} |\n\
             {#p1}\n\
             first↵second… {#p2}\n"
        );
    }
}
