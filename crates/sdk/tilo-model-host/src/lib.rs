//! A Rust app's model and verbs, run in the session's document host.
//!
//! `docs/architecture/apps.md` §2 gives the document host every document: it
//! is the only thing that opens, decrypts, merges, signs, stores and syncs an
//! app's records, and an app's process holds none of them. A tilolang
//! package's model and verbs run there as a WebAssembly module; a Rust app's
//! model is the same kinds of types with the same verbs
//! (`docs/designs/app-language/one-app-model.md`), so a host that links the
//! crate the model lives in runs the same code the app would have run.
//!
//! This crate is that host. [`Record`] keeps one record of one resource kind,
//! read through [`tilo_resource::ResourceKind`] with the type erased:
//!
//! - it holds the document, an Automerge document like any other record;
//! - it publishes what the record offers ([`Record::offers`]) by reading the
//!   model, so a view is offered the resource and its items and a person
//!   reads what they are granted;
//! - it offers a reading ([`Record::read`]) of the resource or one of its
//!   items, never a document;
//! - it admits an action over the door ([`Record::invoke`]) — the model's
//!   `check`, the verb, and the change recorded as the invoker's step, signed
//!   by whoever keeps the record — so the app process neither runs the verb
//!   nor signs the result.
//!
//! No key, transport or identity is anywhere in this crate, and none is given
//! to it: what it signs with, and who an action is recorded as, are the
//! caller's ([`Invoker`]), decided by the session that keeps the record.
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
pub use record::{Invoker, Opened, Record, RecordError};
pub use service::ModelService;
#[cfg(feature = "store")]
pub use store::Kept;
