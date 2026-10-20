//! The wasm host for tilolang apps.
//!
//! The pieces, in the order a call meets them:
//!
//! * [`engine`]: compiles and runs wasm; the only module that names wasmi.
//! * [`handles`] and [`abi`]: the handle table and the `"tl"` import table.
//! * `core` and its `host_*`/`exec` parts: every import, the action transaction, the flow
//!   journal and the view run.
//! * [`store`] and [`amstore`]: where resources live, and the Automerge reference store.
//! * [`authority`]: who may invoke what.
//! * [`view`] and [`view_instance`]: UI trees, the keyed diff and live views.
//! * [`runtime`]: the public API.

pub mod abi;
pub mod amstore;
pub mod authority;
pub mod catalogue;
mod core;
mod embed;
pub mod engine;
pub mod error;
mod exec;
pub mod flow;
pub mod handles;
mod host_res;
mod host_ui;
mod host_values;
pub mod ids;
pub mod meta;
mod num;
pub mod project;
pub mod registry;
mod render;
pub mod runtime;
mod settle;
pub mod space;
pub mod store;
pub mod textdiff;
pub mod value;
pub mod view;
pub mod view_instance;

pub use amstore::AmStore;
pub use authority::{ActionInfo, Authority, Decision, SimpleAuthority};
pub use catalogue::{Signature, SignatureKind, catalogue};
pub use core::{ModelHost, NameHost, OutsideSink};
pub use engine::EngineLimits;
pub use error::RuntimeError;
pub use exec::InvokeOutcome;
pub use flow::{FlowOutcome, Journal, JournalEntry, Resolution, Suggestion};
pub use ids::{ModuleHash, PackageId, Principal};
pub use meta::Meta;
pub use project::ProjectOptions;
pub use render::ViewTarget;
pub use runtime::{BlockOutput, MemorySink, Runtime, SettleReport};
pub use settle::{AgentBlock, InvocationRecord};
pub use space::Space;
pub use store::{ChangeNote, ChangeSummary, Store, StoreError};
pub use value::{ActionRef, RefValue, ResourceRef, Value};
pub use view::{Binding, Patch, UiNode};
pub use view_instance::{EditOutcome, ViewInstance};
