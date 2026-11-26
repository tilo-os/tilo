//! The records of an app's process, read and written over its own connection to its device
//! ([`super::RemoteActionMessage::StoreRequest`]).

use super::{ActionCallError, Caller, Pending};

/// What a provider that serves no app's process answers a store command.
pub const STORE_COMMANDS_UNAVAILABLE: &str =
    "this connection reaches no app's records: only an app's own connection to its device does";

/// The device's one store, as the process of one app reaches its records.
pub trait StoreCommands: Send + Sync {
    /// Answer `command` (`collab_types::store::wire::Command`, as JSON) from `caller` with the
    /// store's `Reply`, as JSON. Called in the order a connection's commands arrive, so a change
    /// sent as a run of parts is taken in that order; the answer may come later. A record the
    /// command founds is of `kind`.
    fn command(
        &self,
        caller: &Caller,
        command: serde_json::Value,
        kind: Option<String>,
    ) -> Pending<Result<serde_json::Value, ActionCallError>>;
}
