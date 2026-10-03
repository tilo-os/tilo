//! Undo and Redo as host actions, so a menu, the keyboard and automation
//! reach the one undo implementation through the same door.

use fluent::FluentArgs;
use tilo_actions::{ActionSpec, Availability, OperationKind, Resource};

use crate::i18n::I18nManager;
use crate::undo::{UndoRequest, UndoStatus};

pub(crate) const EDIT_RESOURCE: &str = "tilo.host:edit";
pub(crate) const EDIT_OPERATION: &str = "tilo.host.edit";

/// Undo and Redo for the local author, titled with the change each would
/// revert and unavailable when there is none.
#[expect(
    clippy::expect_used,
    reason = "fixed unit Edit payloads serialize infallibly"
)]
pub(crate) fn edit_resource(status: &UndoStatus) -> Resource {
    let action = |request: UndoRequest, key: &str| {
        let label = match status.summary(request) {
            Some(change) if change != crate::history_source::NO_VISIBLE_CHANGE => {
                let mut args = FluentArgs::new();
                args.set("change", change.to_owned());
                I18nManager::lookup(&format!("{key}-named"), Some(&args))
            }
            _ => I18nManager::lookup_plain(key),
        };
        let mut spec = ActionSpec::operation(
            request.verb(),
            label,
            OperationKind::Effect,
            EDIT_OPERATION,
            &request,
        )
        .expect("unit Edit operation serializes");
        if !status.can(request) {
            spec.contract.availability = Availability::Unavailable {
                code: "nothing_to_undo".into(),
                reason: status.description(request),
            };
        }
        spec
    };
    Resource::new(
        EDIT_RESOURCE,
        "tilo.host.edit",
        I18nManager::lookup_plain("host-menu-edit"),
    )
    .with_action(action(UndoRequest::Undo, "host-undo"))
    .with_action(action(UndoRequest::Redo, "host-redo"))
}
