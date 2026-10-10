//! Reading instructions out of the document, and the pure parts of
//! carrying them out.
//!
//! An instruction is a paragraph addressed to the agent by name —
//! `Claude: turn the bullets into a table`. What it may ask for is exactly
//! what the room's apps currently publish, through
//! [`Instruction::Perform`]: surveyed through [`crate::surface`] and checked
//! against it again at dispatch, never against a remembered list. There are
//! no agent-only recipes: a table, a caption or a heading is whatever the
//! applications' own published operations do, the same ones a person reaches
//! from the palette.

use std::collections::BTreeMap;

use tilo_doc::snapshot::{AttrVal, BlockView, DocSnapshot};

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
        /// Canonical JSON argument object, keyed exactly as the live
        /// descriptor declares. Empty for a nullary operation.
        arguments: serde_json::Map<String, serde_json::Value>,
        /// Contract confirmation was explicitly and correctly supplied in
        /// the plan metadata. Never inferred from prose or risk level.
        confirmation: ConfirmationEvidence,
    },
    /// Nothing published answers it; the agent says so.
    Unknown(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ConfirmationEvidence {
    #[default]
    None,
    Confirm,
    Phrase(String),
}

impl ConfirmationEvidence {
    pub fn satisfies(&self, policy: &tilo_actions::ConfirmationPolicy) -> bool {
        matches!(
            (self, policy),
            (Self::None, tilo_actions::ConfirmationPolicy::None)
                | (Self::Confirm, tilo_actions::ConfirmationPolicy::Confirm)
        ) || matches!(
            (self, policy),
            (
                Self::Phrase(supplied),
                tilo_actions::ConfirmationPolicy::Phrase { phrase }
            ) if supplied == phrase
        )
    }
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
    ///
    /// The name is matched without regard to case; what follows it is kept
    /// as written, so a quoted value reaches the document as it was typed.
    pub fn addressed(text: &str, name: &str) -> Option<String> {
        let t = text.trim();
        let prefix = format!("{}:", name.to_lowercase());
        let length = prefix.chars().count();
        let head: String = t.chars().take(length).collect();
        if head.to_lowercase() != prefix {
            return None;
        }
        let rest: String = t.chars().skip(length).collect();
        Some(rest.trim().to_string())
    }

    /// The deterministic floor over the room's own surface: whatever the
    /// room's apps currently export, matched by [`surface::from_verbs`].
    ///
    /// The floor never guesses between two operations that fit equally
    /// well; it says so, in [`NoMatch::Ambiguous`], and leaves the choice
    /// to a model or to whoever reads the refusal.
    pub fn from_floor(rest: &str, offers: &[Offer]) -> Instruction {
        match surface::from_verbs(rest, offers) {
            Ok((offer, arguments)) => {
                Instruction::from_answer(&offer.id(), rest, Some(&arguments), offers)
                    .unwrap_or_else(|why| Instruction::Unknown(format!("{why:?}")))
            }
            Err(why) => Instruction::Unknown(unmatched(rest, &why)),
        }
    }

    /// The full gate over an answered id: checked against what the room
    /// exports **now**, not against what it exported when the id was
    /// offered.
    ///
    /// [`NOTHING_FITS`] is a legitimate answer, not a failure. Anything else
    /// must name a live [`Offer`]: an operation that
    /// collects a content hash is never asked to quote one — there is
    /// nowhere honest to get one from an answer — and an operation that
    /// collects text is [`Refused::Unusable`] when neither `supplied` nor a
    /// quoted literal in `rest` gives it any, rather than silently falling
    /// back to the deterministic floor while reporting success.
    ///
    /// # Errors
    ///
    /// [`Refused::NotExported`] when `id` names no live [`Offer`]. [`Refused::Unusable`] when `id` names an offer
    /// that collects text but neither `supplied` nor a quoted literal in
    /// `rest` gives it any.
    pub fn from_answer(
        id: &str,
        rest: &str,
        supplied: Option<&serde_json::Map<String, serde_json::Value>>,
        offers: &[Offer],
    ) -> Result<Instruction, Refused> {
        if id == NOTHING_FITS {
            return Ok(Instruction::Unknown(rest.to_string()));
        }
        let Some(offer) = surface::find(offers, id) else {
            return Err(Refused::NotExported(id.to_string()));
        };
        let mut arguments = if offer.wants_content() {
            supplied.cloned().unwrap_or_default()
        } else if offer.wants_arguments() {
            supplied
                .cloned()
                .or_else(|| offer.arguments_from_literal(rest))
                .ok_or_else(|| {
                    Refused::Unusable(format!(
                        "{} requires structured arguments and neither the answer nor the \
                     instruction supplied a valid object",
                        offer.descriptor.action_label
                    ))
                })?
        } else {
            supplied.cloned().unwrap_or_default()
        };
        let confirmation = match &offer.descriptor.contract.confirmation {
            tilo_actions::ConfirmationPolicy::None => ConfirmationEvidence::None,
            tilo_actions::ConfirmationPolicy::Confirm => {
                if arguments.remove("confirm") != Some(serde_json::Value::Bool(true)) {
                    return Err(Refused::Unusable(format!(
                        "{} requires `confirm: true`",
                        offer.descriptor.action_label
                    )));
                }
                ConfirmationEvidence::Confirm
            }
            tilo_actions::ConfirmationPolicy::Phrase { phrase } => {
                if arguments
                    .remove("confirmation_phrase")
                    .and_then(|v| v.as_str().map(str::to_owned))
                    != Some(phrase.clone())
                {
                    return Err(Refused::Unusable(format!(
                        "{} requires the exact confirmation phrase",
                        offer.descriptor.action_label
                    )));
                }
                ConfirmationEvidence::Phrase(phrase.clone())
            }
        };
        // Confirmation is invocation metadata, never an app argument. Strip
        // both reserved names even for actions that need no confirmation so
        // they cannot collide with or sneak through strict schema binding.
        arguments.remove("confirm");
        arguments.remove("confirmation_phrase");
        if !offer.wants_content() {
            tilo_actions::ActionArgs::bind_json(&offer.descriptor.params, &arguments)
                .map_err(|error| Refused::Unusable(error.to_string()))?;
        }
        Ok(Instruction::Perform {
            id: offer.id(),
            arguments,
            confirmation,
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
            Instruction::Unknown(_) => false,
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
            Instruction::Unknown(_) => false,
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

    /// Deterministic fallback over only the live resource/action surface.
    /// Unlike the legacy recipe floor, this cannot select an operation that
    /// was compiled into the agent but not published by an application.
    pub fn from_surface(rest: &str, offers: &[Offer]) -> Plan {
        let steps = clauses(rest)
            .into_iter()
            .map(|clause| {
                let instruction = match surface::from_verbs(&clause, offers) {
                    Ok((offer, arguments)) => {
                        Instruction::from_answer(&offer.id(), &clause, Some(&arguments), offers)
                            .unwrap_or_else(|why| Instruction::Unknown(format!("{why:?}")))
                    }
                    Err(why) => Instruction::Unknown(unmatched(&clause, &why)),
                };
                Step::bound(instruction, offers)
            })
            .collect();
        Plan { steps }
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
// Reading the document.
// ---------------------------------------------------------------------------

/// The answer that means "no exported action does what was asked".
pub const NOTHING_FITS: &str = "none";

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

    #[test]
    fn instructions_come_only_from_text_addressed_to_the_agent_by_name() {
        // Case `who_was_addressed_is_read_from_the_document_not_decided`.
        {
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

        // Case `instructions_are_addressed_by_name_case_insensitively`.
        {
            assert_eq!(
                Instruction::addressed("scribe: turn the bullets into a table", "Scribe"),
                Some("turn the bullets into a table".to_string())
            );
            assert_eq!(
                Instruction::addressed("SCRIBE: give this a title", "Scribe"),
                Some("give this a title".to_string())
            );
            assert_eq!(
                Instruction::addressed("SCRIBE: add a paragraph \"Next Steps\"", "Scribe"),
                Some("add a paragraph \"Next Steps\"".to_string()),
                "what is quoted keeps its case"
            );
            let blocks = vec![
                para("h", "Shopping"),
                para("ins", "Scribe: table"),
                para("later", "Scribe: not this one"),
            ];
            assert_eq!(
                find_addressed(&blocks, "Scribe"),
                Some((1, "table".to_string()))
            );
            assert_eq!(find_addressed(&blocks, "Ada"), None);
        }
    }

    // -- Instruction::Perform, and the floor that reaches it -------------

    /// An offer built from a real descriptor, so what these tests check is
    /// what a gate checks against a real survey.
    fn offer(
        app: &'static str,
        resource: &str,
        action: &str,
        label: &str,
        collects: Option<tilo_actions::Collects>,
    ) -> Offer {
        let params = if collects.is_some() {
            vec![tilo_actions::ActionParam::required(
                "value",
                tilo_actions::ParamType::Text,
                "value",
            )]
        } else {
            Vec::new()
        };
        Offer {
            app: app.to_string(),
            broker_peer_id: None,
            descriptor: tilo_actions::door::ActionDescriptor {
                semantic_identity: None,
                address: tilo_actions::door::ActionAddress::new(resource, action),
                resource_kind: format!("{app}.thing"),
                resource_label: resource.rsplit(':').next().unwrap_or(resource).to_string(),
                action_label: label.to_string(),
                description: None,
                locus: tilo_actions::ExecutionLocus::ClientLocal,
                kind: tilo_actions::OperationKind::Document,
                contract: tilo_actions::ActionContract::default(),
                params,
                input: match collects {
                    None => tilo_actions::InputSpec::None,
                    Some(c) => tilo_actions::InputSpec::Text {
                        prompt: "p".into(),
                        placeholder: String::new(),
                        collects: c,
                    },
                },
                view: None,
            },
        }
    }

    /// A real word-processor document with one heading, surveyed through
    /// the real contract, so every assertion below is against what the app
    /// actually exports.
    fn surveyed() -> Vec<Offer> {
        let mut doc = automerge::AutoCommit::new();
        let c = word_processor::model::init(&mut doc).expect("a Writer body");
        let mut ids = tilo_doc::model::seeded_id_gen(&doc, &c);
        {
            let mut dm = tilo_doc::DocMut {
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
        // What the app's own view publishes over it: its verbs and the
        // edits at a caret.
        let published = word_processor::actions::published_for(
            &word_processor::actions::WpResourceCtx {
                caret_block: String::new(),
                caret_offset: 0,
                target_blocks: Vec::new(),
                format_spans: Vec::new(),
                selection: None,
                region: word_processor::actions::RegionKind::Body,
                search_hits: Vec::new(),
                content: tilo_content::ContentStore::memory(),
            },
            &doc,
        );
        surface::offers_in(wp, &published, &permitting("so.tilo.word-processor"))
    }

    fn permitting(app: &str) -> ui::permissions::PermissionContext {
        ui::permissions::PermissionContext::agent_granted_at(
            "Scribe",
            &[ui::permissions::TreePath::root().app(app.to_string())],
            ui::permissions::KindSet::READ_UNDOABLE,
        )
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each case keeps its original fixture and assertions"
    )]
    fn an_instruction_resolves_only_to_a_surveyed_bound_and_confirmed_operation() {
        // Case `a_refusal_names_operations_in_words_and_never_an_address`.
        {
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

        // Case `the_floor_needs_a_clear_winner_and_says_so_when_there_is_none`.
        {
            let offers = surveyed();

            // "centre" lands on exactly one offer, the paragraph alignment,
            // with the alignment the instruction named.
            let one = Instruction::from_floor("centre the Notes", &offers);
            let Instruction::Perform {
                id,
                arguments,
                confirmation,
            } = &one
            else {
                panic!("a single clear match must resolve to an address, got {one:?}");
            };
            assert_eq!(arguments.get("align"), Some(&serde_json::json!("center")));
            assert_eq!(confirmation, &ConfirmationEvidence::None);
            assert!(
                surface::find(&offers, id).is_some_and(|o| o
                    .descriptor
                    .action_label
                    .to_lowercase()
                    .contains("align")),
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

            // There is no agent-only vocabulary: "table" means whatever the app
            // publishes for it — here its table of contents and its table at
            // the caret, between which the floor does not guess — never a
            // compiled-in recipe.
            let table = Instruction::from_floor("turn the headings into a table", &offers);
            let Instruction::Unknown(why) = &table else {
                panic!("two of the app's own operations fit; got {table:?}");
            };
            assert!(why.contains("generate table of contents"), "{why}");
            assert!(why.contains("insert table here"), "{why}");
        }

        // Case `an_answer_is_checked_against_the_survey_and_not_against_a_list`.
        {
            let offers = surveyed();
            assert!(
                !offers.is_empty(),
                "the survey found nothing to check against"
            );

            // An operation with a required parameter is dispatchable only with
            // that parameter (checked by the structured-argument tests below);
            // here every operation that needs nothing must pass with nothing.
            let nullary: Vec<&Offer> = offers
                .iter()
                .filter(|offer| offer.descriptor.params.iter().all(|param| !param.required))
                .collect();
            assert!(!nullary.is_empty(), "the survey offers nothing nullary");
            for offer in nullary {
                let supplied = match &offer.descriptor.contract.confirmation {
                    tilo_actions::ConfirmationPolicy::None => serde_json::Map::new(),
                    tilo_actions::ConfirmationPolicy::Confirm => serde_json::Map::from_iter([(
                        "confirm".into(),
                        serde_json::Value::Bool(true),
                    )]),
                    tilo_actions::ConfirmationPolicy::Phrase { phrase } => {
                        serde_json::Map::from_iter([(
                            "confirmation_phrase".into(),
                            serde_json::Value::String(phrase.clone()),
                        )])
                    }
                };
                let confirmation = match &offer.descriptor.contract.confirmation {
                    tilo_actions::ConfirmationPolicy::None => ConfirmationEvidence::None,
                    tilo_actions::ConfirmationPolicy::Confirm => ConfirmationEvidence::Confirm,
                    tilo_actions::ConfirmationPolicy::Phrase { phrase } => {
                        ConfirmationEvidence::Phrase(phrase.clone())
                    }
                };
                let answered =
                    Instruction::from_answer(&offer.id(), "asked", Some(&supplied), &offers);
                assert_eq!(
                    answered,
                    Ok(Instruction::Perform {
                        id: offer.id(),
                        arguments: serde_json::Map::new(),
                        confirmation,
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
                Err(Refused::NotExported("table".into())),
                "a former agent recipe id is not an operation anybody publishes"
            );
        }

        // Case `an_operation_that_collects_content_is_not_asked_for_words`.
        {
            let offers = vec![offer(
                "word-processor",
                "word-processor.section:b7",
                "place-image",
                "Place image below",
                Some(tilo_actions::Collects::Content),
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
                    arguments: serde_json::Map::new(),
                    confirmation: ConfirmationEvidence::None,
                })
            );
        }

        // Case `text_collecting_operations_need_a_literal_or_are_refused`.
        {
            let offers = vec![offer(
                "files",
                "files.entry:/a",
                "rename",
                "Rename",
                Some(tilo_actions::Collects::Words),
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
                    arguments: serde_json::Map::from_iter([(
                        "value".into(),
                        serde_json::Value::String("b.md".into())
                    )]),
                    confirmation: ConfirmationEvidence::None,
                }),
                "a literal quoted in the instruction is used"
            );
            assert_eq!(
                Instruction::from_answer(
                    "files.entry:/a#rename",
                    "rename it",
                    Some(&serde_json::Map::from_iter([(
                        "value".into(),
                        serde_json::Value::String("c.md".into())
                    )])),
                    &offers,
                ),
                Ok(Instruction::Perform {
                    id: "files.entry:/a#rename".into(),
                    arguments: serde_json::Map::from_iter([(
                        "value".into(),
                        serde_json::Value::String("c.md".into())
                    )]),
                    confirmation: ConfirmationEvidence::None,
                }),
                "a literal the answer itself supplied is used even with no quote in the instruction"
            );
        }

        // Case `structured_answers_preserve_types_and_are_bound_before_dispatch`.
        {
            let mut rich = offer("sheets", "sheets.range:r1", "fill", "Fill range", None);
            rich.descriptor.params = vec![
                tilo_actions::ActionParam::required(
                    "count",
                    tilo_actions::ParamType::Integer,
                    "Rows to fill",
                ),
                tilo_actions::ActionParam::required(
                    "enabled",
                    tilo_actions::ParamType::Boolean,
                    "Whether formulas are enabled",
                ),
            ];
            let offers = vec![rich];
            let valid = serde_json::Map::from_iter([
                ("count".into(), serde_json::json!(3)),
                ("enabled".into(), serde_json::json!(true)),
            ]);
            let instruction =
                Instruction::from_answer("sheets.range:r1#fill", "fill it", Some(&valid), &offers)
                    .expect("the descriptor accepts its typed object");
            assert_eq!(
                instruction,
                Instruction::Perform {
                    id: "sheets.range:r1#fill".into(),
                    arguments: valid,
                    confirmation: ConfirmationEvidence::None,
                }
            );

            let wrong = serde_json::Map::from_iter([
                ("count".into(), serde_json::json!("3")),
                ("enabled".into(), serde_json::json!(true)),
            ]);
            assert!(matches!(
                Instruction::from_answer("sheets.range:r1#fill", "fill it", Some(&wrong), &offers,),
                Err(Refused::Unusable(_))
            ));
        }

        // Case `consequential_actions_require_and_strip_explicit_confirmation_metadata`.
        {
            let mut destructive = offer(
                "files",
                "files.entry:/important",
                "delete",
                "Delete file",
                None,
            );
            destructive.descriptor.contract.confirmation =
                tilo_actions::ConfirmationPolicy::Confirm;
            destructive.descriptor.params = vec![tilo_actions::ActionParam::required(
                "recursive",
                tilo_actions::ParamType::Boolean,
                "Delete descendants",
            )];
            let offers = vec![destructive];

            for arguments in [
                serde_json::json!({"recursive": true}),
                serde_json::json!({"recursive": true, "confirm": false}),
                serde_json::json!({"recursive": true, "confirm": "true"}),
            ] {
                assert!(matches!(
                    Instruction::from_answer(
                        "files.entry:/important#delete",
                        "delete it",
                        arguments.as_object(),
                        &offers,
                    ),
                    Err(Refused::Unusable(_))
                ));
            }

            let confirmed = serde_json::json!({"recursive": true, "confirm": true});
            assert_eq!(
                Instruction::from_answer(
                    "files.entry:/important#delete",
                    "delete it",
                    confirmed.as_object(),
                    &offers,
                ),
                Ok(Instruction::Perform {
                    id: "files.entry:/important#delete".into(),
                    arguments: serde_json::Map::from_iter([(
                        "recursive".into(),
                        serde_json::Value::Bool(true),
                    )]),
                    confirmation: ConfirmationEvidence::Confirm,
                })
            );
        }

        // Case `phrase_confirmation_must_match_exactly_before_an_instruction_can_dispatch`.
        {
            let phrase = "delete all archived messages";
            let mut destructive =
                offer("chat", "chat.archive:2025", "purge", "Purge archive", None);
            destructive.descriptor.contract.confirmation =
                tilo_actions::ConfirmationPolicy::Phrase {
                    phrase: phrase.into(),
                };
            let offers = vec![destructive];

            for supplied in [
                serde_json::json!({}),
                serde_json::json!({"confirmation_phrase": "Delete all archived messages"}),
                serde_json::json!({"confirmation_phrase": "delete all archived messages "}),
            ] {
                assert!(matches!(
                    Instruction::from_answer(
                        "chat.archive:2025#purge",
                        "purge it",
                        supplied.as_object(),
                        &offers,
                    ),
                    Err(Refused::Unusable(_))
                ));
            }

            let exact = serde_json::json!({"confirmation_phrase": phrase});
            assert_eq!(
                Instruction::from_answer(
                    "chat.archive:2025#purge",
                    "purge it",
                    exact.as_object(),
                    &offers,
                ),
                Ok(Instruction::Perform {
                    id: "chat.archive:2025#purge".into(),
                    arguments: serde_json::Map::new(),
                    confirmation: ConfirmationEvidence::Phrase(phrase.into()),
                })
            );
        }
    }

    // -- Sequences: Plan, Step, clauses -----------------------------------

    #[test]
    fn a_request_is_split_into_a_sequence_only_at_joins_whose_clauses_resolve() {
        // Case `clauses_split_on_ordering_words_outside_quotes`.
        {
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

        // Case `a_plan_reads_a_request_written_as_several_things_as_a_sequence`.
        {
            let offers = two_apps();
            let plan = Plan::from_floor("archive the file, then rotate the picture right", &offers);
            assert!(plan.is_sequence(), "{plan:?}");
            assert_eq!(plan.len(), 2);
            assert!(
                matches!(&plan.steps[0].instruction, Instruction::Perform { id, .. } if id.ends_with("#archive"))
            );
            assert!(
                matches!(&plan.steps[1].instruction, Instruction::Perform { id, .. } if id.ends_with("#rotate-right"))
            );
            assert_eq!(plan.unanswered(), None);
        }

        // Case `a_single_request_with_a_join_inside_it_is_not_split`.
        {
            // "align left and right" resolves no clause on its own — the floor
            // reads it whole, exactly as a request with no join in it at all.
            let plan = Plan::from_floor("align left and right", &[]);
            assert!(!plan.is_sequence(), "{plan:?}");
            assert_eq!(plan.len(), 1);
        }

        // Case `a_plan_collapses_a_request_that_only_repeats_one_operation`.
        {
            let offers = two_apps();
            let plan = Plan::from_floor("archive the file and archive the file again", &offers);
            assert!(!plan.is_sequence(), "{plan:?}");
            assert!(
                matches!(&plan.steps[0].instruction, Instruction::Perform { id, .. } if id.ends_with("#archive"))
            );
        }

        // Case `a_plan_falls_back_to_the_whole_request_when_nothing_in_it_resolves`.
        {
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
    }

    /// Two published operations a sequence can name, in the apps' own words.
    fn two_apps() -> Vec<Offer> {
        vec![
            offer("files", "files.entry:/a", "archive", "Archive file", None),
            offer(
                "photos",
                "photos.picture:1",
                "rotate-right",
                "Rotate picture right",
                None,
            ),
        ]
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each case keeps its original fixture and assertions"
    )]
    fn steps_read_carrying_and_opening_off_declarations_and_only_an_earlier_open_excuses_a_gap() {
        // Case `carries_is_read_off_the_operations_own_declaration_never_guessed`.
        {
            let content = offer(
                "photos",
                "photos.picture:1",
                "crop",
                "Crop",
                Some(tilo_actions::Collects::Content),
            );
            let words = offer(
                "files",
                "files.entry:/a",
                "rename",
                "Rename",
                Some(tilo_actions::Collects::Words),
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
                arguments: serde_json::Map::new(),
                confirmation: ConfirmationEvidence::None,
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
            assert!(!Step::bound(Instruction::Unknown("x".into()), &offers).carries);
        }

        // Case `opens_an_application_is_true_only_for_the_open_address`.
        {
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
                    arguments: serde_json::Map::new(),
                    confirmation: ConfirmationEvidence::None,
                })
                .opens_an_application()
            );
            assert!(
                !Step::own(Instruction::Perform {
                    id: ordinary.id(),
                    arguments: serde_json::Map::new(),
                    confirmation: ConfirmationEvidence::None,
                })
                .opens_an_application()
            );
            assert!(!Step::own(Instruction::Unknown("x".into())).opens_an_application());
        }

        // Case `unanswered_refuses_a_sequence_with_no_open_ahead_of_the_gap`.
        {
            let plan = Plan {
                steps: vec![
                    Step::own(Instruction::Perform {
                        id: "files.entry:/a#archive".into(),
                        arguments: serde_json::Map::new(),
                        confirmation: ConfirmationEvidence::None,
                    }),
                    Step::own(Instruction::Unknown("juggle three plates".into())),
                ],
            };
            assert_eq!(plan.unanswered(), Some("juggle three plates"));
        }

        // Case `an_open_excuses_a_clause_it_makes_answerable_but_only_once_it_comes_first`.
        {
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
                        arguments: serde_json::Map::new(),
                        confirmation: ConfirmationEvidence::None,
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
                        arguments: serde_json::Map::new(),
                        confirmation: ConfirmationEvidence::None,
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
}
