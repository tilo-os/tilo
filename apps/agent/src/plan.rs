//! Reading instructions out of the document, and the pure parts of
//! carrying them out.
//!
//! An instruction is a paragraph addressed to the agent by name —
//! `Claude: turn the bullets into a table`. The agent reads the same
//! document everyone else does, and answers in it.
//!
//! ## What it may be asked for
//!
//! Two things, and the first of them is open-ended:
//!
//! * **Any operation the room's apps currently export.** These are not
//!   written down anywhere. [`crate::surface`] asks each app for the
//!   resource tree it is publishing and surveys it, so what the agent can
//!   be asked for is whatever is open at that moment, in whichever app —
//!   a section of a document, a file in a folder — addressed by the app's
//!   own ids. This is [`Instruction::Perform`].
//! * **A small set of document [`RECIPES`]**: several of the word
//!   processor's operations composed into one editorial change, with the
//!   choice of *which* blocks decided here from the document. No app
//!   exports "turn the bullets above this line into a table" as a single
//!   operation, and a composition is not something a survey can discover,
//!   so these stay named. They are offered only when a word-processor
//!   document is actually in the room.
//!
//! ## The gate
//!
//! Whatever chose — a model, or the deterministic floor — the answer is
//! checked against the surface as it stands **at dispatch**, not as it
//! stood when the offer was made. An id that names nothing now is
//! [`Refused::NotExported`] and nothing is dispatched; it is never
//! quietly turned into the nearest thing that does resolve.

use std::collections::{BTreeMap, BTreeSet};

use qdoc::snapshot::{AttrVal, BlockView, DocSnapshot};
use word_processor::actions::{self as wp, WordProcessorAction};

use crate::surface::{self, NoMatch, Offer};

/// What the agent was asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Instruction {
    /// Perform one operation the room exports, named by its address.
    ///
    /// The address is the app's own `(resource, action)` pair and the
    /// argument is the text that operation collects, when it collects any.
    /// Neither is interpreted here: what this variant carries is a name,
    /// and resolving it is done against the live surface at dispatch.
    Perform {
        /// `"<resource id>#<action id>"`.
        id: String,
        /// The text the operation collects, when it collects any.
        argument: Option<String>,
    },
    /// Turn the run of bullets above the instruction into a table.
    Table,
    /// Put a caption under the (first) image.
    Caption,
    /// Make the first line a title.
    Heading,
    /// Write out the facts above the instruction as a table the model
    /// drafted — the columns, the row order, and what was left out are its
    /// reading of the document, not a split on separators.
    RunSheet(TableDraft),
    /// Something outside the vocabulary; the agent says so in the document.
    Unknown(String),
}

/// A table the model drafted: **content, never operations.**
///
/// This is the one place a model's own words reach the document, and the
/// boundary is what makes that safe to allow. What arrives here is text for
/// cells. What does not, and cannot, arrive here is a block id, a range to
/// delete, a choice of which blocks go away, or an action name — the agent
/// decides all of that from the document, and dispatches it through the
/// word processor's exported actions exactly as the toolbar would.
///
/// So the failure a bad answer can cause is a table that reads poorly. It
/// is not a document the model rewrote, because nothing here can name a
/// thing to rewrite.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableDraft {
    /// The column headings, which become the table's first row.
    pub headers: Vec<String>,
    /// The body rows, each the same width as `headers`.
    pub rows: Vec<Vec<String>>,
    /// One line to put under the table — what the model noticed and the
    /// table itself cannot say. `None` when there was nothing to add.
    pub note: Option<String>,
}

/// Bounds on a drafted table.
///
/// Not defensive decoration: this content is generated, and the document
/// it lands in is shared and persistent. The caps are what stop a
/// pathological answer from being a pathological document — and they are
/// checked before the first action is dispatched, so a draft that breaks
/// them changes nothing at all rather than landing half-written.
const MAX_COLS: usize = 6;
const MAX_ROWS: usize = 20;
const MAX_CELL_CHARS: usize = 120;
const MAX_NOTE_CHARS: usize = 240;

impl TableDraft {
    /// Whether this is a table the app can be asked to write, and if not,
    /// why — in words that go in a log rather than the document.
    ///
    /// Every row must be exactly as wide as the header. A ragged answer is
    /// refused rather than padded: padding invents a claim about which
    /// column a value belongs to, and the whole point of this table is
    /// that the columns mean something.
    pub fn check(&self) -> Result<(), String> {
        if self.headers.is_empty() {
            return Err("it has no columns".to_string());
        }
        if self.headers.len() > MAX_COLS {
            return Err(format!(
                "it has {} columns and the most that may be written is {MAX_COLS}",
                self.headers.len()
            ));
        }
        if self.rows.is_empty() {
            return Err("it has no rows".to_string());
        }
        if self.rows.len() > MAX_ROWS {
            return Err(format!(
                "it has {} rows and the most that may be written is {MAX_ROWS}",
                self.rows.len()
            ));
        }
        for (i, row) in self.rows.iter().enumerate() {
            if row.len() != self.headers.len() {
                return Err(format!(
                    "row {} has {} cells and the header has {}",
                    i + 1,
                    row.len(),
                    self.headers.len()
                ));
            }
        }
        for cell in self.headers.iter().chain(self.rows.iter().flatten()) {
            if cell.chars().count() > MAX_CELL_CHARS {
                return Err(format!(
                    "a cell is {} characters and the most that may be written is {MAX_CELL_CHARS}",
                    cell.chars().count()
                ));
            }
        }
        if self.headers.iter().all(|h| h.trim().is_empty())
            && self.rows.iter().flatten().all(|c| c.trim().is_empty())
        {
            return Err("every cell in it is blank".to_string());
        }
        if let Some(n) = &self.note {
            if n.chars().count() > MAX_NOTE_CHARS {
                return Err(format!(
                    "the note is {} characters and the most that may be written is {MAX_NOTE_CHARS}",
                    n.chars().count()
                ));
            }
        }
        Ok(())
    }

    /// The table as it will be written: the header row, then the body.
    pub fn grid(&self) -> Vec<Vec<String>> {
        let mut g = vec![self.headers.clone()];
        g.extend(self.rows.iter().cloned());
        g
    }

    /// The note, trimmed, if there is one worth writing.
    pub fn note_text(&self) -> Option<&str> {
        self.note
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
    }
}

/// Why an answer did not become an instruction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The id is not one the app exports. Nothing is dispatched.
    NotExported(String),
    /// The id is exported, but the content that came with it cannot be
    /// written as it stands.
    Unusable(String),
}

impl Instruction {
    /// The body of a paragraph addressed to `name` (case-insensitive), or
    /// `None` when it is not addressed to the agent at all.
    ///
    /// Who was spoken to is decided here and only here. It is deliberately
    /// not part of the choice a model makes: a document can ask the agent
    /// for something, but nothing written in a document can make the agent
    /// answer to a different name.
    pub fn addressed(text: &str, name: &str) -> Option<String> {
        let t = text.trim();
        let prefix = format!("{}:", name.to_lowercase());
        let lower = t.to_lowercase();
        if !lower.starts_with(&prefix) {
            return None;
        }
        Some(lower[prefix.len()..].trim().to_string())
    }

    /// The deterministic floor: the decision the agent reaches on its own
    /// when no model answered — no key, switched off, or a call that was
    /// slow, failed, or answered outside what the room exports.
    ///
    /// A fixed keyword vocabulary was the floor while the agent could do
    /// exactly four things. It cannot be one now: the set of operations is
    /// whatever the room's apps happen to be publishing, and no list
    /// written here could enumerate it. So the floor matches the
    /// instruction's own words against the words the **apps** put on their
    /// operations, and insists on a clear winner.
    ///
    /// The named [`RECIPES`] keep their keywords, because they are the one
    /// part of the vocabulary that is still finite. They are tried first:
    /// "turn the bullets into a table" means the composition, not the word
    /// processor's bare `Insert table`.
    ///
    /// What this must never do is succeed quietly at something else. Every
    /// way of not finding an answer returns [`Instruction::Unknown`] with
    /// the reason in it, and the agent says so in the document.
    pub fn from_floor(rest: &str, offers: &[Offer]) -> Instruction {
        if let Some(recipe) = Recipe::from_keywords(rest) {
            if recipe.available(offers) {
                return recipe.instruction();
            }
        }
        match surface::from_verbs(rest, offers) {
            Ok((offer, argument)) => Instruction::Perform {
                id: offer.id(),
                argument,
            },
            Err(why) => Instruction::Unknown(unmatched(rest, &why)),
        }
    }

    /// An answered id, validated against what the room exports right now —
    /// the gate every answer passes through before anything is dispatched.
    ///
    /// [`NOTHING_FITS`] is a legitimate answer, not a failure: it means no
    /// exported operation does what was asked, and the agent says so in the
    /// document instead of guessing. Anything else that does not resolve is
    /// [`Refused::NotExported`], and the caller falls back rather than
    /// acting on it.
    pub fn from_id(id: &str, rest: &str, offers: &[Offer]) -> Option<Instruction> {
        Self::from_answer(id, rest, None, offers).ok()
    }

    /// The full gate: an id, plus whatever content came with it, checked
    /// against the surface as it stands.
    ///
    /// Both halves are checked here and nothing is dispatched until both
    /// pass. The id must name something the room exports *now*; the content
    /// must be something the app can write. An answer that needs content
    /// and did not get any is [`Refused::Unusable`] rather than silently
    /// doing the deterministic thing instead — a run that quietly
    /// downgrades to the floor while reporting success is the failure this
    /// whole module exists to make visible.
    ///
    /// Passing a freshly taken survey is the caller's job, and it is what
    /// makes this a dispatch-time check rather than a plan-time one: an
    /// address that was valid when it was offered and is not valid now
    /// fails here.
    pub fn from_answer(
        id: &str,
        rest: &str,
        table: Option<TableDraft>,
        offers: &[Offer],
    ) -> Result<Instruction, Refused> {
        if id == NOTHING_FITS {
            return Ok(Instruction::Unknown(rest.to_string()));
        }
        if let Some(recipe) = Recipe::from_id(id) {
            if !recipe.available(offers) {
                return Err(Refused::NotExported(id.to_string()));
            }
            return recipe.with_content(table);
        }
        let Some(offer) = surface::find(offers, id) else {
            return Err(Refused::NotExported(id.to_string()));
        };
        // An operation that collects text needs text, and there is nowhere
        // to get it from an answer that named only the operation. Refusing
        // is the honest result: performing it with an empty string would
        // write a blank where the caller asked for a value.
        if offer.wants_text() {
            let literal = surface::quoted_literal(rest).ok_or_else(|| {
                Refused::Unusable(format!(
                    "{} collects text and the instruction quoted none",
                    offer.descriptor.action_label
                ))
            })?;
            return Ok(Instruction::Perform {
                id: offer.id(),
                argument: Some(literal),
            });
        }
        Ok(Instruction::Perform {
            id: offer.id(),
            argument: None,
        })
    }

    /// Parse `text` as an instruction to `name` using the deterministic
    /// floor, or `None` when it is not addressed to the agent.
    pub fn parse(text: &str, name: &str, offers: &[Offer]) -> Option<Instruction> {
        Self::addressed(text, name).map(|rest| Self::from_floor(&rest, offers))
    }
}

/// Why the floor could not answer, in the words the agent writes into the
/// document.
fn unmatched(rest: &str, why: &NoMatch) -> String {
    match why {
        NoMatch::Nothing => rest.to_string(),
        NoMatch::Ambiguous(ids) => format!(
            "{rest} (that could mean any of {} — say which)",
            ids.join(", ")
        ),
        NoMatch::NeedsText(id) => {
            format!("{rest} ({id} needs the text to use, in quotes)")
        }
    }
}

// ---------------------------------------------------------------------------
// The recipes: compositions no single exported operation covers.
// ---------------------------------------------------------------------------

/// The answer that means "nothing the room exports does what was asked".
pub const NOTHING_FITS: &str = "none";

/// One editorial change made of several of the word processor's operations.
///
/// These are named rather than discovered, and the reason is worth being
/// exact about. A survey enumerates operations; it cannot enumerate
/// *compositions* of them, and it certainly cannot decide which blocks a
/// composition should act on. "Turn the bullets above this line into a
/// table" is a table insert, a splice per cell and a removal, over blocks
/// chosen by reading the document — no app exports it as one operation and
/// none should.
///
/// So the agent keeps them, and they sit **alongside** the surveyed surface
/// rather than being the whole of what it can do. Nothing here is taken on
/// trust either: [`dispatched`] builds the very actions each one performs
/// with the word processor's own builders, and
/// `every_recipe_only_dispatches_actions_the_app_exports` checks the names
/// against what the app's encoder puts on the wire. A renamed or withdrawn
/// action breaks the build or the test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recipe {
    /// The id an answer names it by.
    pub id: &'static str,
    /// What it does, in the words a model is given.
    pub summary: &'static str,
    /// The exported actions carrying it out dispatches.
    pub actions: &'static [&'static str],
    /// Whether it needs content drafted for it before it can be carried out.
    pub needs_table: bool,
}

/// The named compositions, all of them over a word-processor document.
pub const RECIPES: &[Recipe] = &[
    Recipe {
        id: "table",
        summary: "Turn the run of bullets immediately above the instruction into a table.",
        actions: &["InsertTable", "SpliceText", "RemoveBlocks"],
        needs_table: false,
    },
    Recipe {
        id: "caption",
        summary: "Add a caption paragraph underneath the first image in the document.",
        actions: &["InsertParagraph", "RemoveBlocks"],
        needs_table: false,
    },
    Recipe {
        id: "heading",
        summary: "Give a rough draft its structure: make the first line the document's \
                  title, and promote the section labels under it to headings.",
        actions: &["SetParagraphStyle", "RemoveBlocks"],
        needs_table: false,
    },
    Recipe {
        id: "runsheet",
        summary: "Draft the notes above the instruction into a table yourself, and \
                  supply it in `table`. Choose the columns — as few as the facts \
                  need, and none that would sit mostly empty. Put the rows in the \
                  order they actually belong in rather than the order they were \
                  written. Fold in anything stated only in the prose around them. \
                  Leave out anything that is not that kind of fact entirely: drop \
                  the row, rather than keeping it with a blank or a placeholder \
                  in the column it does not have. Then check the result against \
                  the rest of the document and against what you know: if anything \
                  contradicts itself, cannot be right, or would not work in \
                  practice, say so in one line in `note` and leave the facts as \
                  they were — do not silently correct them.",
        actions: &["InsertTable", "SpliceText", "InsertParagraph", "RemoveBlocks"],
        needs_table: true,
    },
];

/// The namespace whose documents the recipes act on. A recipe is only
/// offered when this app is actually publishing something in the room.
const RECIPE_APP: &str = "word-processor";

impl Recipe {
    /// The recipe an id names.
    pub fn from_id(id: &str) -> Option<&'static Recipe> {
        RECIPES.iter().find(|r| r.id == id)
    }

    /// The recipe the instruction's own words name, if any.
    ///
    /// The one part of the old keyword vocabulary that survives, because it
    /// is the one part that is still a closed set. "run sheet" reaches
    /// `table` here and `runsheet` through a model, and the difference is
    /// the point rather than a wrinkle: the model drafts the rows and can
    /// say something about them, this turns the bullets above into a table
    /// and no more.
    pub fn from_keywords(rest: &str) -> Option<&'static Recipe> {
        let r = rest.to_lowercase();
        let id = if r.contains("table") || r.contains("run sheet") || r.contains("runsheet") {
            "table"
        } else if r.contains("caption") {
            "caption"
        } else if r.contains("heading") || r.contains("title") {
            "heading"
        } else {
            return None;
        };
        Self::from_id(id)
    }

    /// Whether the app these act on is publishing anything in the room.
    ///
    /// Grounded in the survey rather than assumed: with no word-processor
    /// document reachable there is nothing for a recipe to edit, and
    /// offering one would be offering an operation on a document that is
    /// not there.
    pub fn available(&self, offers: &[Offer]) -> bool {
        offers.iter().any(|o| o.app == RECIPE_APP)
    }

    /// The instruction this recipe becomes when it needs no content.
    pub fn instruction(&self) -> Instruction {
        match self.id {
            "caption" => Instruction::Caption,
            "heading" => Instruction::Heading,
            // `runsheet` never reaches here — `needs_table` sends it through
            // `with_content` — and `table` is the remaining case.
            _ => Instruction::Table,
        }
    }

    /// The instruction this recipe becomes given the content that came with
    /// it. A recipe that needs a table and got none is refused whole rather
    /// than quietly doing the deterministic thing instead.
    pub fn with_content(&self, table: Option<TableDraft>) -> Result<Instruction, Refused> {
        if !self.needs_table {
            return Ok(self.instruction());
        }
        let draft =
            table.ok_or_else(|| Refused::Unusable("it came with no table to write".to_string()))?;
        draft.check().map_err(Refused::Unusable)?;
        Ok(Instruction::RunSheet(draft))
    }
}

/// The actions a recipe dispatches, built with the word processor's own
/// builders — the same calls [`crate::session`] makes to carry the work out.
/// The arguments are placeholders; only the shape of the action matters.
pub fn dispatched(id: &str) -> Vec<WordProcessorAction> {
    let block = || "block".to_string();
    match id {
        "table" => vec![
            wp::insert_table_action("block", 1, 1),
            wp::splice_text_action(wp::RegionKind::Body, "block", 0, 0, ""),
            wp::remove_blocks_action(vec![block()]),
        ],
        "caption" => vec![
            wp::insert_paragraph_action("block", ""),
            wp::remove_blocks_action(vec![block()]),
        ],
        "heading" => vec![
            wp::set_paragraph_style_action(vec![block()], "heading", Some(1), None),
            wp::remove_blocks_action(vec![block()]),
        ],
        "runsheet" => vec![
            wp::insert_table_action("block", 1, 1),
            wp::splice_text_action(wp::RegionKind::Body, "block", 0, 0, ""),
            wp::insert_paragraph_action("block", ""),
            wp::remove_blocks_action(vec![block()]),
        ],
        _ => Vec::new(),
    }
}

/// The name the word processor's own encoder gives an action: the key of the
/// envelope `encode` puts on the wire. Reading it back from the app rather
/// than writing it down here is what makes the catalogue's claims checkable.
pub fn exported_name(action: &WordProcessorAction) -> Option<String> {
    let encoded = wp::encode(action);
    match serde_json::from_slice::<serde_json::Value>(&encoded.payload).ok()? {
        serde_json::Value::Object(map) => map.keys().next().cloned(),
        // A unit variant encodes as a bare string.
        serde_json::Value::String(s) => Some(s),
        _ => None,
    }
}

/// Every exported action the recipes reach, as the app names it.
pub fn exported_actions() -> BTreeSet<String> {
    RECIPES
        .iter()
        .flat_map(|r| dispatched(r.id))
        .filter_map(|a| exported_name(&a))
        .collect()
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

/// The first paragraph addressed to `name`, with its index — and the body of
/// what it asked for, still undecided. Deciding is [`crate::model`]'s job.
pub fn find_addressed(blocks: &[Block], name: &str) -> Option<(usize, String)> {
    blocks.iter().enumerate().find_map(|(i, b)| {
        if b.kind != "paragraph" {
            return None;
        }
        Instruction::addressed(&b.text, name).map(|rest| (i, rest))
    })
}

/// The first paragraph addressed to `name`, decided by the deterministic
/// floor against what the room currently exports. The model-free path, kept
/// for tests and for the fallback.
pub fn find_instruction(
    blocks: &[Block],
    name: &str,
    offers: &[Offer],
) -> Option<(usize, Instruction)> {
    find_addressed(blocks, name).map(|(i, rest)| (i, Instruction::from_floor(&rest, offers)))
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

/// What a structural pass over a rough draft would change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Structure {
    /// The first line of text: the document's title.
    pub title: Option<String>,
    /// Later lines that read as section labels rather than prose.
    pub sections: Vec<String>,
}

/// Whether `text` reads as a section label rather than a sentence: short,
/// few words, and not punctuated like prose. Deliberately conservative —
/// promoting a real sentence to a heading is the ugly failure here, and a
/// missed section only leaves the draft as it already was.
fn looks_like_a_label(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() || t.chars().count() > 60 {
        return false;
    }
    if t.split_whitespace().count() > 8 {
        return false;
    }
    !t.ends_with(['.', '?', '!', ','])
}

/// Read a rough draft's structure: the first line of text is the title, and
/// each later short unpunctuated line that is *followed by prose* is a
/// section heading. Requiring the following prose is what keeps a list of
/// short lines — an address, a set of bullets — from becoming a stack of
/// headings.
pub fn structure(blocks: &[Block], instruction: usize) -> Structure {
    let mut out = Structure::default();
    let body: Vec<usize> = (0..blocks.len())
        .filter(|&i| i != instruction)
        .filter(|&i| blocks[i].kind != "cell")
        .filter(|&i| !blocks[i].text.trim().is_empty())
        .collect();
    let Some((&first, rest)) = body.split_first() else {
        return out;
    };
    out.title = Some(blocks[first].id.clone());

    for (n, &i) in rest.iter().enumerate() {
        if blocks[i].kind != "paragraph" || !looks_like_a_label(&blocks[i].text) {
            continue;
        }
        // The next line of body text has to read as prose, or this is just
        // a short line among short lines.
        let Some(&next) = rest.get(n + 1) else {
            continue;
        };
        let follows_prose = blocks[next].kind == "paragraph"
            && !looks_like_a_label(&blocks[next].text)
            && blocks[next].text.trim().chars().count() > blocks[i].text.trim().chars().count();
        if follows_prose {
            out.sections.push(blocks[i].id.clone());
        }
    }
    out
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
    fn a_structural_pass_finds_the_title_and_the_section_labels() {
        let blocks = vec![
            para("t", "Quarterly review"),
            para("s1", "Revenue"),
            para("p1", "We closed the quarter ahead of plan, with most of the gain late."),
            para("s2", "Risks"),
            para("p2", "Two suppliers remain single-sourced and that has not moved."),
            para("ins", "Claude: make this a proper document"),
        ];
        let found = structure(&blocks, 5);
        assert_eq!(found.title.as_deref(), Some("t"));
        assert_eq!(found.sections, vec!["s1".to_string(), "s2".to_string()]);
    }

    #[test]
    fn a_structural_pass_leaves_prose_and_bare_short_lines_alone() {
        // A run of short lines with no prose under them is a list, not a
        // stack of headings — promoting them all is the ugly failure.
        let blocks = vec![
            para("t", "Shopping"),
            para("a", "Apples"),
            para("b", "Pears"),
            para("c", "Plums"),
            para("ins", "Claude: make this a proper document"),
        ];
        let found = structure(&blocks, 4);
        assert_eq!(found.title.as_deref(), Some("t"));
        assert!(found.sections.is_empty(), "{:?}", found.sections);

        // A sentence is never a label, however it is followed.
        assert!(!looks_like_a_label("We closed the quarter ahead of plan."));
        assert!(!looks_like_a_label(
            "A line long enough to be prose even without a full stop at its end"
        ));
        assert!(looks_like_a_label("Revenue"));
        assert!(looks_like_a_label("Open questions"));
    }

    #[test]
    fn a_structural_pass_on_an_empty_draft_asks_for_nothing() {
        let blocks = vec![para("ins", "Claude: make this a proper document")];
        assert_eq!(structure(&blocks, 0), Structure::default());
    }

    #[test]
    fn the_catalogue_only_offers_actions_the_app_exports() {
        // The names the model is shown are not a list kept by hand here:
        // each one must be what the word processor's own encoder calls the
        // action its own builder produced. Rename or withdraw an action and
        // this fails rather than the demo offering something imaginary.
        let exported = exported_actions();
        assert!(!exported.is_empty(), "the app exports nothing");
        for cap in CATALOGUE {
            let built: BTreeSet<String> = dispatched(cap.id)
                .iter()
                .filter_map(exported_name)
                .collect();
            assert!(
                !built.is_empty(),
                "{} dispatches nothing the app can encode",
                cap.id
            );
            let advertised: BTreeSet<String> =
                cap.actions.iter().map(|a| a.to_string()).collect();
            assert_eq!(
                advertised, built,
                "{} advertises {:?} but dispatches {:?}",
                cap.id, advertised, built
            );
            for name in &advertised {
                assert!(
                    exported.contains(name),
                    "{} offers {name}, which the app does not export",
                    cap.id
                );
            }
        }
    }

    /// A well-formed drafted table, for entries that call for content.
    fn sample_draft() -> TableDraft {
        TableDraft {
            headers: vec!["Time".into(), "What".into()],
            rows: vec![vec!["18:00".into(), "Doors".into()]],
            note: Some("Sound check is listed after doors open.".into()),
        }
    }

    #[test]
    fn every_catalogue_id_maps_to_an_instruction_and_nothing_else_does() {
        // Every offered id must be actionable — either on its own, or once
        // it is given the content that entry asks for. What none of them
        // may be is `NotExported`, which would mean the catalogue offers
        // the model something this gate would then refuse.
        for cap in CATALOGUE {
            match Instruction::from_answer(cap.id, "asked", None) {
                Ok(_) => {}
                Err(Refused::Unusable(_)) => {
                    // It wants content. With content it must work.
                    assert!(
                        Instruction::from_answer(cap.id, "asked", Some(sample_draft())).is_ok(),
                        "{} is offered but is unusable even with a well-formed table",
                        cap.id
                    );
                }
                Err(Refused::NotExported(_)) => {
                    panic!("{} is offered to the model but maps to no instruction", cap.id)
                }
            }
        }
        assert_eq!(
            Instruction::from_id(NOTHING_FITS, "juggle"),
            Some(Instruction::Unknown("juggle".into()))
        );
        assert_eq!(Instruction::from_id("InsertTable", "x"), None);
        assert_eq!(Instruction::from_id("wander off", "x"), None);
    }

    #[test]
    fn a_drafted_table_is_checked_before_any_of_it_is_written() {
        // The good case: exactly as wide as its header, and it becomes an
        // instruction carrying the content.
        let ok = sample_draft();
        assert_eq!(ok.check(), Ok(()));
        assert_eq!(
            Instruction::from_answer("runsheet", "asked", Some(ok.clone())),
            Ok(Instruction::RunSheet(ok.clone()))
        );
        // The header is the first row, so what gets written is the header
        // plus the body and nothing is silently dropped.
        assert_eq!(
            ok.grid(),
            vec![
                vec!["Time".to_string(), "What".to_string()],
                vec!["18:00".to_string(), "Doors".to_string()],
            ]
        );

        let bad = |d: TableDraft| {
            let e = d.check();
            assert!(e.is_err(), "{d:?} should not be writable");
            // Whatever the reason, it is refused as unusable rather than
            // treated as an id the app does not export — the two send a
            // reader to different places.
            assert!(matches!(
                Instruction::from_answer("runsheet", "asked", Some(d)),
                Err(Refused::Unusable(_))
            ));
        };

        // Ragged: a row that does not match the header is refused, never
        // padded — padding would invent which column a value sits in.
        bad(TableDraft {
            headers: vec!["Time".into(), "What".into()],
            rows: vec![vec!["18:00".into()]],
            note: None,
        });
        bad(TableDraft::default());
        bad(TableDraft {
            headers: vec!["A".into()],
            rows: vec![],
            note: None,
        });
        // Blank all the way through says nothing and would land as an
        // empty grid in a shared document.
        bad(TableDraft {
            headers: vec![" ".into()],
            rows: vec![vec!["".into()]],
            note: None,
        });
        // The caps: a generated table lands in a persistent document, so
        // there is a size past which it is refused whole.
        bad(TableDraft {
            headers: (0..MAX_COLS + 1).map(|i| i.to_string()).collect(),
            rows: vec![(0..MAX_COLS + 1).map(|i| i.to_string()).collect()],
            note: None,
        });
        bad(TableDraft {
            headers: vec!["A".into()],
            rows: (0..MAX_ROWS + 1).map(|i| vec![i.to_string()]).collect(),
            note: None,
        });
        bad(TableDraft {
            headers: vec!["A".into()],
            rows: vec![vec!["x".repeat(MAX_CELL_CHARS + 1)]],
            note: None,
        });
        bad(TableDraft {
            headers: vec!["A".into()],
            rows: vec![vec!["ok".into()]],
            note: Some("n".repeat(MAX_NOTE_CHARS + 1)),
        });

        // Naming the entry without drafting anything is unusable too: it
        // must not quietly fall through to the deterministic table.
        assert!(matches!(
            Instruction::from_answer("runsheet", "asked", None),
            Err(Refused::Unusable(_))
        ));
    }

    #[test]
    fn a_drafted_table_can_carry_only_text_for_cells() {
        // The shape of the payload is the security boundary: there is
        // nowhere in it to put a block id, a range, or an action name, so
        // the worst a bad answer can produce is a poor table. This is a
        // structural claim, so it is asserted structurally — if a field
        // is ever added that names part of the document, this stops
        // compiling and someone has to think about it.
        let TableDraft {
            headers: _,
            rows: _,
            note: _,
        } = TableDraft::default();
    }

    #[test]
    fn a_note_is_only_written_when_there_is_something_in_it() {
        let with = TableDraft {
            note: Some("  Sound check is after doors.  ".into()),
            ..sample_draft()
        };
        assert_eq!(with.note_text(), Some("Sound check is after doors."));
        let blank = TableDraft {
            note: Some("   ".into()),
            ..sample_draft()
        };
        assert_eq!(blank.note_text(), None);
        let none = TableDraft {
            note: None,
            ..sample_draft()
        };
        assert_eq!(none.note_text(), None);
    }

    #[test]
    fn who_was_addressed_is_read_from_the_document_not_decided() {
        assert_eq!(
            Instruction::addressed("Scribe: turn the bullets into a table", "Scribe"),
            Some("turn the bullets into a table".to_string())
        );
        // Nothing written in a document makes the agent answer to another
        // name, or to a sentence that merely mentions it.
        assert_eq!(Instruction::addressed("Dear Scribe: table", "Scribe"), None);
        assert_eq!(Instruction::addressed("Scribe: table", "Ada"), None);
        assert_eq!(
            Instruction::addressed("Scribe:", "Scribe"),
            Some(String::new())
        );
    }

    #[test]
    fn a_run_sheet_is_understood_without_a_model() {
        // The keyword vocabulary is what answers when no model is
        // reachable, and asking for a run sheet in those words used to
        // land in `Unknown` — so the agent's reply to the only
        // instruction it is ever given was that it did not understand
        // it. Spelt both ways, because people write it both ways.
        assert_eq!(
            Instruction::parse("Scribe: make this a run sheet", "Scribe"),
            Some(Instruction::Table)
        );
        assert_eq!(
            Instruction::parse("Scribe: build a runsheet from these", "Scribe"),
            Some(Instruction::Table)
        );
        // And the floor still has one: an instruction it genuinely has
        // no answer for is still reported as such rather than quietly
        // turned into the nearest thing it can do.
        assert_eq!(
            Instruction::parse("Scribe: book the venue", "Scribe"),
            Some(Instruction::Unknown("book the venue".into()))
        );
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
