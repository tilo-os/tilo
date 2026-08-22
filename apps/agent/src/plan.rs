//! Reading instructions out of the document, and the pure parts of
//! carrying them out.
//!
//! An instruction is a paragraph addressed to the agent by name —
//! `Scribe: turn the bullets into a table`. The vocabulary is small and
//! scripted; the shape is what matters: the agent reads the same document
//! everyone else does, and answers in it.

use std::collections::BTreeMap;

use qdoc::snapshot::{AttrVal, BlockView, DocSnapshot};

/// What the agent was asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Instruction {
    /// Turn the run of bullets above the instruction into a table.
    Table,
    /// Put a caption under the (first) image.
    Caption,
    /// Make the first line a title.
    Heading,
    /// Something outside the vocabulary; the agent says so in the document.
    Unknown(String),
}

impl Instruction {
    /// Parse `text` as an instruction to `name` (case-insensitive), or
    /// `None` when it is not addressed to the agent at all.
    pub fn parse(text: &str, name: &str) -> Option<Instruction> {
        let t = text.trim();
        let prefix = format!("{}:", name.to_lowercase());
        let lower = t.to_lowercase();
        if !lower.starts_with(&prefix) {
            return None;
        }
        let rest = lower[prefix.len()..].trim().to_string();
        if rest.is_empty() {
            return Some(Instruction::Unknown(rest));
        }
        Some(if rest.contains("table") {
            Instruction::Table
        } else if rest.contains("caption") {
            Instruction::Caption
        } else if rest.contains("heading") || rest.contains("title") {
            Instruction::Heading
        } else {
            Instruction::Unknown(rest)
        })
    }
}

/// One block, as the agent reads it.
#[derive(Clone, Debug)]
pub struct Block {
    pub id: String,
    pub kind: String,
    pub text: String,
    pub attrs: BTreeMap<String, AttrVal>,
}

impl From<&BlockView> for Block {
    fn from(b: &BlockView) -> Self {
        Block {
            id: b.id.clone(),
            kind: b.kind.clone(),
            text: b.text.clone(),
            attrs: b.attrs.clone(),
        }
    }
}

/// The body, in order.
pub fn blocks(snapshot: &DocSnapshot) -> Vec<Block> {
    snapshot.blocks.iter().map(Block::from).collect()
}

/// The first paragraph addressed to `name`, with its index.
pub fn find_instruction(blocks: &[Block], name: &str) -> Option<(usize, Instruction)> {
    blocks.iter().enumerate().find_map(|(i, b)| {
        if b.kind != "paragraph" {
            return None;
        }
        Instruction::parse(&b.text, name).map(|ins| (i, ins))
    })
}

/// The run of bullets the instruction refers to: the consecutive
/// `list_item` blocks immediately above index `at`, skipping blank
/// paragraphs between them and the instruction.
pub fn bullets_above(blocks: &[Block], at: usize) -> Vec<usize> {
    let mut i = at;
    // Skip blank lines between the bullets and the instruction.
    while i > 0 && blocks[i - 1].kind == "paragraph" && blocks[i - 1].text.trim().is_empty() {
        i -= 1;
    }
    let mut run = Vec::new();
    while i > 0 && blocks[i - 1].kind == "list_item" {
        i -= 1;
        run.push(i);
    }
    run.reverse();
    run
}

/// Split one bullet into cells. The first separator found, in this order,
/// wins for the whole line: ` — `, ` – `, ` - `, `: `, `, `. A line with
/// none is one cell.
pub fn split_cells(line: &str) -> Vec<String> {
    let line = line.trim();
    for sep in [" — ", " – ", " - ", ": ", ", "] {
        if line.contains(sep) {
            return line.split(sep).map(|s| s.trim().to_string()).collect();
        }
    }
    vec![line.to_string()]
}

/// Rows of cells for a table, padded to a common width.
pub fn table_cells(lines: &[&str]) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = lines.iter().map(|l| split_cells(l)).collect();
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    for r in &mut rows {
        r.resize(cols, String::new());
    }
    rows
}

/// The cells of the table `table_id`, ordered row-major, as block ids.
pub fn cells_of(blocks: &[Block], table_id: &str) -> Vec<(i64, i64, String)> {
    let mut cells: Vec<(i64, i64, String)> = blocks
        .iter()
        .filter(|b| b.kind == "cell")
        .filter(|b| matches!(b.attrs.get("table"), Some(AttrVal::Str(t)) if t == table_id))
        .map(|b| {
            let n = |k: &str| match b.attrs.get(k) {
                Some(AttrVal::Int(v)) => *v,
                _ => 0,
            };
            (n("row"), n("col"), b.id.clone())
        })
        .collect();
    cells.sort();
    cells
}

/// Every table id present in `blocks`.
pub fn table_ids(blocks: &[Block]) -> Vec<String> {
    let mut ids: Vec<String> = blocks
        .iter()
        .filter(|b| b.kind == "cell")
        .filter_map(|b| match b.attrs.get("table") {
            Some(AttrVal::Str(t)) => Some(t.clone()),
            _ => None,
        })
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// The first image block, if any.
pub fn first_image(blocks: &[Block]) -> Option<usize> {
    blocks.iter().position(|b| b.kind == "image")
}

/// The first block with visible text that is not the instruction itself.
pub fn first_text_block(blocks: &[Block], instruction: usize) -> Option<usize> {
    blocks
        .iter()
        .enumerate()
        .find(|(i, b)| *i != instruction && !b.text.trim().is_empty() && b.kind != "cell")
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(id: &str, text: &str) -> Block {
        Block {
            id: id.into(),
            kind: "paragraph".into(),
            text: text.into(),
            attrs: BTreeMap::new(),
        }
    }
    fn bullet(id: &str, text: &str) -> Block {
        Block {
            id: id.into(),
            kind: "list_item".into(),
            text: text.into(),
            attrs: BTreeMap::new(),
        }
    }

    #[test]
    fn instructions_are_addressed_by_name_case_insensitively() {
        assert_eq!(Instruction::parse("scribe: turn the bullets into a table", "Scribe"), Some(Instruction::Table));
        assert_eq!(Instruction::parse("Scribe: caption the image please", "Scribe"), Some(Instruction::Caption));
        assert_eq!(Instruction::parse("SCRIBE: give this a title", "Scribe"), Some(Instruction::Heading));
        assert_eq!(
            Instruction::parse("Scribe: juggle", "Scribe"),
            Some(Instruction::Unknown("juggle".into()))
        );
        assert_eq!(Instruction::parse("Dear Scribe: table", "Scribe"), None);
        assert_eq!(Instruction::parse("Scribe: table", "Ada"), None);
    }

    #[test]
    fn the_bullets_above_the_instruction_are_the_run_it_means() {
        let blocks = vec![
            para("h", "Shopping"),
            bullet("a", "Apples - 3"),
            bullet("b", "Pears - 5"),
            para("blank", ""),
            para("ins", "Scribe: table"),
            bullet("later", "not this one"),
        ];
        let (at, ins) = find_instruction(&blocks, "Scribe").unwrap();
        assert_eq!((at, ins), (4, Instruction::Table));
        assert_eq!(bullets_above(&blocks, at), vec![1, 2]);
        assert!(bullets_above(&blocks, 0).is_empty());
    }

    #[test]
    fn cells_split_on_the_first_separator_the_line_uses() {
        assert_eq!(split_cells("Apples - 3"), vec!["Apples", "3"]);
        assert_eq!(split_cells("Venue: The Hall, room 2"), vec!["Venue", "The Hall, room 2"]);
        assert_eq!(split_cells("Just one"), vec!["Just one"]);
        assert_eq!(
            table_cells(&["a - 1 - x", "b - 2"]),
            vec![vec!["a", "1", "x"], vec!["b", "2", ""]]
        );
    }

    #[test]
    fn cells_are_found_by_table_id_in_row_major_order() {
        let mut c1 = BTreeMap::new();
        c1.insert("table".into(), AttrVal::Str("t1".into()));
        c1.insert("row".into(), AttrVal::Int(1));
        c1.insert("col".into(), AttrVal::Int(0));
        let mut c0 = c1.clone();
        c0.insert("row".into(), AttrVal::Int(0));
        let blocks = vec![
            Block { id: "x".into(), kind: "cell".into(), text: "".into(), attrs: c1 },
            Block { id: "y".into(), kind: "cell".into(), text: "".into(), attrs: c0 },
        ];
        assert_eq!(table_ids(&blocks), vec!["t1".to_string()]);
        let cells = cells_of(&blocks, "t1");
        assert_eq!(cells.iter().map(|c| c.2.as_str()).collect::<Vec<_>>(), vec!["y", "x"]);
    }
}
