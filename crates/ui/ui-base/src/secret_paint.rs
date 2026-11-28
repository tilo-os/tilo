//! Whether a paint put a secret on screen. A secret field that holds a value notes it
//! while it draws, masked or shown.

use std::cell::Cell;

/// What a secret shows in place of its value once its window loses focus: a set of dots that
/// says a value is there without telling it or how long it is.
pub const SECRET_MASK: &str = "••••••••••••";

thread_local! {
    static PAINTED: Cell<bool> = const { Cell::new(false) };
}

/// Record that the paint running on this thread drew a secret.
pub fn note_secret_painted() {
    PAINTED.with(|painted| painted.set(true));
}

/// Watches one paint (a frame, or a cached subtree) for secrets.
#[must_use = "the scope reports only through `finish`"]
pub struct SecretPaint {
    outer: bool,
}

impl SecretPaint {
    pub fn begin() -> Self {
        Self {
            outer: PAINTED.with(|painted| painted.replace(false)),
        }
    }

    /// Whether anything painted since [`Self::begin`] was a secret.
    pub fn finish(self) -> bool {
        PAINTED.with(Cell::get)
    }
}

impl Drop for SecretPaint {
    fn drop(&mut self) {
        let outer = self.outer;
        PAINTED.with(|painted| painted.set(outer || painted.get()));
    }
}
