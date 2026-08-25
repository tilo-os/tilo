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
    /// Drop rows with nothing in them.
    ///
    /// A row of empty cells is not a row; on the page it is a band of
    /// ruled whitespace in the middle of a table, and it was drafted in a
    /// live run of the very beat this exists for. The recipe already asks
    /// for the row to be dropped rather than kept with a blank in it, so
    /// this enforces what the instruction says instead of leaving it to
    /// be followed.
    ///
    /// Dropped rather than refused, deliberately. Refusing the draft
    /// would trade a table with one blank line in it for a refusal, and
    /// the blank line is a smaller fault than not answering at all. What
    /// is *not* done here is padding a ragged row: that would invent a
    /// claim about which column a value belongs to, and
    /// [`Self::check`] still refuses those.
    pub fn drop_blank_rows(&mut self) {
        self.rows.retain(|r| r.iter().any(|c| !c.trim().is_empty()));
    }

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
        // An operation that collects content is left without one here on
        // purpose. Its argument is a content hash, which no instruction
        // contains and no answer may invent; it is bound at dispatch from
        // what the step before it produced, or the step is refused. See
        // [`Step::carries`].
        if offer.wants_content() {
            return Ok(Instruction::Perform {
                id: offer.id(),
                argument: None,
            });
        }
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

// ---------------------------------------------------------------------------
// Sequences: one request, more than one thing to do.
// ---------------------------------------------------------------------------

/// One thing to do, and where the text it is done with comes from.
///
/// A step is exactly what a whole instruction used to be, plus the one fact a
/// single instruction never needed: whether its argument is something the
/// request said, or something the step before it made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub instruction: Instruction,
    /// Whether this step's argument is the previous step's result.
    ///
    /// Set from the *operation's own declaration* ([`Offer::wants_content`]),
    /// never from anything a model said and never from the words of the
    /// request. An operation that collects a content hash can only be
    /// performed with content something produced, so there is exactly one
    /// place its argument can come from and no choice to be made about it.
    ///
    /// The value is not stored here, only the fact that it is carried: what
    /// step one actually produced is not known until step one has run, and
    /// filling it in earlier would be recording a guess. See
    /// [`crate::surface::Product`].
    pub carries: bool,
}

impl Step {
    /// A step that stands on its own.
    pub fn own(instruction: Instruction) -> Step {
        Step {
            instruction,
            carries: false,
        }
    }

    /// The step an already-validated instruction becomes, reading off the
    /// live surface whether its argument has to be carried.
    /// Read a clause again, against a room that has changed since the plan
    /// was made.
    ///
    /// A plan resolves every step up front, so a sequence that cannot be
    /// finished is never started. That holds for every step whose app was
    /// already in the room, and fails for one case: a step whose operation
    /// exists *because of* an earlier step. "Open the photo editor and crop
    /// this" names, in its second clause, an operation no survey could have
    /// held while the first clause had not run.
    ///
    /// So an unresolved clause keeps its own words, and the dispatcher asks
    /// again with the survey it retakes each step. Nothing is widened: the
    /// question is the same one, the offers are whatever the room exports
    /// now, and a clause that still answers to nothing comes back
    /// [`Instruction::Unknown`] with the refusal it would have given anyway.
    pub(crate) fn reconsider(clause: &str, offers: &[Offer]) -> Step {
        Step::bound(Instruction::from_floor(clause, offers), offers)
    }

    fn bound(instruction: Instruction, offers: &[Offer]) -> Step {
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

    /// Whether this step is a refusal rather than something to do.
    pub fn is_unanswered(&self) -> bool {
        matches!(self.instruction, Instruction::Unknown(_))
    }
}

/// An instruction, resolved: the ordered steps that carry it out.
///
/// Most requests are one step and always were. The reason this type exists is
/// the request that is not — "crop this square and put it under Sound check"
/// is one sentence, two operations and two applications, and an agent that
/// answered it with a single [`Instruction`] would carry out half of it and
/// report success. There is no honest way to say half a sentence back to
/// somebody, so the shape of an answer has to be able to hold all of it.
///
/// What a sequence is **not** is a widening of what may be asked for. Every
/// step is resolved against the same discovered surface a single instruction
/// is, and validated again at dispatch; a step whose address stops resolving
/// stops the sequence. The one thing a sequence adds is that a step's
/// argument may come from the step before it rather than from the request —
/// and even that is not a choice anybody makes, but a fact the operation
/// itself declares.
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
    /// A sequence carrying one of these must not be started. Working through
    /// the steps until the refusal is reached would leave the earlier ones
    /// applied to somebody's document with nothing asked for by them — and
    /// unlike a step that fails *at dispatch*, this is known before anything
    /// has happened, so there is no reason to find out the hard way.
    pub fn unanswered(&self) -> Option<&str> {
        self.steps.iter().find_map(|s| match &s.instruction {
            Instruction::Unknown(what) => Some(what.as_str()),
            _ => None,
        })
    }

    /// The deterministic floor over a request that may name several things.
    ///
    /// A request is read as a sequence only when it is written as one — the
    /// ordering words in [`CONJUNCTIONS`], outside any quotation — and only
    /// when reading it that way answers something. A single instruction with
    /// "and" inside it ("align left and right") resolves no clause on its own,
    /// and is handed to [`Instruction::from_floor`] whole, exactly as before.
    ///
    /// The case worth being exact about is the one in between: a request whose
    /// clauses *some* of which resolve. That is a sequence, and it is returned
    /// as one with the unresolved clause left as [`Instruction::Unknown`] —
    /// never collapsed back into a single instruction. Collapsing is what
    /// produces the failure this whole type exists to prevent: "crop the
    /// photograph and place it below Sound check" read whole matches the
    /// second half, which the agent would then do, and answer as though it had
    /// done both.
    pub fn from_floor(rest: &str, offers: &[Offer]) -> Plan {
        Plan::assembled(rest, |clause| {
            Step::bound(Instruction::from_floor(clause, offers), offers)
        })
    }

    /// The floor with the named compositions left out: only operations the
    /// room exports.
    ///
    /// For a request that arrived carrying its own subject. A composition is
    /// defined relative to the line that asks for it, and a message in a
    /// channel is not a line in a document — so a recipe keyword matching
    /// there is a word in the prose, not a request for the composition. That
    /// is how "align centre the Run sheet section" came out as a table.
    ///
    /// Nothing is widened: the candidates are whatever the caller scoped to
    /// the subject, and a part that still answers to nothing is a refusal.
    pub fn from_operations(rest: &str, offers: &[Offer]) -> Plan {
        Plan::assembled(rest, |clause| {
            let instruction = match surface::from_verbs(clause, offers) {
                Ok((offer, argument)) => Instruction::Perform {
                    id: offer.id(),
                    argument,
                },
                // The floor's own words for why, which name operations the
                // way a person would rather than by address.
                Err(why) => Instruction::Unknown(unmatched(clause, &why)),
            };
            Step::bound(instruction, offers)
        })
    }

    /// Split a request into its parts and resolve each with `step`, or read
    /// it whole when it turns out not to be several requests after all.
    fn assembled(rest: &str, step: impl Fn(&str) -> Step) -> Plan {
        let clauses = clauses(rest);
        if clauses.len() < 2 {
            return Plan { steps: vec![step(rest)] };
        }
        let steps: Vec<Step> = clauses.iter().map(|c| step(c)).collect();
        // Nothing in it was a request the room could answer, so the ordering
        // word was a word in one sentence rather than a join between two.
        if steps.iter().all(Step::is_unanswered) {
            return Plan { steps: vec![step(rest)] };
        }
        // Every part named the same operation, which is one request said
        // twice rather than two to carry out. "Centre it and align the
        // heading" is one alignment; performing it twice is harmless here and
        // would not be for a turn, a rename or an insert.
        if steps
            .windows(2)
            .all(|w| w[0].instruction == w[1].instruction)
        {
            return Plan {
                steps: vec![steps.into_iter().next().expect("at least two")],
            };
        }
        Plan { steps }
    }

    /// The full gate over an answered sequence: every id checked against the
    /// surface as it stands, and the whole answer refused if any is not one
    /// the room exports.
    ///
    /// The list is the only thing that grew. Each id passes exactly the check
    /// a single answer passes ([`Instruction::from_answer`]), so nothing an
    /// answer can say reaches dispatch without having been resolved against a
    /// freshly taken survey — a sequence is not a way in around that.
    ///
    /// Where the clause count matches the id count, each step is validated
    /// against its own clause, so a quoted literal is read where it was
    /// written rather than anywhere in the sentence.
    pub fn from_answer(
        ids: &[String],
        rest: &str,
        table: Option<TableDraft>,
        offers: &[Offer],
    ) -> Result<Plan, Refused> {
        if ids.is_empty() {
            return Err(Refused::Unusable("it named nothing to do".to_string()));
        }
        if ids.len() == 1 {
            return Ok(Plan::of(Instruction::from_answer(
                &ids[0], rest, table, offers,
            )?));
        }
        // A recipe rewrites the run of blocks around the line that asked for
        // it, reading the page to decide which. Which blocks those are is a
        // different question after another step has changed the page, and
        // answering it twice would be the agent deciding what to rewrite. So
        // a composition is a whole request or it is not this one.
        if ids.iter().any(|id| Recipe::from_id(id).is_some()) {
            return Err(Refused::Unusable(
                "an editorial change rewrites the text around the line that asks \
                 for it, so it cannot be one step of several"
                    .to_string(),
            ));
        }
        let parts = clauses(rest);
        let mut steps = Vec::new();
        for (i, id) in ids.iter().enumerate() {
            let said = parts.get(i).map(String::as_str).unwrap_or(rest);
            let said = if parts.len() == ids.len() { said } else { rest };
            let instruction = Instruction::from_answer(id, said, None, offers)?;
            steps.push(Step::bound(instruction, offers));
        }
        Ok(Plan { steps })
    }
}

/// The words that join one thing to do to the next.
///
/// Ordering words only, and longest first so that `and then` is one join
/// rather than `and` followed by a clause beginning `then`. Deliberately not
/// punctuation on its own: a comma separates the parts of a single request far
/// more often than it separates two.
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
/// exists: what is inside the quotation marks is a value somebody supplied,
/// and `rename it to "sound and vision.md"` asks for one thing.
pub fn clauses(rest: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut start = 0usize;
    let mut in_straight = false;
    let mut in_curly = false;
    let mut i = 0usize;
    while i < rest.len() {
        let tail = &rest[i..];
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
                tail.len() >= s.len() && tail.as_bytes()[..s.len()].eq_ignore_ascii_case(s.as_bytes())
            }) {
                out.push(rest[start..i].trim().to_string());
                i += sep.len();
                start = i;
                continue;
            }
        }
        i += ch.len_utf8();
    }
    out.push(rest[start..].trim().to_string());
    out.retain(|c| !c.is_empty());
    out
}

/// Why the floor could not answer, in the words the agent writes into the
/// document.
///
/// The apps' own labels and nothing else. An address is how the agent
/// dispatches; it is not something to say out loud to somebody reading the
/// page, and a refusal reciting six of them reads as a broken machine even
/// when the machine is working. The address goes in the log line beside this.
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
            format!("{rest} — {} needs the words to use, in quotes", label.to_lowercase())
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
    // The ordering rule is stated here rather than enforced in code, and
    // that is a decision rather than an omission. Nothing on this side
    // knows which column is the one to order by: the columns are the
    // model's own choice, every cell is free text, and the same recipe
    // drafts tables that are not schedules at all. A sort here could
    // therefore only guess — and guessing wrong would rearrange facts
    // behind the author's back, which is the one thing the last sentence
    // of this summary forbids.
    //
    // What went wrong was narrower: the rule used to say "the order they
    // actually belong in", which on a document that disagrees with
    // itself is not a defined order. Given notes reading `Doors: 18:00`
    // and `Sound check: 18:30` under prose saying doors open at seven,
    // the model ordered by the sequence it believed real -- sound check,
    // then doors -- while correctly leaving the recorded times alone. The
    // result was a Time column reading 18:30 above 18:00: defensible
    // reasoning, and a run sheet that reads backwards to everyone who
    // did not reconstruct it. So the rule now points at the table's own
    // column, and hands the contradiction to `note`, where it was
    // already going.
    Recipe {
        id: "runsheet",
        summary: "Draft the notes above the instruction into a table yourself, and \
                  supply it in `table`. Choose the columns — as few as the facts \
                  need, and none that would sit mostly empty. Order the rows by \
                  what the table itself will say: read down the column you are \
                  ordering by, and it has to run in order. Where the document \
                  contradicts itself so that no such order exists, keep the order \
                  the notes were written in and put the disagreement in `note` — \
                  a table whose own column runs backwards reads as a mistake \
                  whatever the reasoning behind it was. \
                  Fold in anything stated only in the prose around them. \
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
        let mut draft =
            table.ok_or_else(|| Refused::Unusable("it came with no table to write".to_string()))?;
        // Before the gate, so a draft that is empty *once the blank rows
        // are gone* is refused as having no rows rather than written as a
        // table of nothing.
        draft.drop_blank_rows();
        draft.check().map_err(Refused::Unusable)?;
        Ok(Instruction::RunSheet(draft))
    }
}

/// What a composition would be called in a sentence a person reads.
///
/// The recipes are the one thing the agent does that no app exports a label
/// for — they are compositions *of* exported actions — so the words have to be
/// written down here beside the recipes themselves. `None` for anything that
/// is not a composition, including an instruction that answers to nothing: a
/// request the agent could not read has no honest short name, and inventing
/// one would put a confident phrase in front of a person over an operation
/// that is not going to happen.
pub fn recipe_label(instruction: &Instruction) -> Option<&'static str> {
    match instruction {
        Instruction::Table => Some("turn the bullets into a table"),
        Instruction::Caption => Some("caption the image"),
        Instruction::Heading => Some("make a heading"),
        Instruction::RunSheet(_) => Some("write a run sheet"),
        Instruction::Perform { .. } | Instruction::Unknown(_) => None,
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

    /// An offer built from a real descriptor, so what the gate checks here is
    /// what it checks in a session.
    fn offer(app: &'static str, resource: &str, action: &str, label: &str, collects: Option<qos_actions::Collects>) -> Offer {
        Offer {
            app,
            descriptor: qos_actions::door::ActionDescriptor {
                address: qos_actions::door::ActionAddress::new(resource, action),
                resource_kind: format!("{app}.thing"),
                resource_label: resource.rsplit(':').next().unwrap_or(resource).to_string(),
                action_label: label.to_string(),
                description: None,
                locus: qos_actions::ExecutionLocus::ClientLocal,
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

    /// A photo editor and a word processor, both publishing something.
    fn two_apps() -> Vec<Offer> {
        vec![
            offer(
                "photos",
                "photos.image:/tmp/harbour.png",
                "crop",
                "Crop",
                Some(qos_actions::Collects::Words),
            ),
            offer(
                "word-processor",
                "word-processor.section:b7",
                "place-image",
                "Place image below",
                Some(qos_actions::Collects::Content),
            ),
            offer(
                "word-processor",
                "word-processor.section:b7",
                "align-center",
                "Align centre",
                None,
            ),
        ]
    }

    #[test]
    fn a_request_is_split_where_it_joins_two_things_and_nowhere_else() {
        assert_eq!(clauses("crop it and place image below"), vec!["crop it", "place image below"]);
        assert_eq!(clauses("crop it, then place it"), vec!["crop it", "place it"]);
        assert_eq!(clauses("crop it and then place it"), vec!["crop it", "place it"]);
        // One request that happens to contain the word.
        assert_eq!(clauses("align left and right"), vec!["align left", "right"]);
        // A value somebody supplied is never split, whichever quotes the
        // word processor produced.
        assert_eq!(
            clauses(r#"rename it to "sound and vision.md""#),
            vec![r#"rename it to "sound and vision.md""#]
        );
        assert_eq!(
            clauses("rename it to \u{201c}sound and vision.md\u{201d}"),
            vec!["rename it to \u{201c}sound and vision.md\u{201d}"]
        );
    }

    /// The headline: one sentence, two operations, two applications — and the
    /// second one's argument is not in the sentence at all.
    #[test]
    fn one_instruction_naming_two_apps_becomes_two_steps() {
        let offers = two_apps();
        let plan = Plan::from_floor(
            r#"crop the photograph to "0.25,0.25,0.5,0.5" and place image below b7"#,
            &offers,
        );
        assert_eq!(plan.len(), 2, "{plan:?}");
        assert_eq!(
            plan.steps[0].instruction,
            Instruction::Perform {
                id: "photos.image:/tmp/harbour.png#crop".into(),
                argument: Some("0.25,0.25,0.5,0.5".into()),
            }
        );
        assert!(!plan.steps[0].carries, "the first step has nothing to carry from");
        assert_eq!(
            plan.steps[1].instruction,
            Instruction::Perform {
                id: "word-processor.section:b7#place-image".into(),
                // Empty, and filled at dispatch from what the crop produced.
                // A value here would be one the request never contained.
                argument: None,
            }
        );
        assert!(
            plan.steps[1].carries,
            "an operation that collects content takes it from the step before it"
        );
    }

    /// The failure this whole type exists to prevent: read whole, that
    /// sentence matches the *second* operation and nothing would ever have
    /// cropped anything.
    #[test]
    fn reading_the_whole_sentence_would_have_done_only_half_of_it() {
        let offers = two_apps();
        let asked = r#"crop the photograph to "0.25,0.25,0.5,0.5" and place image below b7"#;
        let whole = Instruction::from_floor(asked, &offers);
        assert_eq!(
            whole,
            Instruction::Perform {
                id: "word-processor.section:b7#place-image".into(),
                argument: None,
            },
            "if this stops being the second half, the test below proves less"
        );
        let plan = Plan::from_floor(asked, &offers);
        assert_ne!(
            plan.steps.first().map(|s| &s.instruction),
            Some(&whole),
            "the sequence must not begin with the half a whole reading picks"
        );
    }

    /// A conjunction inside a request that names nothing is a word in one
    /// sentence, and the sentence is read whole — exactly as it was before
    /// sequences existed.
    #[test]
    fn a_conjunction_that_joins_nothing_leaves_the_request_whole() {
        let offers = two_apps();
        let plan = Plan::from_floor("tidy up and finish", &offers);
        assert_eq!(plan.len(), 1, "{plan:?}");
        assert_eq!(
            plan.steps[0].instruction,
            Instruction::Unknown("tidy up and finish".into()),
            "the refusal must name the whole request, not half of it"
        );
    }

    /// One request said twice is one request. Doing it twice is harmless for
    /// an alignment and is not for a turn, a rename or an insert.
    #[test]
    fn parts_that_name_the_same_operation_are_one_step() {
        let offers = two_apps();
        let plan = Plan::from_floor("align centre and centre it", &offers);
        assert_eq!(plan.len(), 1, "{plan:?}");
    }

    /// A part the room cannot answer is kept as the refusal it is, rather
    /// than dropped so the rest can proceed.
    #[test]
    fn a_part_that_answers_to_nothing_is_carried_as_a_refusal() {
        let offers = two_apps();
        let plan = Plan::from_floor("place image below b7 and juggle three plates", &offers);
        assert_eq!(plan.len(), 2, "{plan:?}");
        assert_eq!(
            plan.unanswered().map(str::to_string),
            Some("juggle three plates".to_string()),
            "{plan:?}"
        );
    }

    /// Every id in an answered sequence passes the same gate a single answer
    /// passes, and one that does not resolve refuses the whole answer — a
    /// list is not a way past the check.
    #[test]
    fn an_answered_sequence_is_gated_id_by_id() {
        let offers = two_apps();
        let good = vec![
            "photos.image:/tmp/harbour.png#crop".to_string(),
            "word-processor.section:b7#place-image".to_string(),
        ];
        let plan = Plan::from_answer(&good, r#"crop to "0,0,1,1" and place image"#, None, &offers)
            .expect("both ids are exported");
        assert_eq!(plan.len(), 2);
        assert!(plan.steps[1].carries);

        let bad = vec![
            "photos.image:/tmp/harbour.png#crop".to_string(),
            "word-processor.document#delete-everything".to_string(),
        ];
        assert_eq!(
            Plan::from_answer(
                &bad,
                r#"crop to "0,0,1,1" and delete everything"#,
                None,
                &offers
            ),
            Err(Refused::NotExported(
                "word-processor.document#delete-everything".to_string()
            )),
            "one id the room does not export refuses the whole answer"
        );
    }

    /// A composition is defined by the line that asks for it, so it is a
    /// whole request or it is not this one.
    #[test]
    fn a_composition_cannot_be_one_step_of_several() {
        let mut offers = two_apps();
        offers.push(offer(
            "word-processor",
            "word-processor.document",
            "insert-table",
            "Insert table",
            None,
        ));
        let answered = vec![
            "runsheet".to_string(),
            "word-processor.section:b7#align-center".to_string(),
        ];
        assert!(matches!(
            Plan::from_answer(&answered, "draft it and centre it", None, &offers),
            Err(Refused::Unusable(_))
        ));
    }

    /// An operation that collects a content hash is never refused for having
    /// quoted none: there is nothing anybody could quote.
    #[test]
    fn an_operation_that_collects_content_is_not_asked_for_words() {
        let offers = two_apps();
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

    /// A real page, and a real survey of it.
    ///
    /// Built by asking the word processor for the tree it publishes rather
    /// than by writing addresses down here, so every gate assertion below is
    /// against what the app actually exports. A withdrawn operation moves
    /// this fixture and the assertions together.
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

    /// The capability an inviter issues for one app.
    fn permitting(app: &str) -> ui::permissions::PermissionContext {
        use ui::permissions::{Actor, CapOverride, PermissionContext, Scope, Subject, Verb};
        PermissionContext::from_capabilities(
            Actor::agent("Scribe"),
            vec![CapOverride::grant(
                Subject::agent("Scribe"),
                Scope::App(app.to_string()),
                Verb::Write,
            )
            .with_id("test")],
        )
    }

    /// Nothing the agent writes into a document names an address.
    ///
    /// An address is how it dispatches: `word-processor.document#insert-table`
    /// is an internal identifier, and a refusal that recites six of them in
    /// parentheses reads as a broken machine to the person the document
    /// belongs to. The separator is the tell, so that is what is asserted —
    /// it catches every spelling of the mistake, not the one example somebody
    /// thought of.
    /// A clause the room could not answer, answered once the room changes.
    ///
    /// The case this exists for: "open the photo editor and crop this
    /// square". At the moment the plan is made, the photo editor is not in
    /// the room, so no survey contains a crop and the second clause resolves
    /// to nothing. Resolving the whole plan up front is otherwise right — a
    /// sequence that cannot be finished should not be started — but here it
    /// refuses a step for the state of a room that the step before it exists
    /// to change.
    ///
    /// Both halves are asserted, because only the pair is the claim. Against
    /// the room as it was, the clause genuinely does not resolve; against the
    /// room as it becomes, the same words resolve to the crop. A test that
    /// checked only the second would pass just as well if `reconsider` were
    /// resolving against something other than the offers it is handed.
    #[test]
    fn a_clause_resolves_once_the_app_that_exports_it_is_here() {
        let before = vec![offer(
            "word-processor",
            "document:1",
            "place-image",
            "Place image",
            Some(qos_actions::Collects::Content),
        )];
        let after = {
            let mut o = before.clone();
            o.push(offer("photos", "photo:1", "crop", "Crop", None));
            o
        };

        let clause = "crop the photograph";

        let unresolved = Step::reconsider(clause, &before);
        assert!(
            matches!(unresolved.instruction, Instruction::Unknown(_)),
            "with no photo editor in the room the clause must not resolve, \
             or the second half of this test proves nothing: got {:?}",
            unresolved.instruction,
        );

        let resolved = Step::reconsider(clause, &after);
        match resolved.instruction {
            Instruction::Perform { id, .. } => {
                let address = surface::decode_address(&id)
                    .expect("a resolved step must carry a decodable address");
                // The operation, not the app label: the claim is that these
                // words now reach the crop the photo editor brought with it,
                // and the word processor that was already here exports no
                // such thing. Asserting the action is what distinguishes
                // "resolved to the new app" from "resolved to anything at
                // all".
                assert_eq!(
                    address.action, "crop",
                    "the clause must resolve to the operation the arriving \
                     app exports, not to one that was already here",
                );
                assert!(
                    address.resource.starts_with("photo"),
                    "and on that app's own resource: {}",
                    address.resource,
                );
            }
            other => panic!(
                "once the photo editor exports a crop, the same words must \
                 resolve to it: got {other:?}"
            ),
        }
    }

    #[test]
    fn a_refusal_names_operations_in_words_and_never_an_address() {
        let offers = surveyed();
        assert!(!offers.is_empty(), "nothing to refuse against");

        let mut written = vec![
            unmatched("juggle three plates", &NoMatch::Nothing),
            unmatched("align it", &NoMatch::Ambiguous(vec![
                "Align left".into(),
                "Align centre".into(),
            ])),
            unmatched("rename it", &NoMatch::NeedsText("Rename".into())),
            surface::in_words(&offers, 3),
        ];
        // And whatever the floor itself produces against a real survey, so
        // this cannot pass on hand-written inputs alone.
        for asked in ["align it", "juggle three plates", "rename it"] {
            if let Instruction::Unknown(why) = Instruction::from_floor(asked, &offers) {
                written.push(why);
            }
        }

        for text in &written {
            assert!(
                !text.contains(surface::ADDRESS_SEP),
                "an address reached prose a person reads: {text:?}"
            );
            for offer in &offers {
                assert!(
                    !text.contains(offer.address().resource.as_str()),
                    "a resource id reached prose a person reads: {text:?}"
                );
            }
        }

        // It still says something: the alternatives are named, in the app's
        // own words, so the refusal is useful rather than merely clean.
        let ambiguous = unmatched(
            "align it",
            &NoMatch::Ambiguous(vec!["Align left".into(), "Align centre".into()]),
        );
        assert!(ambiguous.contains("align left"), "{ambiguous}");
        assert!(ambiguous.contains("align centre"), "{ambiguous}");
    }

    #[test]
    fn every_recipe_only_dispatches_actions_the_app_exports() {
        // A recipe is a composition, so its parts are named here — but they
        // are not taken on trust: each one must be what the word processor's
        // own encoder calls the action its own builder produced. Rename or
        // withdraw an action and this fails rather than the agent offering
        // something imaginary.
        let exported = exported_actions();
        assert!(!exported.is_empty(), "the app exports nothing");
        for recipe in RECIPES {
            let built: BTreeSet<String> = dispatched(recipe.id)
                .iter()
                .filter_map(exported_name)
                .collect();
            assert!(
                !built.is_empty(),
                "{} dispatches nothing the app can encode",
                recipe.id
            );
            let advertised: BTreeSet<String> =
                recipe.actions.iter().map(|a| a.to_string()).collect();
            assert_eq!(
                advertised, built,
                "{} advertises {:?} but dispatches {:?}",
                recipe.id, advertised, built
            );
            for name in &advertised {
                assert!(
                    exported.contains(name),
                    "{} offers {name}, which the app does not export",
                    recipe.id
                );
            }
        }
    }

    /// The gate answers an address the room really exports, and refuses one
    /// it does not — including an address that was valid against some other
    /// survey. Discovery is only worth anything if the check is against the
    /// surface as it stands.
    #[test]
    fn an_answer_is_checked_against_the_survey_and_not_against_a_list() {
        let offers = surveyed();
        assert!(!offers.is_empty(), "the survey found nothing to check against");

        // Everything the survey turned up is answerable, by its own id.
        for offer in &offers {
            let answered = Instruction::from_answer(&offer.id(), "asked", None, &offers);
            if offer.wants_content() {
                // It collects a content hash, which no instruction contains.
                // Refusing it for having quoted none would refuse it always;
                // what it gets instead is nothing here and a binding at
                // dispatch, so the gate must let it through with no argument.
                assert_eq!(
                    answered,
                    Ok(Instruction::Perform {
                        id: offer.id(),
                        argument: None
                    }),
                    "{} collects content, not words; it must not be asked to quote any",
                    offer.id()
                );
                continue;
            }
            if offer.wants_text() {
                // It collects text and the instruction quoted none. Refused
                // as unusable rather than performed with a blank.
                assert!(
                    matches!(answered, Err(Refused::Unusable(_))),
                    "{} collects text; without any it must be unusable, got {answered:?}",
                    offer.id()
                );
                continue;
            }
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

        // An address of the right shape that nothing in this room carries.
        let stale = "word-processor.section:gone-block#align-left";
        assert_eq!(
            Instruction::from_answer(stale, "asked", None, &offers),
            Err(Refused::NotExported(stale.to_string())),
            "an address the survey does not carry must not be dispatched"
        );
        // And an answer that is not an address at all.
        assert_eq!(
            Instruction::from_id("wander off", "x", &offers),
            None
        );
        assert_eq!(Instruction::from_id("InsertTable", "x", &offers), None);
        assert_eq!(
            Instruction::from_id(NOTHING_FITS, "juggle", &offers),
            Some(Instruction::Unknown("juggle".into()))
        );
    }

    /// A recipe is only offered while the app it edits is in the room.
    #[test]
    fn a_recipe_is_refused_when_its_app_is_not_here() {
        let offers = surveyed();
        assert_eq!(
            Instruction::from_answer("table", "asked", None, &offers),
            Ok(Instruction::Table)
        );
        // Same answer, empty room: there is no document for it to act on, so
        // it is not exported rather than attempted against nothing.
        assert_eq!(
            Instruction::from_answer("table", "asked", None, &[]),
            Err(Refused::NotExported("table".into()))
        );
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
    fn every_recipe_id_maps_to_an_instruction_and_nothing_else_does() {
        // Every recipe offered must be actionable — on its own, or once it
        // is given the content it asks for. What none of them may be is
        // `NotExported`, which would mean the agent offers something this
        // gate would then refuse.
        let offers = surveyed();
        for recipe in RECIPES {
            match Instruction::from_answer(recipe.id, "asked", None, &offers) {
                Ok(_) => {}
                Err(Refused::Unusable(_)) => {
                    // It wants content. With content it must work.
                    assert!(
                        Instruction::from_answer(
                            recipe.id,
                            "asked",
                            Some(sample_draft()),
                            &offers
                        )
                        .is_ok(),
                        "{} is offered but is unusable even with a well-formed table",
                        recipe.id
                    );
                }
                Err(Refused::NotExported(_)) => {
                    panic!("{} is offered but maps to no instruction", recipe.id)
                }
            }
        }
    }

    /// A row with nothing in it does not reach the page.
    ///
    /// Drafted by a live model in a run of the demo's own beat: the two
    /// real events, and then `["", ""]`. It passes every bound `check`
    /// applies -- it is exactly as wide as the header, and the table as a
    /// whole is not blank -- so nothing refused it and the page got a band
    /// of ruled whitespace through the middle of the payoff frame.
    ///
    /// Asserted through `from_answer`, which is the gate every drafted
    /// table actually passes through, rather than through the normaliser
    /// on its own.
    #[test]
    fn a_blank_row_is_dropped_rather_than_written() {
        let offers = surveyed();
        let drafted = TableDraft {
            headers: vec!["Time".into(), "Item".into()],
            rows: vec![
                vec!["18:00".into(), "Doors".into()],
                vec!["".into(), "".into()],
                vec!["18:30".into(), "Sound check".into()],
                vec!["  ".into(), "\t".into()],
            ],
            note: None,
        };
        // It is well-formed by every bound the gate applies, which is why
        // this reached a document in the first place.
        assert_eq!(drafted.check(), Ok(()));

        let Ok(Instruction::RunSheet(written)) =
            Instruction::from_answer("runsheet", "asked", Some(drafted), &offers)
        else {
            panic!("a well-formed draft must still be accepted");
        };
        assert_eq!(
            written.rows,
            vec![
                vec!["18:00".to_string(), "Doors".to_string()],
                vec!["18:30".to_string(), "Sound check".to_string()],
            ],
            "the blank rows survived into the table"
        );
        // Whitespace is not content: a row of spaces and tabs reads as an
        // empty band on the page exactly as a row of empty strings does.
        assert!(
            !written.grid().iter().any(|r| r.iter().all(|c| c.trim().is_empty())),
            "a row of whitespace is still a blank row: {:?}",
            written.grid()
        );
    }

    /// And a draft that is *only* blank rows is refused, not written as a
    /// table with a header and nothing under it.
    #[test]
    fn a_draft_with_nothing_but_blank_rows_is_refused() {
        let offers = surveyed();
        let empty = TableDraft {
            headers: vec!["Time".into(), "Item".into()],
            rows: vec![vec!["".into(), "".into()], vec![" ".into(), " ".into()]],
            note: None,
        };
        match Instruction::from_answer("runsheet", "asked", Some(empty), &offers) {
            Err(Refused::Unusable(why)) => assert!(
                why.contains("no rows"),
                "refused for the wrong reason: {why}"
            ),
            other => panic!("a table of nothing must not be written: {other:?}"),
        }
    }

    #[test]
    fn a_drafted_table_is_checked_before_any_of_it_is_written() {
        // The good case: exactly as wide as its header, and it becomes an
        // instruction carrying the content.
        let ok = sample_draft();
        let offers = surveyed();
        assert_eq!(ok.check(), Ok(()));
        assert_eq!(
            Instruction::from_answer("runsheet", "asked", Some(ok.clone()), &offers),
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
                Instruction::from_answer("runsheet", "asked", Some(d), &surveyed()),
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
            Instruction::from_answer("runsheet", "asked", None, &offers),
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
        // The recipes keep their keywords, because they are the one part
        // of the vocabulary that is still a closed set. Asking for a run
        // sheet in those words used to land in `Unknown` — so the agent's
        // reply to the only instruction it was ever given was that it did
        // not understand it. Spelt both ways, because people write it both
        // ways.
        let offers = surveyed();
        assert_eq!(
            Instruction::parse("Scribe: make this a run sheet", "Scribe", &offers),
            Some(Instruction::Table)
        );
        assert_eq!(
            Instruction::parse("Scribe: build a runsheet from these", "Scribe", &offers),
            Some(Instruction::Table)
        );
        // And the floor still has one: an instruction nothing in the room
        // answers is reported as such rather than quietly turned into the
        // nearest thing it could do.
        assert!(matches!(
            Instruction::parse("Scribe: book the venue", "Scribe", &offers),
            Some(Instruction::Unknown(_))
        ));
    }

    /// The floor with an open surface: an instruction whose words land on
    /// exactly one exported operation resolves to that address, and one that
    /// lands on several is refused with them named.
    #[test]
    fn the_floor_needs_a_clear_winner_and_says_so_when_there_is_none() {
        let offers = surveyed();
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

        // "align" is the word four of the document's operations carry.
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

        // Nothing in the room carries any of these words.
        assert!(matches!(
            Instruction::from_floor("juggle three plates", &offers),
            Instruction::Unknown(_)
        ));
    }

    #[test]
    fn instructions_are_addressed_by_name_case_insensitively() {
        let offers = surveyed();
        assert_eq!(
            Instruction::parse("scribe: turn the bullets into a table", "Scribe", &offers),
            Some(Instruction::Table)
        );
        assert_eq!(
            Instruction::parse("Scribe: caption the image please", "Scribe", &offers),
            Some(Instruction::Caption)
        );
        assert_eq!(
            Instruction::parse("SCRIBE: give this a title", "Scribe", &offers),
            Some(Instruction::Heading)
        );
        assert!(matches!(
            Instruction::parse("Scribe: juggle", "Scribe", &offers),
            Some(Instruction::Unknown(_))
        ));
        assert_eq!(Instruction::parse("Dear Scribe: table", "Scribe", &offers), None);
        assert_eq!(Instruction::parse("Scribe: table", "Ada", &offers), None);
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
        let (at, ins) = find_instruction(&blocks, "Scribe", &surveyed()).unwrap();
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
