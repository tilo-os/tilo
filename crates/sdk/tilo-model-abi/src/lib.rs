//! The imports a Rust app's model module calls and the exports it answers: the document
//! host's ABI.
//!
//! A Rust app's model and verbs run in the session's document host as a WebAssembly module
//! (`docs/architecture/apps.md` §2), the same kind of module a tilolang package runs as, with
//! no imports beyond the host's. This crate is those imports and exports, named once.
//!
//! The module (`tilo-model-guest`) and the host that runs it (`tilo-model-host`) build apart —
//! one for `wasm32`, the other into the session — so this crate has no dependencies, and
//! neither side can drift from the other.
//!
//! # Crossing the boundary
//!
//! Everything crosses as JSON in the module's own memory, so neither side sees the other's
//! Rust types, and no pointer outlives the call it was made in:
//!
//! - The host writes a request into the module's memory: the operation, the record, the
//!   document, the action and the invoker.
//! - The module publishes its answer by writing JSON where the host reads it, or refuses the
//!   call with a JSON message the host reports as the door's answer.
//! - What a module cannot reach is what it does not import: there is no clock, no random
//!   source, no file and no network in [`IMPORTS`]. Entropy is the host's, which is what makes
//!   a new document's actor id and the ids an action mints reproducible when the host replays
//!   the same request.

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
    // it wrote, or a negated error code.
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
/// The call takes no arguments, and answers nothing: what it has to say, it publishes with the
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

/// The size a module's buffer starts at when it pulls from the host: enough for a small
/// record's reading, and the host answers with the bigger size a larger one needs.
pub const BUFFER_START: usize = 8192;

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
