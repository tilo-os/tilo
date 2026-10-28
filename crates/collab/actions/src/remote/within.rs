//! How far a caller's requests reach when they continue an invocation.

use super::Caller;

/// How far a caller's requests reach without the person being asked: the
/// [`Reach`](tilo_action_policy::Reach) of the invocation they continue, if
/// any. It bounds what the caller may do, not who the caller is: it is
/// equal for every caller, so a child someone's invocation made is still
/// theirs to follow and cancel.
#[derive(Clone, Copy, Debug)]
pub(super) struct Within(Option<tilo_action_policy::Reach>);
impl PartialEq for Within {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for Within {}
impl std::hash::Hash for Within {
    fn hash<H: std::hash::Hasher>(&self, _: &mut H) {}
}

impl Within {
    pub(super) const NONE: Self = Self(None);
}

impl Caller {
    /// The same caller, asking as part of an invocation admitted to reach
    /// `reach`: what goes further runs only once the person confirmed it.
    /// It only narrows. Set only by the host that matched the request to
    /// the invocation it continues, never from a payload.
    #[must_use]
    pub fn within(mut self, reach: tilo_action_policy::Reach) -> Self {
        self.within = Within(Some(
            self.within.0.map_or(reach, |held| held.narrowed(reach)),
        ));
        self
    }

    /// How far this caller's requests reach without the person being asked,
    /// when they continue an invocation ([`Self::within`]).
    #[must_use]
    pub const fn ceiling(&self) -> Option<tilo_action_policy::Reach> {
        self.within.0
    }
}
