//! The imports a Rust app's model module calls and the exports it answers, with the JSON the
//! two sides exchange: the document host's ABI.
//!
//! A Rust app's model and verbs run in the session's document host as a WebAssembly module
//! (`docs/architecture/apps.md` §2), the same kind of module a tilolang package runs as, with
//! no imports beyond the host's. This crate is those imports, the export's name, and the
//! request and answer they carry, named once.
//!
//! The module (`tilo-model-guest`) and the host that runs it (`tilo-model-host`) build apart —
//! one for `wasm32`, the other into the session — so this crate is the one place the two
//! agree, and neither can drift from the other.
//!
//! # Crossing the boundary
//!
//! Everything crosses as JSON in the module's own memory, so neither side sees the other's
//! Rust types and no pointer outlives the call it was made in:
//!
//! - The host writes a [`Request`] into the module's memory: the operation, the record, the
//!   document the host opened, the action it admits, and the invoker and time it decided.
//! - The module answers an [`Answer`]: the document as the model left it, and what the record
//!   now offers and reads. A call the model refused is a [`Refusal`], which the host reports
//!   as the door's answer.
//! - What a module cannot reach is what it does not import. There is no clock, no random
//!   source, no file and no network in [`IMPORTS`]: entropy is the host's, which is what makes
//!   a new document's actor id and the ids an action mints reproducible when the host replays
//!   the same request.
//!
//! # Why the tree is data
//!
//! What a record offers (`Answer::offers`) and what it reads (`Answer::reading`) travel as the
//! JSON forms below rather than as the host's own spec type. That is what keeps the host
//! authoritative: it rebuilds the tree from this data with the verbs naming the record's
//! address, and it checks every change the module made against the one change it asked for,
//! before it keeps anything. A module that answers a tree promising more than it may, or a
//! document some other change made, is refused here — not bounded later.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};

/// The module name every import lives in.
pub const MODULE: &str = "tilo_model";

/// Every import the module calls, as the engine links it: module, name, parameters, result.
/// A host that runs a model has these linked beside the `tl` table
/// (`tilolang_runtime::engine::Host::guest_abi`).
///
/// Signatures are strings of `i` (i32), `l` (i64) and `f` (f64): parameters, then the result.
pub const IMPORTS: &[(&str, &str, &str, &str)] = &[
    // The request the host made, as JSON. Answers the number of bytes written to the buffer
    // the module offered, or the negated number it needs when the buffer was too small.
    (MODULE, "request", "ii", "i"),
    // `count` bytes of the host's entropy, written where the module points; answers how many
    // it wrote.
    (MODULE, "entropy", "ii", "i"),
    // This call's answer, as JSON bytes.
    (MODULE, "answer", "ii", ""),
    // This call's refusal, as JSON bytes.
    (MODULE, "fail", "ii", ""),
    // What a person reading the host's log should see.
    (MODULE, "log", "ii", ""),
];

/// The export a model module answers for the record of the kind `kind`: `record_`, then each
/// character of the kind that is a letter, a digit or `_`, and `_` for every other — so
/// `tasks.tracker` is `record_tasks_tracker`.
///
/// The call takes no arguments and answers nothing: what it has to say, it publishes with the
/// `answer` import.
#[must_use]
pub fn export(kind: &str) -> String {
    let mut name = String::with_capacity("record_".len() + kind.len());
    name.push_str("record_");
    for character in kind.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            name.push(character);
        } else {
            name.push('_');
        }
    }
    name
}

/// What the host asks the module to do.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Request {
    /// Which operation this is.
    pub op: Op,
    /// The record's kind (`tasks.tracker`).
    pub kind: String,
    /// The resource address the record is published at.
    pub address: String,
    /// The record's document, saved, as base64: `None` for one that does not exist yet.
    #[serde(default)]
    pub document: Option<String>,
    /// The action the host admits, for [`Op::Invoke`].
    #[serde(default)]
    pub action: Option<Asked>,
    /// What the reading is of: nothing for the record itself, or the id of one of its items.
    #[serde(default)]
    pub item: Option<String>,
    /// Whether `address` names the record itself rather than one of its items.
    #[serde(default)]
    pub record: bool,
    /// Who invokes, as the session decided it: the person, an agent acting for them, or the
    /// app. The module commits the change under it and never chooses it.
    #[serde(default)]
    pub invoker: Option<String>,
    /// The person the invoker acts for, when they are an agent or a bot.
    #[serde(default)]
    pub on_behalf_of: Option<String>,
    /// The time the change is made at, in milliseconds since the Unix epoch, as the session
    /// read it once for this invocation.
    #[serde(default)]
    pub now_ms: i64,
}

/// What the host asks the model to do with the record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    /// Open the record, or write its genesis when it holds none, and answer its document.
    Open,
    /// Bring the record up to the model's version, and answer its document.
    Migrate,
    /// Answer a reading of the record, or of one of its items.
    Read,
    /// Run an action of the record, and answer the document the model left and its reading.
    Invoke,
}

/// An action the host admits, as the door named it.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Asked {
    /// The action's name (`create`).
    pub action: String,
    /// Its arguments, by parameter name.
    #[serde(default)]
    pub arguments: Map<String, Json>,
}

/// What the module answers.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Answer {
    /// The record's document, saved, as base64: what the model left.
    #[serde(default)]
    pub document: Option<String>,
    /// Whether the record was made by this call, having held nothing before.
    #[serde(default)]
    pub created: bool,
    /// What the record offers, for [`Op::Open`] and [`Op::Invoke`].
    #[serde(default)]
    pub offers: Option<ResourceNode>,
    /// The reading, for [`Op::Read`] and after [`Op::Invoke`].
    #[serde(default)]
    pub reading: Option<Json>,
}

/// Why the module refused a call: what the host reports as the door's answer.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Refusal {
    /// What kind of refusal it is.
    pub kind: RefusalKind,
    /// What it says, in words a person reads.
    pub message: String,
}

/// The kinds of refusal a model answers with, as the door names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalKind {
    /// The record does not read as the model.
    Model,
    /// The door refused the action: no such action, bad arguments, or the verb's own refusal.
    Door,
    /// The call named an operation or a record the module does not serve.
    Unknown,
}

/// A resource the record publishes, as data: the host turns it into the tree its door offers.
///
/// The host never takes a spec from the module: it reads this and builds the tree itself, with
/// every verb naming the record's address, so a module cannot promise a caller more than the
/// host may offer them.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ResourceNode {
    /// The resource address (`tasks.tracker`, or one of its items).
    pub id: String,
    /// Its kind (`tasks.tracker`).
    pub kind: String,
    /// Its label, as people read it.
    pub label: String,
    /// What each action of it is, by name.
    #[serde(default)]
    pub actions: Vec<ActionNode>,
    /// The items it collects.
    #[serde(default)]
    pub children: Vec<ResourceNode>,
    /// What the resource is called as a type, when it declares one.
    #[serde(default)]
    pub resource_type: Option<TypeNode>,
}

/// One action a resource offers, as data.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ActionNode {
    /// The action's id (`create`).
    pub id: String,
    /// Its label, as people read it.
    pub label: String,
    /// The other words people use for it.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Its picture, by its Lucide name.
    #[serde(default)]
    pub icon: Option<String>,
    /// What it does, in a sentence.
    #[serde(default)]
    pub description: Option<String>,
    /// Its parameters, by name.
    #[serde(default)]
    pub params: Vec<ParamNode>,
    /// The state it shows in its row, if it shows one.
    #[serde(default)]
    pub state: Option<StateNode>,
    /// Its audience: `people`, `agents` or `all` (the default).
    #[serde(default)]
    pub audience: Option<String>,
    /// Whether it takes something away.
    #[serde(default)]
    pub destructive: bool,
    /// Why it cannot be invoked now, when it cannot.
    #[serde(default)]
    pub unavailable: Option<String>,
}

/// One parameter of an action, as data.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ParamNode {
    /// Its name, as arguments name it.
    pub name: String,
    /// Its type: `text`, `number`, `integer`, `bool`, `id`, `place`, `choice` or `content`.
    pub kind: String,
    /// What it is, in words.
    #[serde(default)]
    pub description: Option<String>,
    /// Whether the action may be called without it.
    #[serde(default)]
    pub optional: bool,
}

/// The state an action's row shows, as data.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StateNode {
    /// A short badge ("On", "Current").
    #[serde(default)]
    pub badge: Option<String>,
    /// Whether invoking it flips the state the badge shows.
    #[serde(default)]
    pub toggles: bool,
}

/// What a resource is called as a type, as data: what search matches a query naming what
/// something is ("my mails") against.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct TypeNode {
    /// Its type: what one is called, and what several are.
    pub noun: NounNode,
    /// Its app, as people call it.
    pub app: NounNode,
    /// Whether it is its app's root.
    pub root: bool,
}

/// What one of something is called and what several are (`mailbox`, `mailboxes`).
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct NounNode {
    /// What one is called.
    pub one: String,
    /// What several are called.
    pub many: String,
}

/// A reading's own shape, carried beside it so a host can tell a record's from an item's.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Reading {
    /// The resource the reading is of.
    pub id: String,
    /// The fields, by name.
    #[serde(default)]
    pub fields: Json,
    /// The ids of the items it collects, each its own reading.
    #[serde(default)]
    pub parts: Option<Vec<String>>,
}

/// The action type every verb a model serves is encoded under: one action whose payload names
/// the resource and the verb, so the host runs the module's own verb rather than a closure of
/// its own. A caller never sees it — the host resolves the address against what the record
/// publishes, and what it hands the module is the verb's name and arguments.
pub const VERB_TYPE: &str = "tilo.model.verb";

/// The size a module's buffer starts at when it pulls from the host: enough for a small
/// record's document, and the host answers with the bigger size a larger one needs.
pub const BUFFER_START: usize = 64 * 1024;

#[cfg(test)]
mod tests {
    use super::{IMPORTS, export};

    #[test]
    fn a_kind_names_its_export() {
        assert_eq!(export("tasks.tracker"), "record_tasks_tracker");
        assert_eq!(export("tube.watch-history"), "record_tube_watch_history");
        assert_eq!(export("probe.note"), "record_probe_note");
    }

    #[test]
    fn every_import_names_this_module() {
        for (module, name, _, _) in IMPORTS {
            assert_eq!(*module, super::MODULE, "{name} is in another module");
        }
    }
}
