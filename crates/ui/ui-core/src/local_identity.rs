//! Who sits at this device, as far as undo attribution and history authors are concerned: the
//! process's one identity boundary (`tilo_local_identity`), which a host installs a source into.
//! Without one, and in any toolkit built without Tilo's model, there is no local identity.
//!
//! With the model, the collab client's `local_identity` is what the toolkit reads: the same
//! boundary, plus the native source that reads the identity service a launcher bound and the
//! person's public photo.

#[cfg(feature = "model")]
pub use ::collaboration::local_identity::*;
#[cfg(not(feature = "model"))]
pub use tilo_local_identity::*;
