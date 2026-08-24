//! What the room's apps currently have open, and what may be done to it.
//!
//! The agent used to work from a written-down list of four things it knew how
//! to ask the word processor for. That list was checkable — a test read the
//! action names back off the app's own encoder — but it was still a list, and
//! it had exactly one app in it. Anything else in the room was unreachable, and
//! reaching a second app meant writing a second list.
//!
//! This module replaces the list with a question. An app on the qOS action
//! model publishes a [`Resource`] tree and the operations each resource exports
//! (`docs/action-model-recipe.md`); [`qos_actions::door`] turns a live tree into
//! inert, stably-addressed descriptors and performs one of them attributed to
//! whoever asked. So the agent does not need to know what an app can do — it
//! asks, every time, and what comes back is whatever the app is publishing at
//! that moment.
//!
//! ## What the agent supplies, and what it must not
//!
//! A resource tree is a function of the document **and** of what is open: the
//! word processor's sections come from the document, but which block its
//! structural inserts anchor on is a caret, and the explorer's entries are a
//! directory. [`View`] is that half, and it is the agent's *own* — the caret it
//! publishes to the room, the directory it is looking at. It is deliberately
//! not read off anybody's screen, and it carries no coordinates.
//!
//! Everything else comes from the app: the resource ids, the action ids, the
//! labels a person would see, whether an operation takes text, and the encoded
//! action itself. The agent never mints any of it, which is what keeps
//! "discovered" from meaning "guessed".
//!
//! ## Addresses
//!
//! `"<resource id>#<action id>"`, the same spelling the shell's own action
//! surface uses (`shell::shell::search::providers::app_resources`), so one
//! address names one operation whether it arrives through the shell's catalogue
//! or is built here. Never a screen position: the same address keeps meaning
//! the same operation across layouts, themes and sessions.
//!
//! ## Authority
//!
//! A denied operation is **absent** from [`offers`], not marked unavailable in
//! it — there is no "allowed" flag on an [`Offer`] for anything downstream to
//! ignore. The question asked is the one the shell's own gate asks of a caller
//! reaching an app through the door: `Write` on `(app, action)`, resolved
//! against the capability set the agent holds, and denied unless something
//! explicitly permits. The check runs again at dispatch, because a survey is a
//! snapshot and the thing that must not happen is acting on a stale one.

use std::path::PathBuf;

use automerge::AutoCommit;
use qos_actions::door::{ActionAddress, ActionDescriptor};
use qos_actions::{ActionRegistry, Resource};
use ui::permissions::{PermissionContext, PermissionScope, ResourceRef, Verb};

/// Separator between a resource id and an action id in an address.
///
/// `#` because a resource id may contain `.`, `:` and `/` — a file entry's id
/// is an absolute path — while an action id is a plain identifier, and neither
/// half ever contains `#`. Splitting on the last occurrence is therefore
/// unambiguous, and the whole address survives a trip through a JSON reply
/// legibly enough for a person debugging one to read it.
pub const ADDRESS_SEP: char = '#';

/// The single-string form of an address.
pub fn encode_address(address: &ActionAddress) -> String {
    format!("{}{}{}", address.resource, ADDRESS_SEP, address.action)
}

/// Recover an address from [`encode_address`]. `None` when the string is not
/// one — an address a model made up is refused here rather than looked up
/// against a half-parsed id.
pub fn decode_address(id: &str) -> Option<ActionAddress> {
    let (resource, action) = id.rsplit_once(ADDRESS_SEP)?;
    if resource.is_empty() || action.is_empty() {
        return None;
    }
    Some(ActionAddress::new(resource, action))
}

/// The app a resource id belongs to: the segment before the first `.`.
///
/// Resource ids are app-namespaced by convention across the tree
/// (`word-processor.document`, `files.entry:/…`), which is what lets the
/// capability question name the app being acted on without a second table of
/// who owns what.
pub fn app_of(resource_id: &str) -> &str {
    resource_id
        .split_once('.')
        .map(|(head, _)| head)
        .unwrap_or(resource_id)
}

/// Where the agent is looking.
///
/// Its own view state, and the only thing it contributes to a resource tree.
/// Nothing here is a screen position: a block id and a directory path are
/// addresses, and both keep meaning the same thing on a machine with no display
/// at all.
#[derive(Clone, Debug, Default)]
pub struct View {
    /// The block the agent's caret is on, for the app whose subject is a
    /// document. Structural inserts anchor here.
    pub focus_block: Option<String>,
    /// Where in that block.
    pub focus_offset: usize,
    /// The blocks a block-level operation would target — the agent's
    /// selection, or just the focus block.
    pub target_blocks: Vec<String>,
    /// The directory the agent is browsing, for an app whose subject is the
    /// filesystem.
    pub directory: Option<PathBuf>,
}

/// One app whose contract the agent speaks.
///
/// Every field is the app's own code. Adding an app is one entry: what it
/// announces, how its documents are told apart from every other app's in the
/// room, the function that builds its tree, and the decoders it registers.
/// Nothing here describes *what* the app can do — that is the tree's job, and
/// it is asked afresh each time.
pub struct AppContract {
    /// What a launched copy of this app announces to the broker.
    pub app_id: &'static str,
    /// The namespace its resource ids start with. Also the app name the
    /// capability question is phrased about.
    pub namespace: &'static str,
    /// Whether a document in the room is one this app authored.
    ///
    /// A room carries every app's state and none of it is labelled; the broker
    /// fans a frame out by address, and a participant that announced no app id
    /// reaches every subscriber. So a held replica has to be attributable, and
    /// the app is the only thing that knows its own schema.
    pub recognises: fn(&AutoCommit) -> bool,
    /// The app's own resource tree for a view of a document.
    pub resources: fn(&View, &AutoCommit) -> Vec<Resource>,
    /// The app's own decoders, so the door can prove what it would run before
    /// running it.
    pub register: fn(&mut ActionRegistry),
}

fn word_processor_resources(view: &View, doc: &AutoCommit) -> Vec<Resource> {
    // The anchor for a structural insert. The agent's caret when it has one,
    // else the document's first block — the same fallback a freshly opened
    // window makes, and never an invented id.
    let anchor = view.focus_block.clone().or_else(|| {
        let c = qdoc::model::open(doc, &automerge::ROOT).ok()?;
        qdoc::model::materialize(doc, &c)
            .blocks
            .first()
            .map(|b| b.id.clone())
    });
    let Some(anchor) = anchor else {
        return Vec::new();
    };
    let targets = if view.target_blocks.is_empty() {
        vec![anchor.clone()]
    } else {
        view.target_blocks.clone()
    };
    word_processor::actions::provide_resources(
        &word_processor::actions::WpResourceCtx {
            caret_block: anchor,
            caret_offset: view.focus_offset,
            target_blocks: targets,
        },
        doc,
    )
}

fn file_explorer_resources(view: &View, doc: &AutoCommit) -> Vec<Resource> {
    let Some(directory) = view.directory.clone() else {
        return Vec::new();
    };
    // The explorer's own reader, filtered the way it filters: an agent that
    // listed the directory itself would be publishing its own idea of what is
    // there, under the app's ids.
    let mut entries = file_explorer::model::read_dir_sync(&directory).unwrap_or_default();
    entries.retain(|e| !e.name.starts_with('.'));
    file_explorer::model::sort_entries(
        file_explorer::model::SortBy::Name,
        file_explorer::model::SortOrder::Ascending,
        &mut entries,
    );
    file_explorer::actions::provide_resources(
        &file_explorer::actions::FilesResourceCtx {
            directory,
            entries,
        },
        doc,
    )
}

/// Every app the agent can speak to.
///
/// Two, deliberately of different shapes: one whose subject is a rich-text
/// document and one whose subject is a host filesystem. Neither is special-cased
/// anywhere below — everything past this point works from the tree.
pub static CONTRACTS: &[AppContract] = &[
    AppContract {
        app_id: "dev.qos.word-processor",
        namespace: "word-processor",
        recognises: |doc| qdoc::model::open(doc, &automerge::ROOT).is_ok(),
        resources: word_processor_resources,
        register: word_processor::actions::register,
    },
    AppContract {
        app_id: "dev.qos.file-explorer",
        namespace: "files",
        recognises: file_explorer::actions::is_explorer_document,
        resources: file_explorer_resources,
        register: file_explorer::actions::register,
    },
];

/// The contract for a document, or `None` when nothing in the room's apps
/// claims it.
pub fn recognise(doc: &AutoCommit) -> Option<&'static AppContract> {
    CONTRACTS.iter().find(|c| (c.recognises)(doc))
}

/// The contract whose resource ids start with `namespace`.
pub fn for_namespace(namespace: &str) -> Option<&'static AppContract> {
    CONTRACTS.iter().find(|c| c.namespace == namespace)
}

/// The contract for an announced app id.
pub fn for_app_id(app_id: &str) -> Option<&'static AppContract> {
    CONTRACTS.iter().find(|c| c.app_id == app_id)
}

/// Decoders for every app the agent speaks to.
///
/// One registry rather than one per app: the door decodes an encoded action
/// through it before dispatching, and a type no app registered is a refusal
/// rather than a best-effort apply.
pub fn registry() -> ActionRegistry {
    let mut r = ActionRegistry::new();
    for c in CONTRACTS {
        (c.register)(&mut r);
    }
    r
}

/// One operation the room exports right now.
///
/// Inert: the address and the words the app puts on it, and nothing that could
/// perform it. Read-only is a property of this type's shape rather than a
/// promise — there is no callback, token or handle in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    /// The app that published it, by namespace.
    pub app: &'static str,
    pub descriptor: ActionDescriptor,
}

impl Offer {
    /// The address, in the one-string form an answer names it by.
    pub fn id(&self) -> String {
        encode_address(&self.descriptor.address)
    }

    pub fn address(&self) -> &ActionAddress {
        &self.descriptor.address
    }

    /// Whether performing it must supply text.
    pub fn wants_text(&self) -> bool {
        self.descriptor.input.wants_text()
    }

    /// One line, in the app's own words: what this does and what it does it to.
    pub fn summary(&self) -> String {
        let mut s = format!(
            "{} — on {} ({})",
            self.descriptor.action_label, self.descriptor.resource_label, self.descriptor.resource_kind
        );
        if let qos_actions::InputSpec::Text { prompt, .. } = &self.descriptor.input {
            s.push_str(&format!("; takes text: {prompt}"));
        }
        if let Some(d) = &self.descriptor.description {
            s.push_str(&format!(". {d}"));
        }
        s
    }
}

/// The permission question performing `address` has to pass.
///
/// Phrased about the app the resource id names, not about the agent's own
/// namespace: a room grants edit rights per app, so folding every app's
/// operations under one name would make a grant in one of them a grant in all.
pub fn scope_for(address: &ActionAddress) -> PermissionScope {
    PermissionScope {
        resource: ResourceRef::Action {
            app: app_of(&address.resource).to_string(),
            action: address.action.clone(),
        },
        op: Verb::Write,
    }
}

/// Whether the agent may perform `address`. Fail-closed: anything not
/// affirmatively allowed is denied.
pub fn permitted(perms: &PermissionContext, address: &ActionAddress) -> bool {
    perms.check(&scope_for(address))
}

/// The tree `contract` publishes for `doc` as it stands, seen from `view`.
///
/// Rebuilt on every call, never cached. An app's tree changes with every edit —
/// a new heading is a new addressable section, a renamed file is a new label —
/// and the whole value of an address is that it names what is there *now*.
pub fn tree(contract: &AppContract, view: &View, doc: &AutoCommit) -> Vec<Resource> {
    (contract.resources)(view, doc)
}

/// Everything the room exports that the agent may perform.
///
/// The union across every document it holds, in the order the apps are declared
/// and then depth-first within each — parents before children, so a caller sees
/// the document before its sections and the folder before its files.
pub fn offers(
    held: &[(&'static AppContract, &AutoCommit)],
    view: &View,
    perms: &PermissionContext,
) -> Vec<Offer> {
    let mut out = Vec::new();
    for (contract, doc) in held {
        let resources = tree(contract, view, doc);
        for descriptor in qos_actions::door::survey(&resources) {
            // Denied means absent. Nothing downstream is handed an operation
            // it may not perform, so nothing downstream can decide to perform
            // one anyway.
            if !permitted(perms, &descriptor.address) {
                continue;
            }
            out.push(Offer {
                app: contract.namespace,
                descriptor,
            });
        }
    }
    out
}

/// Find the offer an id names, in a freshly taken survey.
pub fn find<'a>(offers: &'a [Offer], id: &str) -> Option<&'a Offer> {
    let address = decode_address(id)?;
    offers.iter().find(|o| *o.address() == address)
}

// ---------------------------------------------------------------------------
// The deterministic floor
// ---------------------------------------------------------------------------

/// Words that carry no intent and must not decide anything.
const NOISE: &[&str] = &[
    "the", "this", "that", "a", "an", "and", "or", "to", "of", "in", "on", "for", "with", "please",
    "can", "you", "could", "would", "it", "its", "my", "our", "me", "us", "into", "from", "at",
    "by", "as", "is", "are", "be", "do", "does", "make", "made", "up", "out",
];

fn significant(word: &str) -> bool {
    word.chars().count() >= 3 && !NOISE.contains(&word)
}

/// The instruction's words, lowercased, with punctuation dropped and the noise
/// removed.
fn keywords(rest: &str) -> Vec<String> {
    rest.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| significant(w))
        .map(|w| w.to_string())
        .collect()
}

/// A literal the instruction quoted, if it quoted exactly one.
///
/// The only argument the floor will ever supply. An operation that collects
/// text needs words that are not in the offer, and with no model there is
/// nowhere honest to get them from except somewhere the instruction said them
/// outright. Anything less than a quoted literal would be the agent inventing
/// content and calling it the user's.
pub fn quoted_literal(rest: &str) -> Option<String> {
    let mut found: Vec<String> = Vec::new();
    for quote in ['"', ''', '\u{201c}'] {
        let close = if quote == '\u{201c}' { '\u{201d}' } else { quote };
        let mut chars = rest.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if c != quote {
                continue;
            }
            let rest_of = &rest[i + c.len_utf8()..];
            if let Some(end) = rest_of.find(close) {
                let lit = rest_of[..end].trim();
                if !lit.is_empty() {
                    found.push(lit.to_string());
                }
                // Skip past the closing quote.
                while let Some(&(j, _)) = chars.peek() {
                    if j >= i + c.len_utf8() + end {
                        break;
                    }
                    chars.next();
                }
            }
        }
    }
    match found.len() {
        1 => found.pop(),
        // Two literals is an instruction naming a thing *and* a value, which
        // this vocabulary cannot take apart. Refusing beats picking one.
        _ => None,
    }
}

/// How well an offer answers `words`: how many of them the app's own labels
/// carry. Zero means it does not answer at all.
fn score(offer: &Offer, words: &[String]) -> usize {
    let action = offer.descriptor.action_label.to_lowercase();
    let resource = offer.descriptor.resource_label.to_lowercase();
    let kind = offer.descriptor.resource_kind.to_lowercase();
    words
        .iter()
        .filter(|w| {
            // The verb has to land on the operation. Matching only the thing
            // being acted on says which file, never what to do to it.
            action.contains(w.as_str()) || resource.contains(w.as_str()) || kind.contains(w.as_str())
        })
        .count()
}

/// What the deterministic floor decided, when it could not decide.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoMatch {
    /// Nothing the room exports carries a word of the instruction.
    Nothing,
    /// Several operations answer it equally well, and choosing between them
    /// would be a guess.
    Ambiguous(Vec<String>),
    /// The operation collects text and the instruction quoted none, so there is
    /// nothing to perform it with that the instruction actually said.
    NeedsText(String),
}

/// The floor: what the agent decides on its own when no model answered.
///
/// A keyword vocabulary cannot enumerate an open-ended action set, so this does
/// not try to. It matches the instruction's own words against the words the
/// apps put on their operations, and it insists on a clear winner: an
/// instruction that fits two operations equally is [`NoMatch::Ambiguous`], not
/// whichever came first. An operation that needs text gets the literal the
/// instruction quoted or nothing at all.
///
/// The refusals are the point. A run with no model reachable either does the
/// one thing the instruction plainly names, or says which of these three it
/// ran into — it never reports success for something it did not do, and never
/// substitutes the nearest available operation for the one that was asked for.
pub fn from_verbs<'a>(
    rest: &str,
    offers: &'a [Offer],
) -> Result<(&'a Offer, Option<String>), NoMatch> {
    let words = keywords(rest);
    if words.is_empty() || offers.is_empty() {
        return Err(NoMatch::Nothing);
    }
    let mut best: Vec<&Offer> = Vec::new();
    let mut best_score = 0usize;
    for offer in offers {
        let s = score(offer, &words);
        if s == 0 {
            continue;
        }
        if s > best_score {
            best_score = s;
            best = vec![offer];
        } else if s == best_score {
            best.push(offer);
        }
    }
    if best.is_empty() {
        return Err(NoMatch::Nothing);
    }
    if best.len() > 1 {
        return Err(NoMatch::Ambiguous(
            best.iter().take(4).map(|o| o.id()).collect(),
        ));
    }
    let chosen = best[0];
    if !chosen.wants_text() {
        return Ok((chosen, None));
    }
    match quoted_literal(rest) {
        Some(text) => Ok((chosen, Some(text))),
        None => Err(NoMatch::NeedsText(chosen.id())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qos_actions::door::ActionAddress;

    #[test]
    fn an_address_with_a_path_in_it_round_trips() {
        let a = ActionAddress::new("files.entry:/var/workspace/a.b/plan.md", "rename");
        assert_eq!(decode_address(&encode_address(&a)), Some(a));
    }

    #[test]
    fn the_app_is_read_off_the_resource_namespace() {
        assert_eq!(app_of("word-processor.document"), "word-processor");
        assert_eq!(app_of("files.entry:/var/workspace/plan.md"), "files");
    }

    #[test]
    fn one_quoted_literal_is_an_argument_and_two_are_not() {
        assert_eq!(quoted_literal(r#"rename it to "launch.md""#), Some("launch.md".into()));
        assert_eq!(quoted_literal(r#"rename "a.md" to "b.md""#), None);
        assert_eq!(quoted_literal("rename it to launch.md"), None);
    }
}
