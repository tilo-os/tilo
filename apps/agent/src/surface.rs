//! What the room's apps currently have open, and what may be done to it.
//!
//! An app on the Tilo action model publishes a [`Resource`](tilo_actions::Resource) tree and the
//! operations each resource exports (`docs/action-model-recipe.md`), and
//! [`tilo_actions::door`] turns a live tree into inert, stably-addressed
//! descriptors the agent can perform. The agent queries that tree each time,
//! so it receives the operations the app currently publishes.
//!
//! ## What the agent supplies, and what it must not
//!
//! Nothing but the arguments it fills in. Everything comes from the app: the resource ids, the action ids, the
//! labels a person would see, whether an operation takes text, and the
//! encoded action itself. The agent never mints any of it.
//!
//! ## Addresses
//!
//! `"<resource id>#<action id>"`, the same spelling the shell's own action
//! surface uses (`shell::shell::search::providers::app_resources`), so one
//! address names one operation wherever it is built. Never a screen
//! position: the same address keeps meaning the same operation across
//! layouts, themes and sessions.
//!
//! ## Authority
//!
//! A denied operation is **absent** from [`offers`](Catalogue::offers), not marked unavailable
//! in it. The question asked is the shell's own gate: `Write` on
//! `(app, action)`, resolved against the agent's capability set and denied
//! unless something explicitly permits. It runs again at dispatch, because a
//! survey is a snapshot and acting on a stale one must not happen.

use std::fmt::Write as _;

use tilo_actions::ActionParam;
use tilo_actions::door::{ActionAddress, ActionDescriptor};

/// Separator between a resource id and an action id in an address.
///
/// `#` because a resource id may contain `.`, `:` and `/`, while an action id
/// is a plain identifier and neither half ever contains `#`. Splitting on the
/// last occurrence is unambiguous.
pub const ADDRESS_SEP: char = '#';

/// The single-string form of an address.
pub fn encode_address(address: &ActionAddress) -> String {
    encode_parts(&address.resource, &address.action)
}

/// Encode the two halves of an address, for a stored step that kept them.
pub fn encode_parts(resource: &str, action: &str) -> String {
    format!("{resource}{ADDRESS_SEP}{action}")
}

/// Recover an address from [`encode_address`]. `None` when the string is not
/// one — an address a model made up is refused rather than looked up
/// against a half-parsed id.
pub fn decode_address(id: &str) -> Option<ActionAddress> {
    let id = if let Some(rest) = id.strip_prefix("broker:") {
        rest.split_once(':')?.1
    } else {
        id
    };
    let (resource, action) = id.rsplit_once(ADDRESS_SEP)?;
    if resource.is_empty() || action.is_empty() {
        return None;
    }
    Some(ActionAddress::new(resource, action))
}

/// The action id an application exports so that it can be opened.
pub const OPEN_ACTION: &str = "open";

/// The tail of the resource id an application has *as an application* rather
/// than as a document.
pub const APP_RESOURCE: &str = "app";

/// The stable kind tag on that resource, shared across every app.
pub const APPLICATION_KIND: &str = "applications.app";

/// The resource prefix of an open of a document the room already has:
/// `record.<app id>.<record id>#open`. Opening it puts that document's
/// operations on the list; `app.<app id>#open` creates a new one.
pub const RECORD_OPEN_PREFIX: &str = "record.";

/// The address that opens `record`, a document of `app_id` the room
/// already has.
pub fn open_record_address(app_id: &str, record: &str) -> ActionAddress {
    ActionAddress::new(
        format!("{RECORD_OPEN_PREFIX}{app_id}.{record}"),
        OPEN_ACTION,
    )
}

/// The app and record an open of an existing document names
/// ([`open_record_address`]), or `None` when `address` is no such open.
///
/// An app id is dotted and never holds a `:`; a record id names its scheme
/// before a `:` (`uuid:…`) and may hold dots after it. So the record starts
/// at the last `.` before the first `:`, or, for a record id without a
/// scheme, at the last `.`.
pub fn opens_record(address: &ActionAddress) -> Option<(&str, &str)> {
    if address.action != OPEN_ACTION {
        return None;
    }
    let rest = address.resource.strip_prefix(RECORD_OPEN_PREFIX)?;
    let head = rest.split_once(':').map_or(rest, |(head, _)| head);
    let (app_id, record) = rest.split_at_checked(head.rfind('.')?)?;
    let record = record.strip_prefix('.')?;
    (!app_id.is_empty() && !record.is_empty()).then_some((app_id, record))
}

/// The app `address` would open, or `None` when it opens nothing: a new
/// document of it (`app.<app id>#open`, `<namespace>.app#open`), or one the
/// room already has ([`opens_record`]).
pub fn opens(address: &ActionAddress) -> Option<&str> {
    if address.action != OPEN_ACTION {
        return None;
    }
    if let Some((app_id, _)) = opens_record(address) {
        return Some(app_id);
    }
    if let Some(app_id) = address.resource.strip_prefix("app.") {
        return (!app_id.is_empty()).then_some(app_id);
    }
    let (namespace, tail) = address.resource.split_once('.')?;
    (tail == APP_RESOURCE).then_some(namespace)
}

/// One operation the room exports right now.
///
/// Inert: the address and the words the app puts on it, and nothing that
/// could perform it — there is no callback, token or handle in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    /// The app that published it, by namespace.
    pub app: String,
    /// Exact live publisher selected by the broker catalogue. A closed
    /// application instead carries `broker_provider`, never publisher zero.
    pub broker_peer_id: Option<u64>,
    /// Stable application/retained route, independent of a live connection.
    pub broker_provider: Option<String>,
    pub descriptor: ActionDescriptor,
}

impl Offer {
    /// The address, in the one-string form an answer names it by.
    pub fn id(&self) -> String {
        let address = encode_address(&self.descriptor.address);
        if let Some(provider) = &self.broker_provider {
            let provider = provider.replace('%', "%25").replace(':', "%3A");
            return format!("broker:{provider}:{address}");
        }
        self.broker_peer_id.map_or_else(
            || address.clone(),
            |peer| format!("broker:{peer}:{address}"),
        )
    }

    pub fn address(&self) -> &ActionAddress {
        &self.descriptor.address
    }

    /// Whether performing it requires a structured argument object.
    pub fn wants_arguments(&self) -> bool {
        self.descriptor.params.iter().any(|param| param.required)
    }

    /// Whether every value it needs is one of a few words it names (an
    /// alignment, a style), which an instruction says outright rather than
    /// quotes.
    pub fn wants_only_choices(&self) -> bool {
        self.wants_arguments()
            && self
                .descriptor
                .params
                .iter()
                .filter(|param| param.required)
                .all(|param| !param.choices.is_empty())
    }

    /// The choices the instruction's own words name for each required
    /// parameter that has them.
    ///
    /// # Errors
    /// The choices of the first parameter the instruction names none, or
    /// more than one, of.
    pub fn arguments_from_choices(
        &self,
        instruction: &str,
    ) -> Result<serde_json::Map<String, serde_json::Value>, Vec<String>> {
        let words: Vec<String> = keywords(instruction).iter().map(|w| spelled(w)).collect();
        let mut arguments = serde_json::Map::new();
        for param in self.descriptor.params.iter().filter(|param| param.required) {
            let named: Vec<String> = param
                .choices
                .iter()
                .map(ToString::to_string)
                .filter(|choice| words.iter().any(|word| *word == choice.to_lowercase()))
                .collect();
            let [choice] = named.as_slice() else {
                return Err(param.choices.iter().map(ToString::to_string).collect());
            };
            arguments.insert(
                param.name.to_string(),
                serde_json::Value::String(choice.clone()),
            );
        }
        Ok(arguments)
    }

    /// Whether the text it wants is the address of content some other
    /// operation produced, rather than words the instruction can quote.
    ///
    /// Nobody writes a content hash into an instruction, and inventing one
    /// would name bytes that do not exist — so an operation that wants
    /// content can only be performed as a step after something that
    /// produced some.
    pub fn wants_content(&self) -> bool {
        self.content_param().is_some()
    }

    /// Whether performing it answers with content another operation takes:
    /// its declared result is a `tilo.content` resource.
    pub fn produces_content(&self) -> bool {
        matches!(
            &self.descriptor.contract.result,
            tilo_actions::ResultSpec::Resource { resource_kind }
                if resource_kind == tilo_actions::CONTENT_RESOURCE_KIND
        )
    }

    /// The parameter content another operation produced is carried under:
    /// its typed content parameter (`#[param(content)]`), or the one input of
    /// an operation that collects only content.
    pub fn content_param(&self) -> Option<String> {
        if let Some(param) = self
            .descriptor
            .params
            .iter()
            .find(|param| param.is_content())
        {
            return Some(param.name.to_string());
        }
        self.descriptor
            .input
            .wants_content()
            .then(|| tilo_actions::PROMPTED_PARAM.to_owned())
    }

    /// Deterministic floor for the common one-text-parameter action. Richer
    /// shapes are deliberately left to the structured planner; guessing how
    /// prose maps onto multiple or non-text fields would violate the schema.
    pub fn arguments_from_literal(
        &self,
        instruction: &str,
    ) -> Option<serde_json::Map<String, serde_json::Value>> {
        let [param] = self.descriptor.params.as_slice() else {
            return None;
        };
        if !matches!(
            param.ty,
            tilo_actions::ParamType::Text | tilo_actions::ParamType::Secret
        ) {
            return None;
        }
        let literal = quoted_literal(instruction)?;
        let mut arguments = serde_json::Map::new();
        arguments.insert(param.name.to_string(), serde_json::Value::String(literal));
        Some(arguments)
    }

    /// One line, in the app's own words: what this does and what it does it
    /// to.
    pub fn summary(&self) -> String {
        let mut s = format!(
            "{} — on {} ({})",
            self.descriptor.action_label,
            self.descriptor.resource_label,
            self.descriptor.resource_kind
        );
        if !self.descriptor.params.is_empty() {
            let names = self
                .descriptor
                .params
                .iter()
                .map(|param| format!("{}: {}", param.name, param.ty.label()))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = write!(s, "; arguments {{{names}}}");
        }
        if let Some(d) = &self.descriptor.description {
            let _ = write!(s, ". {d}");
        }
        s
    }
}

/// Convert the broker's permission-filtered, live catalogue into the same
/// inert offers the planner already consumes. No application registry or
/// document reconstruction is involved: arbitrary apps appear here solely
/// because they published a descriptor to the authenticated broker.
///
/// Accepts either the action host's fetched [`tilo_actions::remote::CataloguePage`]
/// or the broker's pushed [`ui::collaboration::BrokerActionSurface`] directory.
pub fn broker_offers<C: BrokerCatalogue + ?Sized>(catalogue: &C) -> Vec<Offer> {
    catalogue.broker_offers()
}

/// A live, permission-filtered action directory the broker supplies.
pub trait BrokerCatalogue {
    /// Every live action it names, as planner offers.
    fn broker_offers(&self) -> Vec<Offer>;
}

impl BrokerCatalogue for tilo_actions::remote::CataloguePage {
    fn broker_offers(&self) -> Vec<Offer> {
        catalogue_page_offers(self)
    }
}

impl BrokerCatalogue for [ui::collaboration::BrokerActionSurface] {
    fn broker_offers(&self) -> Vec<Offer> {
        surface_offers(self)
    }
}

impl BrokerCatalogue for Vec<ui::collaboration::BrokerActionSurface> {
    fn broker_offers(&self) -> Vec<Offer> {
        surface_offers(self)
    }
}

/// Offers from the broker's pushed directory, including inert closed-app routes.
fn surface_offers(surfaces: &[ui::collaboration::BrokerActionSurface]) -> Vec<Offer> {
    surfaces
        .iter()
        .filter(|surface| surface.live || surface.provider.is_some())
        .flat_map(|surface| {
            surface.actions.iter().map(|action| Offer {
                app: surface.app_id.clone(),
                broker_peer_id: surface.provider.is_none().then_some(surface.peer_id),
                broker_provider: surface.provider.clone(),
                descriptor: ActionDescriptor {
                    // The pushed directory routes invocations; it does not
                    // open views.
                    view: None,
                    ..action.descriptor(tilo_actions::ExecutionLocus::ServerPreferred)
                },
            })
        })
        .collect()
}

fn catalogue_page_offers(catalogue: &tilo_actions::remote::CataloguePage) -> Vec<Offer> {
    catalogue
        .rows
        .iter()
        .filter_map(|entry| {
            // A live action names a connection. Closed applications and
            // workspace actions retain their explicit stable route instead.
            let peer = entry.provider.parse::<u64>().ok();
            if peer.is_none()
                && tilo_actions::remote::workspace_provider_room(&entry.provider).is_none()
                && !entry.provider.starts_with("launch:")
                && tilo_actions::remote::application_provider(&entry.provider).is_none()
            {
                return None;
            }
            let mut descriptor = entry.action.clone()?;
            descriptor.locus = tilo_actions::ExecutionLocus::ServerPreferred;
            Some(Offer {
                app: entry.app_id.clone(),
                broker_peer_id: peer,
                broker_provider: peer.is_none().then(|| entry.provider.clone()),
                descriptor,
            })
        })
        .collect()
}

/// One resource as its app publishes it, with the operations taken out.
///
/// [`Offer`] carries an operation; a resource that exports **nothing**
/// never becomes an offer, so anything an app says by publishing one alone
/// — a note in a child resource's label, say a picture's pixel dimensions —
/// is invisible to a caller working from the survey alone. This is that
/// other half.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceFacts {
    pub id: String,
    pub kind: String,
    pub label: String,
    /// The labels of the child resources that export nothing.
    pub notes: Vec<String>,
}

/// What the room exports and what it says about itself, taken together, at
/// the same moment against the same documents — asking separately would be
/// reading two different rooms.
#[derive(Clone, Debug, Default)]
pub struct Catalogue {
    /// Everything the agent may perform. Denied is absent, as in [`offers`](Self::offers).
    pub offers: Vec<Offer>,
    /// Every resource the room's apps publish, gate or no gate: showing a
    /// resource is non-destructive and needs no permission, acting on one
    /// does — and the only way to ask for a fact is to name an address that
    /// already passed the gate to be in [`Self::offers`].
    pub facts: Vec<ResourceFacts>,
    /// The namespaces of the apps the room is actually holding, read off
    /// the held replicas rather than inferred from [`Self::offers`]: an app
    /// that is here but has nothing open exports no operations and is also
    /// offered as openable, so guessing "open" from the offers would report
    /// a running application as absent.
    pub held: Vec<&'static str>,
}

impl Catalogue {
    /// The facts about the resource `address` is published by.
    pub fn about(&self, resource: &str) -> Option<&ResourceFacts> {
        self.facts.iter().find(|f| f.id == resource)
    }
}

/// Find the offer an id names, in a freshly taken survey.
pub fn find<'a>(offers: &'a [Offer], id: &str) -> Option<&'a Offer> {
    offers.iter().find(|offer| offer.id() == id)
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

/// The instruction's words, lowercased, with punctuation dropped and the
/// noise removed.
/// `word` in the spelling apps name their choices in.
fn spelled(word: &str) -> String {
    match word {
        "centre" | "centred" | "centered" => "center".to_owned(),
        other => other.to_owned(),
    }
}

/// Every word of `text`, lowercased and in the spelling apps name their
/// choices in, noise and all.
fn words_of(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(spelled)
        .collect()
}

fn keywords(rest: &str) -> Vec<String> {
    rest.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| significant(w))
        .map(std::string::ToString::to_string)
        .collect()
}

/// A literal the instruction quoted, if it quoted exactly one.
///
/// The only argument the floor will ever supply: an operation that collects
/// text needs words that are not in the offer, and with no model the only
/// honest source is somewhere the instruction said them outright.
///
/// Straight and typographic double quotes only. An apostrophe is a letter in
/// English prose far more often than a quotation mark, and reading "don't
/// rename it" as a quoted `t rename it` is exactly the confident nonsense
/// this vocabulary exists to avoid.
pub fn quoted_literal(rest: &str) -> Option<String> {
    let mut found: Vec<String> = Vec::new();

    // Straight quotes: splitting alternates outside/inside, so every odd
    // segment is quoted. An unterminated quote leaves a final odd segment,
    // which is why the count has to be even to have closed.
    let parts: Vec<&str> = rest.split('"').collect();
    if parts.len() % 2 == 1 {
        for inside in parts.iter().skip(1).step_by(2) {
            let lit = inside.trim();
            if !lit.is_empty() {
                found.push(lit.to_string());
            }
        }
    }

    // Typographic quotes, which open and close with different characters.
    let mut haystack = rest;
    while let Some(open) = haystack.find('\u{201c}') {
        let Some(after) = haystack.get(open + '\u{201c}'.len_utf8()..) else {
            break;
        };
        let Some(close) = after.find('\u{201d}') else {
            break;
        };
        let Some(lit) = after.get(..close) else {
            break;
        };
        let lit = lit.trim();
        if !lit.is_empty() {
            found.push(lit.to_string());
        }
        let Some(rest_after) = after.get(close + '\u{201d}'.len_utf8()..) else {
            break;
        };
        haystack = rest_after;
    }

    match found.len() {
        1 => found.pop(),
        // Two literals names a thing *and* a value, which this vocabulary
        // cannot take apart. Refusing beats picking one.
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
            // The verb has to land on the operation. Matching only the
            // thing being acted on says which file, never what to do to it.
            // A word naming one of its choices (an alignment) lands on it
            // too.
            action.contains(w.as_str())
                || resource.contains(w.as_str())
                || kind.contains(w.as_str())
                || offer.descriptor.params.iter().any(|param| {
                    param
                        .choices
                        .iter()
                        .any(|choice| choice.to_lowercase() == spelled(w))
                })
        })
        .count()
}

/// What the deterministic floor decided, when it could not decide.
///
/// Every string here is a **label**, never an address: what the agent
/// writes back goes into a document somebody is reading, and an internal
/// identifier belongs in the log line next to the refusal, not in the
/// sentence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoMatch {
    /// Nothing the room exports carries a word of the instruction.
    Nothing,
    /// Several operations answer it equally well, and choosing between them
    /// would be a guess. Carries their labels.
    Ambiguous(Vec<String>),
    /// The operation collects text and the instruction quoted none, so
    /// there is nothing to perform it with that the instruction actually
    /// said. Carries its label.
    NeedsText(String),
}

/// The floor: what the agent decides on its own when no model answered.
///
/// A keyword vocabulary cannot enumerate an open-ended action set, so this
/// does not try to. It matches the instruction's own words against the
/// words the apps put on their operations, and insists on a clear winner:
/// an instruction that fits two operations equally is [`NoMatch::Ambiguous`],
/// not whichever came first — unless what the instruction *supplied* breaks
/// the tie itself (see `collects_a_value_said`). A run with no model
/// reachable either does the one thing plainly named, or says which of
/// these three it ran into — it never reports success for something it did
/// not do, and never substitutes the nearest available operation for the
/// one asked for.
///
/// # Errors
///
/// Returns [`NoMatch::Nothing`] when no offer's words match the
/// instruction, [`NoMatch::Ambiguous`] when more than one offer answers
/// equally well and nothing the instruction supplied breaks the tie, and
/// [`NoMatch::NeedsText`] when the one offer left collects a value but the
/// instruction quoted none.
pub fn from_verbs<'a>(
    rest: &str,
    offers: &'a [Offer],
) -> Result<(&'a Offer, serde_json::Map<String, serde_json::Value>), NoMatch> {
    let words = keywords(rest);
    if words.is_empty() || offers.is_empty() {
        return Err(NoMatch::Nothing);
    }
    let mut best = best_scored(offers, &words);
    if best.is_empty() {
        return Err(NoMatch::Nothing);
    }
    let said = quoted_literal(rest);
    if best.len() > 1 {
        // Two operations answering to one verb need not be two meanings.
        // Whether the instruction supplied a value is a fact about which of
        // them it named — the same fact dispatch reads a moment later, off
        // [`Offer::wants_text`] — so a tie between an operation that takes
        // one and an operation that does not is not a guess.
        //
        // Only ever narrowing, and never to nothing: if every tied
        // operation refuses what the instruction supplied, the tie stands
        // and is refused, which is what keeps a genuine ambiguity (two
        // operations that both take nothing) a question rather than a coin
        // toss.
        let fits: Vec<&Offer> = best
            .iter()
            .copied()
            .filter(|o| collects_a_value_said(o) == said.is_some())
            .collect();
        if !fits.is_empty() {
            best = fits;
        }
    }
    if best.len() > 1 {
        // Equal keyword hits still leave a shorter label closer to what
        // was said: "insert table" beats "generate table of contents" for
        // an instruction that only said "table". Align left / Align centre
        // stay tied (same length) and remain Ambiguous.
        let label_words = |offer: &Offer| keywords(&offer.descriptor.action_label).len().max(1);
        let fewest = best
            .iter()
            .map(|offer| label_words(offer))
            .min()
            .unwrap_or(1);
        let denser: Vec<&Offer> = best
            .iter()
            .copied()
            .filter(|offer| label_words(offer) == fewest)
            .collect();
        if !denser.is_empty() {
            best = denser;
        }
    }
    if best.len() > 1 {
        // Words too small to decide anything alone still tell two labels
        // apart once they tie: "zoom in" is Zoom in, not Zoom out.
        let said: std::collections::HashSet<String> = words_of(rest).into_iter().collect();
        let echoed = |offer: &Offer| {
            words_of(&offer.descriptor.action_label)
                .iter()
                .filter(|word| said.contains(*word))
                .count()
        };
        let most = best.iter().map(|offer| echoed(offer)).max().unwrap_or(0);
        let closer: Vec<&Offer> = best
            .iter()
            .copied()
            .filter(|offer| echoed(offer) == most)
            .collect();
        if !closer.is_empty() {
            best = closer;
        }
    }
    if best.len() > 1 {
        return Err(NoMatch::Ambiguous(
            best.iter()
                .take(4)
                .map(|o| o.descriptor.action_label.clone())
                .collect(),
        ));
    }
    // Unreachable given the checks above — `best` is non-empty and, past
    // the `best.len() > 1` refusal, has exactly one entry — but handled like
    // any other malformed state: refuse rather than panic.
    let Some(&chosen) = best.first() else {
        return Err(NoMatch::Nothing);
    };
    if chosen.wants_only_choices() {
        return chosen
            .arguments_from_choices(rest)
            .and_then(|arguments| with_place(chosen, rest, offers, arguments))
            .map(|arguments| (chosen, arguments))
            .map_err(|choices| {
                NoMatch::Ambiguous(
                    choices
                        .iter()
                        .take(4)
                        .map(|choice| format!("{} {choice}", chosen.descriptor.action_label))
                        .collect(),
                )
            });
    }
    if !collects_a_value_said(chosen) {
        return with_place(chosen, rest, offers, serde_json::Map::new())
            .map(|arguments| (chosen, arguments))
            .map_err(NoMatch::Ambiguous);
    }
    match chosen.arguments_from_literal(rest) {
        Some(arguments) => Ok((chosen, arguments)),
        None => Err(NoMatch::NeedsText(chosen.descriptor.action_label.clone())),
    }
}

/// The offers that answer `words` best, all equally well.
fn best_scored<'a>(offers: &'a [Offer], words: &[String]) -> Vec<&'a Offer> {
    let places = place_rows(offers);
    let mut best: Vec<&Offer> = Vec::new();
    let mut best_score = 0usize;
    for offer in offers {
        // A paragraph published as a place answers by what it does, never
        // by what it says: its words name where, not what to do.
        if places.contains(offer.descriptor.address.resource.as_str())
            && score_action(offer, words) == 0
        {
            continue;
        }
        let s = score(offer, words);
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
    best
}

/// How many of `words` the operation's own label carries.
fn score_action(offer: &Offer, words: &[String]) -> usize {
    let action = offer.descriptor.action_label.to_lowercase();
    words.iter().filter(|w| action.contains(w.as_str())).count()
}

/// The resources published as places inside another resource that offers
/// an operation told where to act (`at`): the paragraphs of a document,
/// each an item of it (`<resource>/<field>/<item>`), of the field the place
/// names when it names one.
pub(crate) fn place_rows(offers: &[Offer]) -> std::collections::HashSet<&str> {
    let placed = placing(offers);
    offers
        .iter()
        .map(|offer| offer.descriptor.address.resource.as_str())
        .filter(|resource| place_in(&placed, resource).is_some())
        .collect()
}

/// Each resource offering an operation told where to act, with the field
/// its places are items of when the operation names it.
fn placing(offers: &[Offer]) -> std::collections::HashMap<&str, Option<&str>> {
    let mut placed = std::collections::HashMap::new();
    for offer in offers {
        if let Some(param) = offer
            .descriptor
            .params
            .iter()
            .find(|param| param.is_place())
        {
            placed.insert(
                offer.descriptor.address.resource.as_str(),
                param.place_field(),
            );
        }
    }
    placed
}

/// The resource `resource` is a place in, and the item it names there.
fn place_in<'a>(
    placed: &std::collections::HashMap<&str, Option<&str>>,
    resource: &'a str,
) -> Option<(&'a str, &'a str)> {
    let (parent, rest) = resource.split_once('/')?;
    let (field, item) = rest.split_once('/')?;
    let named = placed.get(parent)?;
    (named.is_none_or(|named| named == field) && !item.is_empty() && !item.contains('/'))
        .then_some((parent, item))
}

/// The place inside `offer`'s resource that `rest` names, as the paragraphs
/// it publishes word it: "the title" is the first, a quoted phrase is the
/// paragraph that begins with it.
///
/// `Ok(None)` when the instruction names none: the operation acts at the
/// agent's caret.
///
/// # Errors
/// The labels of the paragraphs a quoted phrase begins more than one of.
fn named_place(
    offer: &Offer,
    rest: &str,
    offers: &[Offer],
) -> Result<Option<serde_json::Value>, Vec<String>> {
    let resource = offer.descriptor.address.resource.as_str();
    let field = offer
        .descriptor
        .params
        .iter()
        .find(|param| param.is_place())
        .and_then(ActionParam::place_field);
    let parent = std::collections::HashMap::from([(resource, field)]);
    let mut candidates: Vec<(&str, &str)> = Vec::new();
    for candidate in offers {
        if let Some((_, item)) = place_in(&parent, candidate.descriptor.address.resource.as_str())
            && !candidates.iter().any(|(known, _)| *known == item)
        {
            candidates.push((item, candidate.descriptor.resource_label.as_str()));
        }
    }
    let item = |item: &str| Some(serde_json::json!({ "item": item }));
    if let Some(phrase) = quoted_literal(rest) {
        let phrase = phrase.to_lowercase();
        let said = |label: &str| {
            let label = label.trim_end_matches('…').to_lowercase();
            label.starts_with(&phrase) || phrase.starts_with(&label)
        };
        let named: Vec<&(&str, &str)> =
            candidates.iter().filter(|(_, label)| said(label)).collect();
        return match named.as_slice() {
            [] => Ok(None),
            [(id, _)] => Ok(item(id)),
            many => Err(many.iter().map(|(_, label)| (*label).to_owned()).collect()),
        };
    }
    if words_of(rest).iter().any(|word| word == "title") {
        return Ok(candidates.first().and_then(|(id, _)| item(id)));
    }
    Ok(None)
}

/// `arguments` with the place `rest` names inside `offer`'s resource, when
/// it takes one and the instruction names one.
fn with_place(
    offer: &Offer,
    rest: &str,
    offers: &[Offer],
    mut arguments: serde_json::Map<String, serde_json::Value>,
) -> Result<serde_json::Map<String, serde_json::Value>, Vec<String>> {
    let Some(param) = offer
        .descriptor
        .params
        .iter()
        .find(|param| param.is_place())
    else {
        return Ok(arguments);
    };
    if let Some(place) = named_place(offer, rest, offers)? {
        arguments.insert(param.name.to_string(), place);
    }
    Ok(arguments)
}

/// Whether performing `offer` needs a value the instruction could have said.
///
/// Narrower than [`Offer::wants_text`]. An operation that collects
/// *content* wants text too, but the text it wants is a content hash —
/// there is none anywhere in a sentence, nobody writes one down, and
/// inventing one would name bytes that do not exist. So it is never asked
/// of the words: either an earlier step produced content and the caller
/// binds it, or the operation is refused at dispatch for having nothing to
/// place. This is the question to ask of an instruction and an operation
/// together — whether what the one said is what the other takes.
fn collects_a_value_said(offer: &Offer) -> bool {
    offer.wants_arguments() && !offer.wants_content() && !offer.wants_only_choices()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::ActionContract;
    use tilo_actions::door::ActionAddress;

    #[test]
    fn closed_routes_remain_distinct_without_inventing_a_publisher() {
        let make = |record: &str| {
            serde_json::from_value::<ui::collaboration::BrokerActionSurface>(serde_json::json!({
                "peer_id": 0,
                "provider": format!("launch:room:so.tilo.notes:{record}"),
                "app_id": "so.tilo.notes",
                "cluster_id": "room",
                "record_id": record,
                "live": false,
                "actions": [{
                    "resource": "notes.document",
                    "resource_kind": "notes.document",
                    "resource_label": "Notes",
                    "action": "rename",
                    "action_label": "Rename",
                    "description": null,
                    "operation_kind": tilo_actions::OperationKind::Document,
                    "params": [],
                    "contract": tilo_actions::ActionContract::default(),
                    "input": ui::collaboration::ExportedInput::None,
                }],
            }))
            .unwrap()
        };
        let offers = broker_offers(&vec![make("first"), make("second")]);
        assert_eq!(offers.len(), 2);
        assert_ne!(offers[0].id(), offers[1].id());
        for (offer, record) in offers.iter().zip(["first", "second"]) {
            assert_eq!(
                offer.broker_peer_id, None,
                "an inert route has no publisher authority"
            );
            assert_eq!(
                offer.broker_provider.as_deref(),
                Some(format!("launch:room:so.tilo.notes:{record}").as_str())
            );
            assert_eq!(decode_address(&offer.id()), Some(offer.address().clone()));
            assert_eq!(find(&offers, &offer.id()), Some(offer));
        }
    }

    #[test]
    fn offer_addresses_round_trip_and_stay_distinct_per_publisher() {
        {
            let a = ActionAddress::new("files.entry:/var/workspace/a.b/plan.md", "rename");
            assert_eq!(decode_address(&encode_address(&a)), Some(a));
        }

        {
            let entry = |peer_id: u64| tilo_actions::remote::CatalogueRow {
                provider: peer_id.to_string(),
                app_id: "so.tilo.calculator".into(),
                parent_resource: String::new(),
                resource: tilo_actions::remote::CatalogueResource {
                    id: "calculator.document".into(),
                    kind: "calculator.document".into(),
                    label: "Calculator".into(),
                    presentation: None,
                },
                has_children: false,
                visibility: None,
                action: Some(ActionDescriptor {
                    semantic_identity: None,
                    address: ActionAddress::new("calculator.document", "clear"),
                    resource_kind: "calculator.document".into(),
                    resource_label: "Calculator".into(),
                    action_label: "Clear".into(),
                    description: None,
                    locus: tilo_actions::ExecutionLocus::ClientLocal,
                    kind: tilo_actions::OperationKind::Effect,
                    contract: tilo_actions::ActionContract::default(),
                    params: Vec::new(),
                    input: tilo_actions::InputSpec::None,
                    view: None,
                }),
            };
            let catalogue = tilo_actions::remote::CataloguePage {
                revision: "1".into(),
                rows: vec![entry(41), entry(42)],
                ..tilo_actions::remote::CataloguePage::default()
            };
            let offers = broker_offers(&catalogue);
            assert_eq!(offers.len(), 2);
            assert_ne!(offers[0].id(), offers[1].id());
            assert_eq!(
                find(&offers, &offers[0].id()).unwrap().broker_peer_id,
                Some(41)
            );
            assert_eq!(
                find(&offers, &offers[1].id()).unwrap().broker_peer_id,
                Some(42)
            );
        }

        {
            let address = ActionAddress::new("app.so.tilo.diffmerge", OPEN_ACTION);
            let offer = Offer {
                app: "so.tilo.diffmerge".into(),
                broker_peer_id: Some(0),
                broker_provider: None,
                descriptor: ActionDescriptor {
                    semantic_identity: None,
                    address,
                    resource_kind: APPLICATION_KIND.into(),
                    resource_label: "Diffmerge".into(),
                    action_label: "Open Diffmerge".into(),
                    description: None,
                    locus: tilo_actions::ExecutionLocus::ServerPreferred,
                    kind: tilo_actions::OperationKind::Effect,
                    contract: ActionContract::default(),
                    params: Vec::new(),
                    input: tilo_actions::InputSpec::None,
                    view: None,
                },
            };
            let decoded = decode_address(&offer.id()).expect("broker offer id remains decodable");
            assert_eq!(opens(&decoded), Some("so.tilo.diffmerge"));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each case keeps its original fixture and assertions"
    )]
    fn a_quoted_literal_is_the_value_that_tells_same_verb_operations_apart() {
        {
            assert_eq!(
                quoted_literal(r#"rename it to "launch.md""#),
                Some("launch.md".into())
            );
            assert_eq!(quoted_literal(r#"rename "a.md" to "b.md""#), None);
            assert_eq!(quoted_literal("rename it to launch.md"), None);
            assert_eq!(
                quoted_literal("rename it to \u{201c}launch.md\u{201d}"),
                Some("launch.md".into())
            );
            assert_eq!(quoted_literal("don't rename anything"), None);
            assert_eq!(quoted_literal(r#"rename it to "launch.md"#), None);
        }

        {
            let offers = vec![
                offer(
                    "photos",
                    "photos.image:1",
                    "crop",
                    "Crop to a region",
                    tilo_actions::InputSpec::Text {
                        prompt: "Region to keep".to_string(),
                        placeholder: "0.25,0.25,0.5,0.5".to_string(),
                        collects: tilo_actions::Collects::Words,
                    },
                ),
                offer(
                    "photos",
                    "photos.image:1",
                    "crop-square",
                    "Crop to a square",
                    tilo_actions::InputSpec::None,
                ),
            ];

            let (chosen, argument) =
                from_verbs(r#"crop the photograph to "0.25,0.25,0.5,0.5""#, &offers)
                    .expect("a quoted region tells the two crops apart");
            assert_eq!(
                chosen.address().action,
                "crop",
                "a quoted region must reach the crop that collects one, not {}",
                chosen.descriptor.action_label,
            );
            assert_eq!(
                argument.get("value"),
                Some(&serde_json::Value::String("0.25,0.25,0.5,0.5".into()))
            );

            let (chosen, argument) = from_verbs("crop the photograph", &offers)
                .expect("with no region said, the crop that needs none is the one that can act");
            assert_eq!(chosen.address().action, "crop-square");
            assert!(argument.is_empty());

            // What is gone is a false ambiguity, not the mechanism: two
            // operations that both take nothing are still not told apart by an
            // instruction that supplies nothing, and that is still a genuine
            // question rather than a guess.
            let both_bare = vec![
                offer(
                    "photos",
                    "photos.image:1",
                    "rotate-left",
                    "Rotate left",
                    tilo_actions::InputSpec::None,
                ),
                offer(
                    "photos",
                    "photos.image:1",
                    "rotate-right",
                    "Rotate right",
                    tilo_actions::InputSpec::None,
                ),
            ];
            match from_verbs("rotate the photograph", &both_bare) {
                Err(NoMatch::Ambiguous(labels)) => assert_eq!(labels.len(), 2, "{labels:?}"),
                other => panic!("a genuine ambiguity must still be refused: {other:?}"),
            }

            // A small word the instruction said still tells two labels apart
            // once they tie: "zoom in" is Zoom in, never Zoom out.
            let zooms = vec![
                offer(
                    "word-processor",
                    "word-processor.document",
                    "zoom-in",
                    "Zoom in",
                    tilo_actions::InputSpec::None,
                ),
                offer(
                    "word-processor",
                    "word-processor.document",
                    "zoom-out",
                    "Zoom out",
                    tilo_actions::InputSpec::None,
                ),
            ];
            let (chosen, _) = from_verbs("zoom in", &zooms).expect("zoom in names one operation");
            assert_eq!(chosen.address().action, "zoom-in");
            let (chosen, _) = from_verbs("zoom out please", &zooms).expect("so does zoom out");
            assert_eq!(chosen.address().action, "zoom-out");
            assert!(
                matches!(from_verbs("zoom", &zooms), Err(NoMatch::Ambiguous(_))),
                "zoom alone is still a question"
            );

            // Nor is it dropped when the split would leave nothing: two tied
            // operations that both *want* a value are still ambiguous when the
            // instruction supplied none — narrowing must give up and leave the
            // original tie standing rather than choose from an empty set.
            let both_prompted = vec![
                offer(
                    "photos",
                    "photos.image:1",
                    "crop-left-half",
                    "Crop the left half",
                    tilo_actions::InputSpec::Text {
                        prompt: "Which half".to_string(),
                        placeholder: String::new(),
                        collects: tilo_actions::Collects::Words,
                    },
                ),
                offer(
                    "photos",
                    "photos.image:1",
                    "crop-right-half",
                    "Crop the right half",
                    tilo_actions::InputSpec::Text {
                        prompt: "Which half".to_string(),
                        placeholder: String::new(),
                        collects: tilo_actions::Collects::Words,
                    },
                ),
            ];
            match from_verbs("crop the photograph", &both_prompted) {
                Err(NoMatch::Ambiguous(labels)) => assert_eq!(labels.len(), 2, "{labels:?}"),
                other => panic!(
                    "with no value supplied and both operations wanting one, the \
                     tie must stand rather than pick from nothing: {other:?}"
                ),
            }
        }
    }

    fn offer(
        app: &'static str,
        resource: &str,
        action: &str,
        label: &str,
        input: tilo_actions::InputSpec,
    ) -> Offer {
        let params = matches!(&input, tilo_actions::InputSpec::Text { .. })
            .then(|| {
                vec![tilo_actions::ActionParam::required(
                    "value",
                    tilo_actions::ParamType::Text,
                    "value",
                )]
            })
            .unwrap_or_default();
        Offer {
            app: app.to_string(),
            broker_peer_id: None,
            broker_provider: None,
            descriptor: ActionDescriptor {
                semantic_identity: None,
                address: ActionAddress::new(resource, action),
                resource_kind: format!("{app}.thing"),
                resource_label: "a thing".to_string(),
                action_label: label.to_string(),
                description: None,
                locus: tilo_actions::ExecutionLocus::ClientLocal,
                kind: tilo_actions::OperationKind::Document,
                contract: ActionContract::default(),
                params,
                input,
                view: None,
            },
        }
    }
}
