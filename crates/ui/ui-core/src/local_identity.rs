//! Who sits at this device, as far as undo attribution and history authors are concerned: the
//! process's one identity boundary (`tilo_local_identity`), which a host installs a source into.
//! Without one, and in any toolkit built without Tilo's model, there is no local identity.

pub use tilo_local_identity::*;

/// The colour this person chose to wear everywhere, kept in their settings.
#[cfg(feature = "model")]
pub use ::collaboration::local_identity::own_colour;
