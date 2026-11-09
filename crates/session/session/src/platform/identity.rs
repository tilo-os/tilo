//! The identity this device signs with, as its host keeps it: the identity files of a Linux
//! desktop, the page's own store on the web.
//!
//! What the keys and certificates are is the identity crates' on every host; where this host
//! keeps them is its own. A host with no identity answers none, and the session takes part in no
//! workspace as anybody until the person sets a device up.

use std::sync::Arc;

use tilo_identity_host::custody::Custody;

/// This device's custody, when this host holds an identity at all.
#[must_use]
pub fn custody() -> Option<Arc<dyn Custody>> {
    #[cfg(target_os = "linux")]
    {
        let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
        Some(Arc::new(tilo_identity_host::custody::files::Files::new(
            paths,
        )))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}
