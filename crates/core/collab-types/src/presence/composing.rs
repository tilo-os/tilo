//! Who is composing a message where: a conversation's "… is typing", as every reader of the
//! workspace's presence folds it from the `Composing` words it hears. A window drawing a
//! conversation and an app mirroring it elsewhere read it the same way, so both name the same
//! people typing and stop naming them at the same moment.

use std::collections::HashMap;
use std::time::Duration;

use tilo_member_types::MemberId;
use tilo_presence::PresenceKey;

/// How long a member stays read as composing after their last word: a draft under way is said
/// again as they type, so one who stops drops out shortly after, with no message, and a window
/// that vanished leaves nobody typing for long.
pub const COMPOSING_LINGER: Duration = Duration::from_secs(6);

/// A reader's clock: an instant, and the one `by` after it (the last one it holds, past that).
pub trait Clock: Copy + Ord {
    #[must_use]
    fn later(self, by: Duration) -> Self;
}

impl Clock for std::time::Instant {
    fn later(self, by: Duration) -> Self {
        self.checked_add(by).unwrap_or(self)
    }
}

/// A page's clock, which is its own type in a browser.
#[cfg(target_family = "wasm")]
impl Clock for web_time::Instant {
    fn later(self, by: Duration) -> Self {
        self.checked_add(by).unwrap_or(self)
    }
}

/// Time since a reader's start, as a test keeps it.
impl Clock for Duration {
    fn later(self, by: Duration) -> Self {
        self.saturating_add(by)
    }
}

/// Who is composing a message under which key, and since when, at the clock `I` its reader keeps.
#[derive(Clone, Debug)]
pub struct Composers<I> {
    since: HashMap<(PresenceKey, MemberId), I>,
}

impl<I> Default for Composers<I> {
    fn default() -> Self {
        Self {
            since: HashMap::new(),
        }
    }
}

impl<I: Clock> Composers<I> {
    /// `member` said at `now` whether they compose under `key`: a stop clears them at once.
    pub fn heard(&mut self, key: PresenceKey, member: MemberId, composing: bool, now: I) {
        if composing {
            self.since.insert((key, member), now);
        } else {
            self.since.remove(&(key, member));
        }
    }

    /// `member` left `key`: they compose there no more.
    pub fn left(&mut self, key: &PresenceKey, member: &MemberId) {
        self.since
            .retain(|(at, who), _| !(at == key && who == member));
    }

    /// Who composes under `key` at `now`, sorted. A composer quiet for [`COMPOSING_LINGER`]
    /// without saying they stopped is none.
    #[must_use]
    pub fn at(&self, key: &PresenceKey, now: I) -> Vec<MemberId> {
        self.matching(|at| at == key, now)
    }

    /// Who composes in the resource `id` at `now`, whichever app said it there, sorted.
    #[must_use]
    pub fn at_resource(&self, id: &str, now: I) -> Vec<MemberId> {
        self.matching(
            |at| matches!(at, PresenceKey::Resource { id: at, .. } if at == id),
            now,
        )
    }

    /// When the first composer gone quiet drops out: what a reader that shows them wakes for.
    #[must_use]
    pub fn ends(&self) -> Option<I> {
        self.since
            .values()
            .min()
            .map(|since| since.later(COMPOSING_LINGER))
    }

    /// Forget the composers gone quiet by `now`.
    pub fn prune(&mut self, now: I) {
        self.since
            .retain(|_, since| now < since.later(COMPOSING_LINGER));
    }

    /// Each key and member composing there, quiet or not, as last pruned.
    pub fn keys(&self) -> impl Iterator<Item = &(PresenceKey, MemberId)> {
        self.since.keys()
    }

    fn matching(&self, key: impl Fn(&PresenceKey) -> bool, now: I) -> Vec<MemberId> {
        let mut found: Vec<MemberId> = self
            .since
            .iter()
            .filter(|((at, _), since)| key(at) && now < since.later(COMPOSING_LINGER))
            .map(|((_, who), _)| who.clone())
            .collect();
        found.sort();
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversation(app: &str, id: &str) -> PresenceKey {
        PresenceKey::resource(app, id, "TextChannel")
    }

    /// A composer reads as composing until they stop or go quiet for the linger, wherever the
    /// app that said it, and a stop is read at once.
    #[test]
    fn a_composer_reads_as_composing_until_they_stop_or_go_quiet() {
        let ada = || MemberId::person("ada");
        let bo = || MemberId::person("bo");
        let mut composers = Composers::<Duration>::default();
        composers.heard(conversation("chat", "c1"), ada(), true, Duration::ZERO);
        composers.heard(
            conversation("other", "c1"),
            bo(),
            true,
            Duration::from_secs(1),
        );
        assert_eq!(
            composers.at_resource("c1", Duration::from_secs(2)),
            vec![ada(), bo()],
            "both compose in c1, whichever app said it"
        );
        assert_eq!(
            composers.at(&conversation("chat", "c1"), Duration::ZERO),
            vec![ada()]
        );
        assert!(composers.at_resource("c2", Duration::ZERO).is_empty());
        assert_eq!(composers.ends(), Some(COMPOSING_LINGER));
        assert_eq!(
            composers.at_resource("c1", COMPOSING_LINGER),
            vec![bo()],
            "Ada went quiet for the linger"
        );
        composers.heard(
            conversation("other", "c1"),
            bo(),
            false,
            Duration::from_secs(3),
        );
        assert!(
            composers.at_resource("c1", COMPOSING_LINGER).is_empty(),
            "Bo stopped"
        );
        composers.prune(COMPOSING_LINGER);
        assert_eq!(composers.keys().count(), 0, "nobody is left to wake for");
    }
}
