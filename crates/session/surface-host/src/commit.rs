//! Committing what a part's runtime planned to the record its views read.

use tilolang_tilo::DynAction;
use tilolang_tilo::host::Host;
use ui::tilo_actions::{ActionContext, ActionInvoker, apply_local_with_context};

/// Commit `action` to `record` as the door would: one change, which records the action it is and
/// whom it was made for, as a record's service commits one.
///
/// The store that planned the action keeps its own write of it until a change of the record
/// carries the edit, and writes it onto the record's copy again after every change that does not.
/// A change that records no action would leave that write standing over whatever is written to
/// the record after it by anyone else: a REPL's or an agent's change, or one from another of the
/// person's devices, would never show.
///
/// The change is made under a writer of its own: the session's record service writes the same
/// record on its own copy, and two copies never share a writer.
pub(crate) fn recorded(
    host: &Host,
    record: &mut tilo_resource::Doc,
    action: &DynAction,
) -> Result<(), String> {
    let encoded = action.encode().map_err(|error| error.to_string())?;
    // Whom the action was made for: the person or agent its authorship names, or its app acting
    // by itself when it names nobody.
    let invoker = if action.authorship.invoker.is_empty() {
        format!("{}{}", tilolang_tilo::authority::TRIGGER_PREFIX, action.app)
    } else {
        action.authorship.invoker.clone()
    };
    let context = ActionContext::for_resource(
        // The workspace root's path, for an action that wrote no resource of its own.
        action.resources().first().copied().unwrap_or("room"),
        &action.kind,
        &action.kind,
    )
    .with_invoker(ActionInvoker::actor(invoker));
    host.with_schemas(|| apply_local_with_context(record, action, encoded, None, context))
        .map(|_| ())
        .map_err(|error| error.to_string())
}
