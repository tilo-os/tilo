//! An app's records read without its window: what the system's search index
//! asks of an app that has not opened the record.
//!
//! Search is a record's to answer, not a window's: a message, a conversation
//! or an agent is found on a device that never ran the app, because whoever
//! keeps the record reads it here and publishes what it holds to the same
//! index a running window publishes to. The reader is the app's own binary in
//! its `--index` mode: it links the app's model types, so what it derives is
//! exactly what the app's window would, and the session that asks links none
//! of them (`docs/architecture/apps.md`).
//!
//! The document arrives on standard input and leaves as the entries on
//! standard output, as JSON: no path, no store and no window is handed in, so
//! a reader cannot reach anything but the bytes it was asked about.

use std::io::{Read as _, Write as _};

use tilo_resource::{Doc, Kinds};
use ui_core::search::index_publication::IndexEntry;

use super::app::ResourceApp;
use super::kinds::resource_kind;

/// Every kind `A` declares, hosted by `A`: its resource, and the parts of the
/// chains it keeps beside its record.
///
/// The same set [`super::kinds::register`] hosts for the process, built
/// without depending on a definition having been created: a host that reads a
/// record of an app that is not running reads it by these.
fn kinds_of<A: ResourceApp>() -> Kinds {
    let mut declared = super::Records::declaring();
    A::records(&mut declared);
    let mut kinds = Kinds::new();
    kinds.insert(resource_kind::<A>());
    for chain in declared.into_chains() {
        kinds.insert(chain.hosted_by(A::ID, A::TITLE));
    }
    kinds
}

/// The flag that makes an app's binary print what a record holds for search
/// instead of running: its document is read from standard input.
pub const INDEX_FLAG: &str = "--index";

/// What search finds in `doc`, as the index keeps it: every kind of `A`'s
/// that the document holds, read the way its window reads the record.
///
/// A record kept before the app changed its storage is read at this build's
/// version, in memory: the document itself is the app's to migrate.
#[must_use]
pub fn entries<A: ResourceApp>(doc: &Doc) -> Vec<IndexEntry> {
    let mut found = Vec::new();
    let mut keys = std::collections::BTreeSet::new();
    for kind in kinds_of::<A>().iter() {
        let mut read = doc.clone();
        if kind.needs_migrate(&read) && kind.migrate(&mut read).is_err() {
            continue;
        }
        let Ok(entries) = kind.search(&read) else {
            // The document holds another kind: its own type answers for it.
            continue;
        };
        for entry in entries {
            let Some(entry) = super::root::index_entry(&entry).and_then(IndexEntry::bounded) else {
                continue;
            };
            if keys.insert(entry.key.clone()) {
                found.push(entry);
            }
        }
    }
    found
}

/// Answer the `--index` mode when the command line asks for it: read the
/// record's document from standard input and print its entries. `None` for
/// a command line that starts the app as usual.
pub fn serve<A: ResourceApp>() -> Option<i32> {
    let asked = std::env::args().skip(1).any(|arg| arg == INDEX_FLAG);
    if !asked {
        return None;
    }
    #[allow(
        clippy::exit,
        reason = "the record reader is the whole run of the process"
    )]
    Some(match ask::<A>() {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Error: {error}");
            1
        }
    })
}

/// [`serve`] once it is known to be asked: the document in, the entries out.
fn ask<A: ResourceApp>() -> Result<(), String> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .read_to_end(&mut bytes)
        .map_err(|error| format!("the record is not read: {error}"))?;
    let doc =
        Doc::load(&bytes).map_err(|error| format!("the record is not a document: {error}"))?;
    let entries = entries::<A>(&doc);
    let mut out = Vec::new();
    serde_json::to_writer(&mut out, &entries)
        .map_err(|error| format!("the entries are not written: {error}"))?;
    std::io::stdout()
        .lock()
        .write_all(&out)
        .map_err(|error| format!("the entries do not leave: {error}"))
}
