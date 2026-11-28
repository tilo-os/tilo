//! Who this person is to the people they work with: the colour they wear
//! everywhere (`identity.colour`, a presence hue id), read through
//! `ui::tokens::colors::PresenceHue::chosen_in`.

use crate::Key;

pub const SECTION: &str = "identity";

pub const COLOUR: Key<Option<String>> = Key::new(SECTION, "colour", || None);

/// Whether the person's identity is backed up: `Some(false)` once they left the backup for later,
/// `Some(true)` once a backup was verified, `None` before setup asked.
pub const BACKED_UP: Key<Option<bool>> = Key::new(SECTION, "backed_up", || None);
/// Whether the system reminds them of a backup they left for later.
pub const BACKUP_REMINDERS: Key<bool> = Key::new(SECTION, "backup_reminders", || true);
/// When they were last reminded of it, in seconds since the epoch.
pub const BACKUP_REMINDED: Key<Option<u64>> = Key::new(SECTION, "backup_reminded", || None);
