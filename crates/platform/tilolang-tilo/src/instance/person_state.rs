//! What a person keeps of a window and a workspace of their own: the view state they
//! picked in views, and what they have seen, as the runtime holds it for them.
//!
//! A window is given the person's entries of the workspace's record of them when it opens, and
//! what they change here is what the host keeps back into it ([`super::personal`]).

use super::Instance;
use crate::personal::{self, Keep, PersonalState};
use crate::runtime::seen::SeenWrite;
use crate::runtime::view_state::StateKey;

impl Instance {
    /// Give the views what `held`, the workspace's record of each person's own state, keeps of
    /// the person at the window `tab` (the shell's tab it takes): how this window shows their
    /// resources, where they are in it, and what they have seen. Another window's place is its
    /// own. A slot already holding its value is left alone, so a host may run this every frame.
    /// Whether the instance had views to give it to.
    ///
    /// Everyone's marks reach the runtime, not the person looking's alone: a conversation says
    /// who has read to a line, and the line is theirs, not a count the app keeps.
    pub fn bring_personal(&mut self, held: &PersonalState, tab: u64) -> bool {
        let Some(host) = self.host() else {
            return false;
        };
        let runtime = host.runtime();
        let kept = held.of(&host.viewer().0);
        for slot in &kept.slots {
            if let Some(view) = personal::of_window(tab, &slot.window) {
                let key = StateKey::view(view, slot.person.clone(), slot.state.clone());
                runtime.set_view_state(key, slot.value.clone());
            }
        }
        for seen in &held.seen {
            runtime.set_seen(&seen.person, &seen.resource, Some(seen.mark.clone()));
        }
        let runs: Vec<String> = kept.runs.into_iter().map(|reviewed| reviewed.run).collect();
        runtime.set_seen_runs(&host.viewer().0, &runs);
        true
    }

    /// What the person at the window `tab` changed of their own state since last asked, as the
    /// changes that keep it in the workspace's record of it, which holds `held` now: the view
    /// state they set in this window, and what they marked seen. Someone else's mark is not theirs
    /// to keep.
    pub fn take_personal(&mut self, held: &PersonalState, tab: u64) -> Vec<Keep> {
        let Some(host) = self.host() else {
            return Vec::new();
        };
        let runtime = host.runtime();
        let me = host.viewer().0.clone();
        let mut keeps = Vec::new();
        for (key, value) in runtime.take_view_writes() {
            match Keep::view(&key, &value) {
                Ok(Keep::View {
                    person,
                    window,
                    state,
                    value,
                }) => keeps.push(Keep::View {
                    person,
                    window: personal::in_window(tab, &window),
                    state,
                    value,
                }),
                Ok(keep) => keeps.push(keep),
                Err(why) => tracing::debug!(%why, "the slot is no person's state"),
            }
        }
        let mut runs: Vec<String> = held
            .of(&me)
            .runs
            .into_iter()
            .map(|reviewed| reviewed.run)
            .collect();
        for write in runtime.take_seen() {
            match write {
                SeenWrite::Resource {
                    person,
                    resource,
                    mark,
                } if person.eq_ignore_ascii_case(&me) => keeps.push(Keep::Seen {
                    person: me.clone(),
                    resource,
                    mark,
                }),
                SeenWrite::Run { person, run } if person.eq_ignore_ascii_case(&me) => {
                    if !runs.contains(&run) {
                        runs.push(run);
                        runs.sort();
                    }
                    keeps.push(Keep::Runs {
                        person: me.clone(),
                        runs: runs.clone(),
                    });
                }
                _ => {}
            }
        }
        keeps
    }
}
