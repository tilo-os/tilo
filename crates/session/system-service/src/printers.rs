//! The `printers` resource: what [`tilo_system_resources::printers`] says it is, published
//! with the actions of [`PrintAction`] and marked unavailable where this computer cannot print.
//!
//! The machine side is [`crate::device::System`]'s (`System::printers`, `System::print`, and
//! `System::find_printers`), so the reading is what CUPS and SANE answered the last time the
//! service read them; a change on the printer bus makes the service read again.

use tilo_actions::ResourceSpec;
use tilo_resource::Effect as _;
use tilo_service::{offer, unavailable};
use tilo_system_resources::printers::{PrintAction, Printers, PRINTERS};

use crate::i18n::name;

/// The `printers` resource, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.system.printers", label = "Printers")]
pub struct PrintersReading;

/// The `printers` resource reading `printers`.
#[must_use]
pub fn resource(printers: &Printers) -> ResourceSpec {
    let mut resource = offer::<PrintAction>(tilo_resource::typed_resource::<PrintersReading>(
        PRINTERS,
        name("so.tilo.system.printers", "Printers"),
    ));
    if !printers.serving {
        for action in [
            "find",
            "add",
            "remove",
            "set-default",
            "set-paused",
            "cancel-job",
            "retry-job",
        ] {
            unavailable(
                &mut resource,
                action,
                "no_printing",
                "This computer has no printer service",
            );
        }
    }
    resource
}
