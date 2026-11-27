//! The person's presence on the connection an app's launcher handed it.
//!
//! A window says one word of its person at a time (where they point, what they select) and takes
//! what the other members said in its workspace. Both travel on the process's one connection to the
//! device's daemon, which the runner that starts the app installs here before the first frame. The
//! toolkit knows only this seam, so it links nothing of the daemon's transport. A host process
//! (the shell, a test) installs nothing, and its windows say and hear nothing this way.

use std::sync::{Arc, OnceLock};

/// The connection a window's presence words travel on.
pub trait PresenceLine: Send + Sync {
    /// Say one word of this window's person to the device's live presence.
    ///
    /// # Errors
    /// Why the device did not take the word.
    fn say(&self, said: &serde_json::Value) -> Result<(), String>;

    /// What the device's live session heard from the other members since the last call, one
    /// written word each.
    fn heard(&self) -> Vec<String>;
}

static LINE: OnceLock<Arc<dyn PresenceLine>> = OnceLock::new();

/// Install the process's presence line. The first install stands: a process has one connection
/// to its device.
pub fn install(line: Arc<dyn PresenceLine>) {
    let _ = LINE.set(line);
}

/// The presence line the runner installed, if this process has one.
pub fn installed() -> Option<&'static Arc<dyn PresenceLine>> {
    LINE.get()
}
