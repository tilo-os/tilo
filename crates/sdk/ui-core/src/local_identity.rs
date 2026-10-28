//! Who sits at this device, as far as undo attribution and history authors
//! are concerned: the collab client's identity seam
//! (`collaboration::local_identity`), and the person's photo ready to paint.

pub use ::collaboration::local_identity::*;

/// The local person's own public photo, ready to paint, if they set one:
/// for apps that show the person beside what they wrote (their own mail).
#[must_use]
pub fn own_photo() -> Option<crate::peniko::ImageData> {
    #[cfg(feature = "local-daemons")]
    {
        own_public_photo().map(|photo| crate::widget::avatar::profile_image(&photo))
    }
    #[cfg(not(feature = "local-daemons"))]
    {
        None
    }
}
