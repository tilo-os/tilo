//! What a person keeps of a window and a workspace of their own: the view state they
//! picked in views, and what they have seen, as the runtime holds it for them.
//!
//! A window is given the person's entries of the workspace's record of them when it opens, and
//! what they change here is what the host keeps back into it ([`super::personal`]).

use super::Instance;

impl Instance {
    /// What the person has seen, as their entries of the workspace's record keep it: per
    /// resource the newest version they saw (`None` takes a mark back), and the runs they
    /// reviewed. What `unseen(r)` and the marks the host draws read; each view that read one
    /// runs again.
    pub fn set_seen(
        &mut self,
        marks: impl IntoIterator<Item = (String, Option<crate::runtime::seen::SeenMark>)>,
        runs: &[String],
    ) {
        let Some(host) = self.host() else {
            return;
        };
        let person = host.viewer().0.clone();
        let runtime = host.runtime();
        for (resource, mark) in marks {
            runtime.set_seen(&person, &resource, mark);
        }
        runtime.set_seen_runs(&person, runs);
    }

    /// What the person marked seen in this window since last asked (opening, reviewing,
    /// a mark's Mark seen), for the host to keep in the record with the system's
    /// `mark seen` action.
    pub fn take_seen(&mut self) -> Vec<crate::runtime::seen::SeenWrite> {
        self.host()
            .map(|host| host.runtime().take_seen())
            .unwrap_or_default()
    }

    /// Give the runtime the view state the person's entries of the record keep: the
    /// place they picked in an app shows again, on this device and on theirs.
    pub fn set_view_states(
        &mut self,
        slots: impl IntoIterator<
            Item = (
                crate::runtime::view_state::StateKey,
                crate::runtime::value::Value,
            ),
        >,
    ) -> bool {
        let Some(host) = self.host() else {
            return false;
        };
        let runtime = host.runtime();
        let mut changed = false;
        for (key, value) in slots {
            changed |= runtime.set_view_state(key, value);
        }
        changed
    }

    /// What the person changed of the view state since last asked, for the host to
    /// keep in the record.
    pub fn take_view_writes(
        &mut self,
    ) -> Vec<(
        crate::runtime::view_state::StateKey,
        crate::runtime::value::Value,
    )> {
        self.host()
            .map(|host| host.runtime().take_view_writes())
            .unwrap_or_default()
    }
}
