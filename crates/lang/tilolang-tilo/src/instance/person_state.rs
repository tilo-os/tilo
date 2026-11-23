//! What a person keeps of a window and a workspace of their own: the view state they
//! picked in views, and what they have seen, as the runtime holds it for them.
//!
//! A window is given the person's entries of the workspace's document of them when it opens, and
//! what they change here is what the host keeps back into it ([`super::personal`]).

use super::Instance;
use crate::personal::{self, Keep, PersonalState};
use crate::runtime::seen::SeenWrite;
use crate::runtime::view_state::StateKey;

impl Instance {
    /// Give the views what `held`, the workspace's document of each person's own state, keeps of
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
        let kept = held.of(self.me.as_str());
        for slot in &kept.slots {
            if let Some(view) = personal::of_window(tab, &slot.window) {
                let key = StateKey::view(view, personal::actor(&slot.person), slot.state.clone());
                runtime.set_view_state(key, slot.value.clone());
            }
        }
        for seen in &held.seen {
            runtime.set_seen(
                &personal::actor(&seen.person),
                &seen.resource,
                Some(seen.mark.clone()),
            );
        }
        let runs: Vec<String> = kept.runs.into_iter().map(|reviewed| reviewed.run).collect();
        runtime.set_seen_runs(host.viewer(), &runs);
        true
    }

    /// What the person at the window `tab` changed of their own state since last asked, as the
    /// changes that keep it in the workspace's document of it, which holds `held` now: the view
    /// state they set in this window, and what they marked seen. Someone else's mark is not theirs
    /// to keep.
    pub fn take_personal(&mut self, held: &PersonalState, tab: u64) -> Vec<Keep> {
        let Some(host) = self.host() else {
            return Vec::new();
        };
        let runtime = host.runtime();
        let me = self.me.as_str().to_owned();
        let mine = |who: &crate::runtime::Actor| personal::person_of(who) == Some(me.as_str());
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
                } if mine(&person) => keeps.push(Keep::Seen {
                    person: me.clone(),
                    resource,
                    mark,
                }),
                SeenWrite::Run { person, run } if mine(&person) => {
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
