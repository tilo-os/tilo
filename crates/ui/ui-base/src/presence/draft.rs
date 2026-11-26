//! What a window says of the draft its person writes, so a conversation on every other window
//! says "… is typing" while they type and stops shortly after they stop.
//!
//! A reader drops a composer it heard nothing from for [`COMPOSING_LINGER`], so a window whose
//! person vanished leaves no one typing. A person who keeps typing is therefore said again, on a
//! keystroke, once half of that time passed since they were last said: nothing is said while the
//! draft stands still, and a stop (the draft sent or emptied) is said at once.

use super::{COMPOSING_LINGER, PresenceKey};
use crate::collaboration::PresencePayload;
use crate::time::Instant;

/// What was last said of the person's draft: where, as the draft stood on the last look, and when.
#[derive(Debug, Default)]
pub struct DraftSaid {
    said: Option<Said>,
}

#[derive(Debug)]
struct Said {
    key: PresenceKey,
    /// The draft's revision on the last look: a new one is a keystroke.
    seen: u64,
    at: Instant,
}

impl DraftSaid {
    /// What to say now that the draft is `draft` at `now`: the resource it is written in and any
    /// number that changes as the person types in it, or `None` with no draft. A start or a move
    /// to another resource is said at once, a stop under the key the draft was under (or nobody
    /// could clear it), and a draft that changed is said again once half of
    /// [`COMPOSING_LINGER`] passed since it was last said.
    pub fn look(
        &mut self,
        draft: Option<(PresenceKey, u64)>,
        now: Instant,
    ) -> Option<(PresenceKey, PresencePayload)> {
        let Some((key, revision)) = draft else {
            let said = self.said.take()?;
            return Some((said.key, PresencePayload::Composing { composing: false }));
        };
        if let Some(said) = self.said.as_mut().filter(|said| said.key == key) {
            let typed = std::mem::replace(&mut said.seen, revision) != revision;
            if !typed || now.saturating_duration_since(said.at) < COMPOSING_LINGER / 2 {
                return None;
            }
            said.at = now;
        } else {
            self.said = Some(Said {
                key: key.clone(),
                seen: revision,
                at: now,
            });
        }
        Some((key, PresencePayload::Composing { composing: true }))
    }
}
