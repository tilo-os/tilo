//! Reading instructions out of the document, and the pure parts of
//! carrying them out.
//!
//! An instruction is a paragraph addressed to the agent by name —
//! `Claude: turn the bullets into a table`. Two things it may ask for: the
//! fixed [`CATALOGUE`] of document recipes below, and, through
//! [`Instruction::Perform`], any operation the room's apps currently
//! export — not written down anywhere, but surveyed through
//! [`crate::surface`] and checked against it again at dispatch, never
//! against a remembered list.

use std::collections::{BTreeMap, BTreeSet};

use qdoc::snapshot::{AttrVal, BlockView, DocSnapshot};
use word_processor::actions::{self as wp, WordProcessorAction};

use crate::surface::{self, NoMatch, Offer};

/// What the agent was asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Instruction {
    /// Perform one operation the room exports, named by its address. The
    /// address is the app's own `(resource, action)` pair; the argument is
    /// the text that operation collects, when it collects any. Neither is
    /// interpreted here — resolving it is done against the live surface at
    /// dispatch.
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
    /// Something outside the vocabulary; the agent says so in the document.
    Unknown(String),
}

/// Why an answer did not become an instruction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The id is not one the room exports right now. Nothing is dispatched.
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
        let rest = lower.strip_prefix(&prefix)?;
        Some(rest.trim().to_string())
    }

    /// The keyword vocabulary: the decision the agent reaches on its own.
    ///
    /// This is what runs when no model is available — no key, switched off,
    /// or a call that was slow, failed, or answered outside the catalogue.
    /// It is the floor a run cannot drop below, not the intended path.
    pub fn from_keywords(rest: &str) -> Instruction {
        if rest.contains("table") {
            Instruction::Table
        } else if rest.contains("caption") {
            Instruction::Caption
        } else if rest.contains("heading") || rest.contains("title") {
            Instruction::Heading
        } else {
            Instruction::Unknown(rest.to_string())
        }
    }

    /// A catalogue id, validated against [`CATALOGUE`] — the gate every
    /// answer from a model passes through before anything is dispatched.
    ///
    /// [`NOTHING_FITS`] is a legitimate answer, not a failure: it means no
    /// exported action does what was asked, and the agent says so in the
    /// document instead of guessing. Anything else outside the catalogue is
    /// `None`, and the caller falls back rather than acting on it.
    pub fn from_id(id: &str, rest: &str) -> Option<Instruction> {
        if id == NOTHING_FITS {
            return Some(Instruction::Unknown(rest.to_string()));
        }
        if !CATALOGUE.iter().any(|c| c.id == id) {
            return None;
        }
        match id {
            "table" => Some(Instruction::Table),
            "caption" => Some(Instruction::Caption),
            "heading" => Some(Instruction::Heading),
            _ => None,
        }
    }

    /// Parse `text` as an instruction to `name` using the keyword
    /// vocabulary, or `None` when it is not addressed to the agent.
    pub fn parse(text: &str, name: &str) -> Option<Instruction> {
        Self::addressed(text, name).map(|rest| Self::from_keywords(&rest))
    }

    /// The deterministic floor, widened to the room's own surface: the
    /// fixed [`CATALOGUE`] vocabulary first, and — only when nothing there
    /// matches — whatever the room's apps currently export, matched by
    /// [`surface::from_verbs`].
    ///
    /// The floor never guesses between two operations that fit equally
    /// well; it says so, in [`NoMatch::Ambiguous`], and leaves the choice
    /// to a model or to whoever reads the refusal.
    pub fn from_floor(rest: &str, offers: &[Offer]) -> Instruction {
        let by_keyword = Self::from_keywords(rest);
        if !matches!(by_keyword, Instruction::Unknown(_)) {
            return by_keyword;
        }
        match surface::from_verbs(rest, offers) {
            Ok((offer, argument)) => Instruction::Perform {
                id: offer.id(),
                argument,
            },
            Err(why) => Instruction::Unknown(unmatched(rest, &why)),
        }
    }

    /// The full gate over an answered id: checked against what the room
    /// exports **now**, not against what it exported when the id was
    /// offered.
    ///
    /// [`NOTHING_FITS`] is a legitimate answer, not a failure. An id in
    /// [`CATALOGUE`] resolves exactly as [`Instruction::from_id`] already
    /// does. Anything else must name a live [`Offer`]: an operation that
    /// collects a content hash is never asked to quote one — there is
    /// nowhere honest to get one from an answer — and an operation that
    /// collects text is [`Refused::Unusable`] when neither `supplied` nor a
    /// quoted literal in `rest` gives it any, rather than silently falling
    /// back to the deterministic floor while reporting success.
    ///
    /// # Errors
    ///
    /// [`Refused::NotExported`] when `id` names neither a catalogue entry
    /// nor a live [`Offer`]. [`Refused::Unusable`] when `id` names an offer
    /// that collects text but neither `supplied` nor a quoted literal in
    /// `rest` gives it any.
    pub fn from_answer(
        id: &str,
        rest: &str,
        supplied: Option<&str>,
        offers: &[Offer],
    ) -> Result<Instruction, Refused> {
        if id == NOTHING_FITS {
            return Ok(Instruction::Unknown(rest.to_string()));
        }
        if let Some(instruction) = Self::from_id(id, rest) {
            return Ok(instruction);
        }
        let Some(offer) = surface::find(offers, id) else {
            return Err(Refused::NotExported(id.to_string()));
        };
        if offer.wants_content() {
            return Ok(Instruction::Perform {
                id: offer.id(),
                argument: None,
            });
        }
        if offer.wants_text() {
            let literal = supplied
                .map(str::to_string)
                .or_else(|| surface::quoted_literal(rest))
                .ok_or_else(|| {
                    Refused::Unusable(format!(
                        "{} collects text and neither the answer nor the \
                         instruction supplied any",
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
}

/// Why the floor could not answer, in the words the agent writes into the
/// document. The apps' own labels and nothing else — an address is how the
/// agent dispatches, not something to say to somebody reading the page.
pub fn unmatched(rest: &str, why: &NoMatch) -> String {
    match why {
        NoMatch::Nothing => rest.to_string(),
        NoMatch::Ambiguous(labels) => format!(
            "{rest} — that could mean {}. Which one?",
            labels
                .iter()
                .map(|l| l.to_lowercase())
                .collect::<Vec<_>>()
                .join(" or ")
        ),
        NoMatch::NeedsText(label) => {
            format!(
                "{rest} — {} needs the words to use, in quotes",
                label.to_lowercase()
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Sequences: one request, more than one thing to do.
// ---------------------------------------------------------------------------

/// One thing to do, and where its argument comes from.
///
/// A step is exactly what a whole instruction used to be, plus the one fact
/// a single instruction never needed: whether its argument is something the
/// request said, or something the step before it made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub instruction: Instruction,
    /// Whether this step's argument is the previous step's result.
    ///
    /// Read off the *operation's own declaration* ([`Offer::wants_content`]),
    /// never from a model and never from the words of the request: an
    /// operation that collects a content hash can only be performed with
    /// content something produced, so there is exactly one place its
    /// argument can come from.
    pub carries: bool,
}

impl Step {
    /// A step that stands on its own: every instruction, until now.
    pub fn own(instruction: Instruction) -> Step {
        Step {
            instruction,
            carries: false,
        }
    }

    /// The step a resolved instruction becomes, reading off the survey it
    /// was resolved against whether its argument has to be carried.
    pub fn bound(instruction: Instruction, offers: &[Offer]) -> Step {
        let carries = match &instruction {
            Instruction::Perform { id, .. } => {
                surface::find(offers, id).is_some_and(Offer::wants_content)
            }
            _ => false,
        };
        Step {
            instruction,
            carries,
        }
    }

    /// Whether performing this step starts an application — the one kind of
    /// step that changes what the room exports rather than what a document
    /// says. [`Plan::unanswered`] treats a clause after one differently: a
    /// survey taken before the open ran could never have held the
    /// operation that clause is asking for.
    pub fn opens_an_application(&self) -> bool {
        match &self.instruction {
            Instruction::Perform { id, .. } => surface::decode_address(id)
                .as_ref()
                .and_then(surface::opens)
                .is_some(),
            _ => false,
        }
    }

    /// Whether this step is a refusal rather than something to do.
    pub fn is_unanswered(&self) -> bool {
        matches!(self.instruction, Instruction::Unknown(_))
    }
}

/// An instruction, resolved: the ordered steps that carry it out.
///
/// Most requests are one step and always were. The reason this type exists
/// is the request that is not — "caption the image and give this a title"
/// is one sentence and two operations, and an agent that answered it with a
/// single [`Instruction`] would carry out half of it and report success.
/// There is no honest way to say half a sentence back to somebody, so the
/// shape of an answer has to hold all of it.
///
/// A sequence is not a way to ask for more: every step is resolved against
/// the same discovered surface a single instruction is, and validated again
/// at dispatch. The one thing it adds is that a step's argument may come
/// from the step before it — and even that is not a choice anybody makes,
/// but a fact the operation itself declares ([`Step::carries`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub steps: Vec<Step>,
}

impl Plan {
    /// A plan of one step: what every instruction was until now.
    pub fn of(instruction: Instruction) -> Plan {
        Plan {
            steps: vec![Step::own(instruction)],
        }
    }

    pub fn len(&self) -> usize {
        self.steps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Whether this asks for more than one thing.
    pub fn is_sequence(&self) -> bool {
        self.steps.len() > 1
    }

    /// The first part of the request that resolved to nothing, if any.
    ///
    /// A plan carrying one of these must not be started: working through
    /// the steps until the refusal is reached would leave the earlier ones
    /// applied to somebody's document for a request that, taken whole, was
    /// never going to be finished.
    ///
    /// The one exception is a clause that names an operation an *earlier*
    /// step's own open would create. Such a clause cannot resolve against a
    /// survey taken before that open ran — the application it needs is not
    /// there yet — so it is not fatal here; it is only fatal if nothing
    /// before it opens an application, which is exactly what is checked.
    pub fn unanswered(&self) -> Option<&str> {
        let mut an_open_comes_first = false;
        for s in &self.steps {
            if let Instruction::Unknown(what) = &s.instruction
                && !an_open_comes_first
            {
                return Some(what.as_str());
            }
            if s.opens_an_application() {
                an_open_comes_first = true;
            }
        }
        None
    }

    /// The deterministic floor over a request that may name several things.
    ///
    /// A request is read as a sequence only when it is written as one — the
    /// ordering words in [`CONJUNCTIONS`], outside any quotation — and only
    /// when reading it that way answers something. A single instruction
    /// with "and" inside it ("align left and right") resolves no clause on
    /// its own and is handed to [`Instruction::from_floor`] whole, exactly
    /// as before.
    pub fn from_floor(rest: &str, offers: &[Offer]) -> Plan {
        Plan::assembled(rest, |clause| {
            Step::bound(Instruction::from_floor(clause, offers), offers)
        })
    }

    /// Split a request into its parts and resolve each with `step`, or read
    /// it whole when it turns out not to be several requests after all.
    fn assembled(rest: &str, step: impl Fn(&str) -> Step) -> Plan {
        let parts = clauses(rest);
        if parts.len() < 2 {
            return Plan {
                steps: vec![step(rest)],
            };
        }
        let steps: Vec<Step> = parts.iter().map(|c| step(c)).collect();
        // Nothing in it was a request the room could answer, so the
        // ordering word was a word in one sentence rather than a join
        // between two.
        if steps.iter().all(Step::is_unanswered) {
            return Plan {
                steps: vec![step(rest)],
            };
        }
        // Every part named the same operation, which is one request said
        // twice rather than two to carry out — performing it twice is
        // harmless here.
        if steps.windows(2).all(|w| match w {
            [a, b] => a.instruction == b.instruction,
            _ => false,
        }) {
            let mut steps = steps;
            steps.truncate(1);
            return Plan { steps };
        }
        Plan { steps }
    }
}

/// The words that join one thing to do to the next.
///
/// Ordering words only, and longest first so that `and then` is one join
/// rather than `and` followed by a clause beginning `then`. Deliberately
/// not punctuation on its own: a comma separates the parts of a single
/// request far more often than it separates two.
pub const CONJUNCTIONS: &[&str] = &[
    ", and then ",
    " and then ",
    ", then ",
    " then ",
    ", and ",
    " and ",
];

/// Split a request into the things it asks for, in order.
///
/// Quoted text is never split, for the same reason [`surface::quoted_literal`]
/// exists: what is inside quotation marks is a value somebody supplied, and
/// `rename it to "sound and vision.md"` asks for one thing.
pub fn clauses(rest: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut start = 0usize;
    let mut in_straight = false;
    let mut in_curly = false;
    let mut i = 0usize;
    while i < rest.len() {
        let Some(tail) = rest.get(i..) else { break };
        let Some(ch) = tail.chars().next() else { break };
        match ch {
            '"' => in_straight = !in_straight,
            '\u{201c}' => in_curly = true,
            '\u{201d}' => in_curly = false,
            _ => {}
        }
        if !in_straight && !in_curly {
            // Compared as bytes, not as a substring: the conjunctions are
            // ASCII, and `tail` may begin with a multi-byte character — a
            // typographic quotation mark, in the very case this has to get
            // right — which slicing by a byte length would split.
            if let Some(sep) = CONJUNCTIONS.iter().find(|s| {
                tail.as_bytes()
                    .get(..s.len())
                    .is_some_and(|b| b.eq_ignore_ascii_case(s.as_bytes()))
            }) {
                if let Some(clause) = rest.get(start..i) {
                    out.push(clause.trim().to_string());
                }
                i += sep.len();
                start = i;
                continue;
            }
        }
        i += ch.len_utf8();
    }
    out.push(rest.get(start..).unwrap_or_default().trim().to_string());
    out.retain(|c| !c.is_empty());
    out
}

// ---------------------------------------------------------------------------
// The catalogue: what the agent can be asked for, grounded in the actions the
// word processor exports.
// ---------------------------------------------------------------------------

/// The answer that means "no exported action does what was asked".
pub const NOTHING_FITS: &str = "none";

/// One thing the agent can do, and the exported actions it does it with.
///
/// `actions` names [`WordProcessorAction`] variants. Nothing here is taken on
/// trust: [`dispatched`] builds those very actions with the word processor's
/// own builders, and `the_catalogue_only_offers_actions_the_app_exports`
/// checks the names against what the app's encoder puts on the wire. A
/// renamed or withdrawn action breaks the build or the test, never a run.
pub struct Capability {
    /// The id a model answers with, and the only thing it may answer.
    pub id: &'static str,
    /// What it does, in the words the model is given.
    pub summary: &'static str,
    /// The exported actions carrying it out dispatches.
    pub actions: &'static [&'static str],
}

/// Everything the agent can be asked for. The model chooses from this and
/// nothing else; it never proposes a document mutation of its own.
pub const CATALOGUE: &[Capability] = &[
    Capability {
        id: "table",
        summary: "Turn the run of bullets immediately above the instruction into a table.",
        actions: &["InsertTable", "SpliceText", "RemoveBlocks"],
    },
    Capability {
        id: "caption",
        summary: "Add a caption paragraph underneath the first image in the document.",
        actions: &["InsertParagraph", "RemoveBlocks"],
    },
    Capability {
        id: "heading",
        summary: "Give a rough draft its structure: make the first line the document's \
                  title, and promote the section labels under it to headings.",
        actions: &["SetParagraphStyle", "RemoveBlocks"],
    },
];

/// The actions a capability dispatches, built with the word processor's own
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

/// Every exported action the catalogue can reach, as the app names it.
pub fn exported_actions() -> BTreeSet<String> {
    CATALOGUE
        .iter()
        .flat_map(|c| dispatched(c.id))
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

/// The first paragraph addressed to `name`, decided by the keyword
/// vocabulary. The model-free path, kept for tests and for the fallback.
pub fn find_instruction(blocks: &[Block], name: &str) -> Option<(usize, Instruction)> {
    find_addressed(blocks, name).map(|(i, rest)| (i, Instruction::from_keywords(&rest)))
}

/// The run of bullets the instruction refers to: the consecutive
/// `list_item` blocks immediately above index `at`, skipping blank
/// paragraphs between them and the instruction.
pub fn bullets_above(blocks: &[Block], at: usize) -> Vec<usize> {
    let mut i = at;
    // Skip blank lines between the bullets and the instruction.
    while i > 0 {
        match blocks.get(i - 1) {
            Some(b) if b.kind == "paragraph" && b.text.trim().is_empty() => i -= 1,
            _ => break,
        }
    }
    let mut run = Vec::new();
    while i > 0 {
        match blocks.get(i - 1) {
            Some(b) if b.kind == "list_item" => {
                i -= 1;
                run.push(i);
            }
            _ => break,
        }
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
    let body: Vec<&Block> = blocks
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != instruction)
        .map(|(_, b)| b)
        .filter(|b| b.kind != "cell")
        .filter(|b| !b.text.trim().is_empty())
        .collect();
    let Some((&first, rest)) = body.split_first() else {
        return out;
    };
    out.title = Some(first.id.clone());

    for (n, &b) in rest.iter().enumerate() {
        if b.kind != "paragraph" || !looks_like_a_label(&b.text) {
            continue;
        }
        // The next line of body text has to read as prose, or this is just
        // a short line among short lines.
        let Some(&next) = rest.get(n + 1) else {
            continue;
        };
        let follows_prose = next.kind == "paragraph"
            && !looks_like_a_label(&next.text)
            && next.text.trim().chars().count() > b.text.trim().chars().count();
        if follows_prose {
            out.sections.push(b.id.clone());
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
            para(
                "p1",
                "We closed the quarter ahead of plan, with most of the gain late.",
            ),
            para("s2", "Risks"),
            para(
                "p2",
                "Two suppliers remain single-sourced and that has not moved.",
            ),
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
        // this fails rather than the agent offering something imaginary.
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
            let advertised: BTreeSet<String> = cap
                .actions
                .iter()
                .map(std::string::ToString::to_string)
                .collect();
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

    #[test]
    fn every_catalogue_id_maps_to_an_instruction_and_nothing_else_does() {
        for cap in CATALOGUE {
            assert!(
                Instruction::from_id(cap.id, "asked").is_some(),
                "{} is offered but maps to no instruction",
                cap.id
            );
        }
        assert_eq!(
            Instruction::from_id(NOTHING_FITS, "juggle"),
            Some(Instruction::Unknown("juggle".into()))
        );
        assert_eq!(Instruction::from_id("InsertTable", "x"), None);
        assert_eq!(Instruction::from_id("wander off", "x"), None);
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
    fn instructions_are_addressed_by_name_case_insensitively() {
        assert_eq!(
            Instruction::parse("scribe: turn the bullets into a table", "Scribe"),
            Some(Instruction::Table)
        );
        assert_eq!(
            Instruction::parse("Scribe: caption the image please", "Scribe"),
            Some(Instruction::Caption)
        );
        assert_eq!(
            Instruction::parse("SCRIBE: give this a title", "Scribe"),
            Some(Instruction::Heading)
        );
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
        assert_eq!(
            split_cells("Venue: The Hall, room 2"),
            vec!["Venue", "The Hall, room 2"]
        );
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
            Block {
                id: "x".into(),
                kind: "cell".into(),
                text: String::new(),
                attrs: c1,
            },
            Block {
                id: "y".into(),
                kind: "cell".into(),
                text: String::new(),
                attrs: c0,
            },
        ];
        assert_eq!(table_ids(&blocks), vec!["t1".to_string()]);
        let cells = cells_of(&blocks, "t1");
        assert_eq!(
            cells.iter().map(|c| c.2.as_str()).collect::<Vec<_>>(),
            vec!["y", "x"]
        );
    }

    // -- Instruction::Perform, and the floor that reaches it -------------

    /// An offer built from a real descriptor, so what these tests check is
    /// what a gate checks against a real survey.
    fn offer(
        app: &'static str,
        resource: &str,
        action: &str,
        label: &str,
        collects: Option<qos_actions::Collects>,
    ) -> Offer {
        Offer {
            app,
            descriptor: qos_actions::door::ActionDescriptor {
                address: qos_actions::door::ActionAddress::new(resource, action),
                resource_kind: format!("{app}.thing"),
                resource_label: resource.rsplit(':').next().unwrap_or(resource).to_string(),
                action_label: label.to_string(),
                description: None,
                locus: qos_actions::ExecutionLocus::ClientLocal,
                kind: qos_actions::OperationKind::Document,
                input: match collects {
                    None => qos_actions::InputSpec::None,
                    Some(c) => qos_actions::InputSpec::Text {
                        prompt: "p".into(),
                        placeholder: String::new(),
                        collects: c,
                    },
                },
            },
        }
    }

    /// A real word-processor document with one heading, surveyed through
    /// the real contract, so every assertion below is against what the app
    /// actually exports.
    fn surveyed() -> Vec<Offer> {
        let mut doc = automerge::AutoCommit::new();
        let c = qdoc::model::init(&mut doc, &automerge::ROOT);
        let mut ids = qdoc::model::seeded_id_gen(&doc, &c);
        {
            let mut dm = qdoc::DocMut {
                doc: &mut doc,
                c: &c,
                ids: &mut ids,
            };
            let h = dm.insert_block(None, "heading");
            dm.set_attr(&h, "level", 1i64);
            dm.splice_text(&h, 0, 0, "Notes");
        }
        doc.commit();
        let wp = &surface::CONTRACTS[0];
        assert_eq!(wp.namespace, "word-processor");
        surface::offers(
            &[(wp, &doc)],
            &surface::View::default(),
            &permitting("word-processor"),
        )
    }

    fn permitting(app: &str) -> ui::permissions::PermissionContext {
        use ui::permissions::{Actor, CapOverride, PermissionContext, Scope, Subject, Verb};
        PermissionContext::from_capabilities(
            Actor::agent("Scribe"),
            vec![
                CapOverride::grant(
                    Subject::agent("Scribe"),
                    Scope::App(app.to_string()),
                    Verb::Write,
                )
                .with_id("test"),
            ],
        )
    }

    /// Nothing the agent writes into a document names an address: it is an
    /// internal identifier, and a refusal reciting one reads as a broken
    /// machine to the person the document belongs to.
    #[test]
    fn a_refusal_names_operations_in_words_and_never_an_address() {
        let offers = surveyed();
        assert!(!offers.is_empty(), "nothing to refuse against");

        let mut written = vec![unmatched("juggle three plates", &NoMatch::Nothing)];
        for asked in ["align it", "juggle three plates"] {
            if let Instruction::Unknown(why) = Instruction::from_floor(asked, &offers) {
                written.push(why);
            }
        }
        for text in &written {
            assert!(
                !text.contains(surface::ADDRESS_SEP),
                "an address reached prose a person reads: {text:?}"
            );
        }

        let ambiguous = unmatched(
            "align it",
            &NoMatch::Ambiguous(vec!["Align left".into(), "Align centre".into()]),
        );
        assert!(ambiguous.contains("align left"), "{ambiguous}");
        assert!(ambiguous.contains("align centre"), "{ambiguous}");
    }

    #[test]
    fn the_floor_needs_a_clear_winner_and_says_so_when_there_is_none() {
        let offers = surveyed();

        // "centre" and "notes" together land on exactly one offer: the
        // heading's own alignment, not the document's.
        let one = Instruction::from_floor("centre the Notes", &offers);
        let Instruction::Perform { id, argument } = &one else {
            panic!("a single clear match must resolve to an address, got {one:?}");
        };
        assert_eq!(*argument, None);
        assert!(
            surface::find(&offers, id).is_some_and(|o| o
                .descriptor
                .action_label
                .to_lowercase()
                .contains("centre")),
            "the floor chose {id}"
        );

        // "align" alone fits every alignment action equally.
        let many = Instruction::from_floor("align it", &offers);
        let Instruction::Unknown(why) = &many else {
            panic!("an ambiguous instruction must not resolve, got {many:?}");
        };
        assert!(
            why.contains("Which one?"),
            "ambiguity must be reported as ambiguity: {why}"
        );
        assert!(
            why.contains("align"),
            "and it must name the alternatives, in the app's own words: {why}"
        );
        assert!(
            !why.contains(surface::ADDRESS_SEP),
            "in words, never as addresses: {why}"
        );

        assert!(matches!(
            Instruction::from_floor("juggle three plates", &offers),
            Instruction::Unknown(_)
        ));

        // The fixed vocabulary still wins outright, unaffected by the
        // surveyed surface.
        assert_eq!(
            Instruction::from_floor("turn the bullets into a table", &offers),
            Instruction::Table
        );
    }

    /// The gate answers an address the room really exports, and refuses one
    /// it does not — including an address that was valid against some
    /// other survey.
    #[test]
    fn an_answer_is_checked_against_the_survey_and_not_against_a_list() {
        let offers = surveyed();
        assert!(
            !offers.is_empty(),
            "the survey found nothing to check against"
        );

        for offer in &offers {
            let answered = Instruction::from_answer(&offer.id(), "asked", None, &offers);
            assert_eq!(
                answered,
                Ok(Instruction::Perform {
                    id: offer.id(),
                    argument: None
                }),
                "{} is offered but the gate would not dispatch it",
                offer.id()
            );
        }

        let stale = "word-processor.section:gone-block#align-left";
        assert_eq!(
            Instruction::from_answer(stale, "asked", None, &offers),
            Err(Refused::NotExported(stale.to_string())),
            "an address the survey does not carry must not be dispatched"
        );
        assert_eq!(
            Instruction::from_answer(NOTHING_FITS, "juggle", None, &offers),
            Ok(Instruction::Unknown("juggle".into()))
        );
        assert_eq!(
            Instruction::from_answer("table", "asked", None, &offers),
            Ok(Instruction::Table),
            "a catalogue id still resolves the way it always did"
        );
    }

    /// An operation that collects a content hash is never refused for
    /// having quoted none: there is nothing anybody could quote.
    #[test]
    fn an_operation_that_collects_content_is_not_asked_for_words() {
        let offers = vec![offer(
            "word-processor",
            "word-processor.section:b7",
            "place-image",
            "Place image below",
            Some(qos_actions::Collects::Content),
        )];
        let answered = Instruction::from_answer(
            "word-processor.section:b7#place-image",
            "place image below b7",
            None,
            &offers,
        );
        assert_eq!(
            answered,
            Ok(Instruction::Perform {
                id: "word-processor.section:b7#place-image".into(),
                argument: None,
            })
        );
    }

    /// An operation that collects text needs words from somewhere honest —
    /// the answer itself, or a literal quoted in the instruction — and is
    /// refused rather than performed with a blank when it gets neither.
    #[test]
    fn text_collecting_operations_need_a_literal_or_are_refused() {
        let offers = vec![offer(
            "files",
            "files.entry:/a",
            "rename",
            "Rename",
            Some(qos_actions::Collects::Words),
        )];
        assert!(
            matches!(
                Instruction::from_answer("files.entry:/a#rename", "rename it", None, &offers),
                Err(Refused::Unusable(_))
            ),
            "no literal anywhere must be refused, not performed with a blank"
        );
        assert_eq!(
            Instruction::from_answer(
                "files.entry:/a#rename",
                r#"rename it to "b.md""#,
                None,
                &offers
            ),
            Ok(Instruction::Perform {
                id: "files.entry:/a#rename".into(),
                argument: Some("b.md".into()),
            }),
            "a literal quoted in the instruction is used"
        );
        assert_eq!(
            Instruction::from_answer("files.entry:/a#rename", "rename it", Some("c.md"), &offers),
            Ok(Instruction::Perform {
                id: "files.entry:/a#rename".into(),
                argument: Some("c.md".into()),
            }),
            "a literal the answer itself supplied is used even with no quote in the instruction"
        );
    }

    // -- Sequences: Plan, Step, clauses -----------------------------------

    #[test]
    fn clauses_split_on_ordering_words_outside_quotes() {
        assert_eq!(
            clauses("give this a title and caption the image"),
            vec![
                "give this a title".to_string(),
                "caption the image".to_string()
            ]
        );
        assert_eq!(
            clauses("give this a title, then caption the image"),
            vec![
                "give this a title".to_string(),
                "caption the image".to_string()
            ]
        );
        // "and then" is one join, not "and" followed by a clause starting
        // "then" — the longest match has to win.
        assert_eq!(
            clauses("caption the image and then give this a title"),
            vec![
                "caption the image".to_string(),
                "give this a title".to_string()
            ]
        );
        // A quoted literal that itself contains a join is never split.
        assert_eq!(
            clauses(r#"rename it to "sound and vision.md""#),
            vec![r#"rename it to "sound and vision.md""#.to_string()]
        );
        // A single request with no ordering word is one clause.
        assert_eq!(
            clauses("caption the image"),
            vec!["caption the image".to_string()]
        );
    }

    #[test]
    fn a_plan_reads_a_request_written_as_several_things_as_a_sequence() {
        let plan = Plan::from_floor("give this a title, then caption the image", &[]);
        assert!(plan.is_sequence(), "{plan:?}");
        assert_eq!(plan.len(), 2);
        assert_eq!(plan.steps[0].instruction, Instruction::Heading);
        assert_eq!(plan.steps[1].instruction, Instruction::Caption);
        assert_eq!(plan.unanswered(), None);
    }

    #[test]
    fn a_single_request_with_a_join_inside_it_is_not_split() {
        // "align left and right" resolves no clause on its own — the floor
        // reads it whole, exactly as a request with no join in it at all.
        let plan = Plan::from_floor("align left and right", &[]);
        assert!(!plan.is_sequence(), "{plan:?}");
        assert_eq!(plan.len(), 1);
    }

    #[test]
    fn a_plan_collapses_a_request_that_only_repeats_one_operation() {
        let plan = Plan::from_floor("give this a heading and give it a heading again", &[]);
        assert!(!plan.is_sequence(), "{plan:?}");
        assert_eq!(plan.steps[0].instruction, Instruction::Heading);
    }

    #[test]
    fn a_plan_falls_back_to_the_whole_request_when_nothing_in_it_resolves() {
        // Both clauses come back unanswered, so the join was a word in one
        // sentence rather than a seam between two requests — and the
        // fallback reads the *whole* text, not just the first clause.
        let whole = "juggle three plates and then take a bow";
        let plan = Plan::from_floor(whole, &[]);
        assert_eq!(plan.len(), 1, "{plan:?}");
        assert_eq!(
            plan.steps[0].instruction,
            Instruction::Unknown(whole.to_string())
        );
    }

    #[test]
    fn carries_is_read_off_the_operations_own_declaration_never_guessed() {
        let content = offer(
            "photos",
            "photos.picture:1",
            "crop",
            "Crop",
            Some(qos_actions::Collects::Content),
        );
        let words = offer(
            "files",
            "files.entry:/a",
            "rename",
            "Rename",
            Some(qos_actions::Collects::Words),
        );
        let none = offer(
            "word-processor",
            "word-processor.section:b",
            "align-left",
            "Align left",
            None,
        );
        let offers = vec![content.clone(), words.clone(), none.clone()];

        let perform = |o: &Offer| Instruction::Perform {
            id: o.id(),
            argument: None,
        };
        assert!(
            Step::bound(perform(&content), &offers).carries,
            "an operation that collects content must be carried into"
        );
        assert!(
            !Step::bound(perform(&words), &offers).carries,
            "an operation that collects words is never carried into — it is quoted, not carried"
        );
        assert!(!Step::bound(perform(&none), &offers).carries);

        // Never true for anything but Perform, whatever the survey holds.
        assert!(!Step::bound(Instruction::Table, &offers).carries);
    }

    #[test]
    fn opens_an_application_is_true_only_for_the_open_address() {
        let open = offer(
            "photos",
            "photos.app",
            "open",
            "Open the photo editor",
            None,
        );
        let ordinary = offer("photos", "photos.picture:1", "crop", "Crop", None);
        assert!(
            Step::own(Instruction::Perform {
                id: open.id(),
                argument: None
            })
            .opens_an_application()
        );
        assert!(
            !Step::own(Instruction::Perform {
                id: ordinary.id(),
                argument: None
            })
            .opens_an_application()
        );
        assert!(!Step::own(Instruction::Table).opens_an_application());
    }

    #[test]
    fn unanswered_refuses_a_sequence_with_no_open_ahead_of_the_gap() {
        let plan = Plan {
            steps: vec![
                Step::own(Instruction::Table),
                Step::own(Instruction::Unknown("juggle three plates".into())),
            ],
        };
        assert_eq!(plan.unanswered(), Some("juggle three plates"));
    }

    /// A step naming what only an earlier open would create is not fatal —
    /// but only once that open has actually run first.
    #[test]
    fn an_open_excuses_a_clause_it_makes_answerable_but_only_once_it_comes_first() {
        let open = offer(
            "photos",
            "photos.app",
            "open",
            "Open the photo editor",
            None,
        );
        let excused = Plan {
            steps: vec![
                Step::own(Instruction::Perform {
                    id: open.id(),
                    argument: None,
                }),
                Step::own(Instruction::Unknown("crop it square".into())),
            ],
        };
        assert_eq!(
            excused.unanswered(),
            None,
            "a clause an earlier open would make answerable is not fatal up front"
        );

        let too_early = Plan {
            steps: vec![
                Step::own(Instruction::Unknown("crop it square".into())),
                Step::own(Instruction::Perform {
                    id: open.id(),
                    argument: None,
                }),
            ],
        };
        assert_eq!(
            too_early.unanswered(),
            Some("crop it square"),
            "nothing has opened anything yet, so this gap is still fatal"
        );
    }
}
