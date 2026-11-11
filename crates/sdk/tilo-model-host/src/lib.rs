//! A Rust app's model and verbs, run in the session's document host.
//!
//! `docs/architecture/apps.md` §1–§2 gives the document host every document: it is the only
//! thing that opens, decrypts, merges, signs, stores and syncs an app's records, and an app's
//! process holds none of them. An app's model and verbs run there as a WebAssembly module with
//! no imports beyond the host's, and a Rust app's model is the same kinds of types with the
//! same verbs (`docs/designs/app-language/one-app-model.md`).
//!
//! This crate is that host's side of it. [`Module`] loads the app's own module and answers its
//! imports; [`Record`] keeps one record of one resource kind over it:
//!
//! - it holds the document, an Automerge document like any other record, and hands it to the
//!   module for the length of a call;
//! - it builds what the record offers ([`Record::offers`]) from the tree the module answered,
//!   through [`tilo_resource::ResourceKind`]'s shapes with the type erased, with every verb
//!   addressing the resource it is invoked on — so a view is offered the resource and its items,
//!   and a module cannot offer a caller an address the host does not publish;
//! - it offers a reading ([`Record::read`]) of the resource or one of its items, never a
//!   document;
//! - it admits an action over the door ([`Record::invoke`]) — the model's `check` and verb run
//!   in the module, and the change the module made kept only when it is the one change the host
//!   asked for, committed as the invoker's step — so the app process neither runs the verb nor
//!   signs the result.
//!
//! An app's code is about as trustworthy as a random web page's, and this host is privileged.
//! So the module reaches nothing it did not import ([`tilo_model_abi::IMPORTS`]): no clock, no
//! random source, no file, no network. What it is handed is a reading and the record's
//! document; what it answers is JSON the host reads back and checks.
//!
//! No key, transport or identity is anywhere in this crate, and none is given to it: what it
//! signs with, and who an action is recorded as, are the caller's ([`Invoker`]), decided by the
//! session that keeps the record.
//!
//! The session's side over many records is one per app in a workspace,
//! published to the session's door as a service
//! (`tilolang_tilo::records_service::RecordService` is the tilolang
//! package's). An app's connection reads and invokes them by the same
//! addresses it would have used had it kept the record itself.

mod module;
mod record;
pub mod service;
#[cfg(feature = "store")]
mod store;

pub use module::{Module, ModuleError};

/// The model module a host's tests run: `tag`'s model crate (`tasks-model`, `tube-model`, `console-model`) built
/// for `wasm32` by this crate's build script, the same code the app ships.
///
/// `None` when this build could not build it — a machine with no Rust that has the wasm32
/// target — which the build script said when it ran.
#[must_use]
pub fn fixture_module(name: &str) -> Option<Vec<u8>> {
    let dir = env!("TILO_MODEL_FIXTURE_DIR");
    std::fs::read(std::path::Path::new(dir).join(format!("{}.wasm", name.replace('-', "_")))).ok()
}
pub use record::{Invoker, Opened, Record, RecordError};
pub use service::ModelService;
#[cfg(feature = "store")]
pub use store::Kept;
