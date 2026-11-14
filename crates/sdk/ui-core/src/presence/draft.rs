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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::MemberId;
    use crate::presence::PresenceCtx;
    use crate::time::Duration;

    fn channel() -> PresenceKey {
        PresenceKey::resource("so.tilo.chat", "channel-1", "TextChannel")
    }

    /// Ada types into the channel a key each 250 ms for `typed`, then sends her draft or leaves
    /// it, and Bo's window takes what her window says: whether Bo reads her typing, each 100 ms
    /// for 30 s, the clock driven by the test.
    fn read_by_bo(typed: Duration, sent: bool) -> Vec<(Duration, bool)> {
        let every = Duration::from_millis(250);
        let start = crate::time::now();
        let ada = MemberId::person("ada");
        let mut bo = PresenceCtx::default();
        let mut said = DraftSaid::default();
        let mut revision = 0;
        let mut next_key = Duration::ZERO;
        let mut read = Vec::new();
        let mut at = Duration::ZERO;
        while at <= Duration::from_secs(30) {
            let now = start + at;
            if at < typed && at >= next_key {
                revision += 1;
                next_key += every;
            }
            let draft = (at < typed || !sent).then(|| (channel(), revision));
            if let Some((key, payload)) = said.look(draft, now) {
                bo.apply_remote_presence_at(ada.clone(), key, payload, now);
            }
            bo.prune_composing(now);
            read.push((at, bo.composing_at(&channel(), now).contains(&ada)));
            at += Duration::from_millis(100);
        }
        read
    }

    #[test]
    fn someone_typing_reads_as_typing_until_they_send_or_stop() {
        let typed = Duration::from_secs(20);
        let kept = read_by_bo(typed, false);
        assert!(
            kept.iter().all(|(at, typing)| *at >= typed || *typing),
            "typing for 20 s reads as typing throughout"
        );
        let last_key = typed - Duration::from_millis(250);
        assert!(
            kept.iter()
                .all(|(at, typing)| !*typing || *at < last_key + COMPOSING_LINGER),
            "a draft left standing stops reading as typing within the linger"
        );
        let sent = read_by_bo(typed, true);
        assert!(
            sent.iter().all(|(at, typing)| (*at < typed) == *typing),
            "typing until sent, and not a moment after"
        );
    }
}
