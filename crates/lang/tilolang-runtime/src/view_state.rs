//! View state (workspace-model.md, "The pieces" and "In the language"): what a view declares with
//! `window` and `lens`.
//!
//! Both belong to one person. `window` state is how that person's window shows its resource
//! (which view, sort, filter); `lens` state is where they are in it (scroll, a draft they are
//! typing). Both live here, in the runtime, keyed by the window's id, the person looking and
//! the declaration's id; the layer says which of the two a slot is. The runtime also keeps
//! slots of its own — a delivery, the lenses on a resource, a seen mark — keyed the same way
//! but in no layer, and some of them one person's. A slot holds the value in the form the
//! guest hands it over: Int, Float and Bool boxed like `some_i`, everything else as it is.
//! Every change bumps the slot's generation, which is how a view that read the slot knows to
//! run again.
//!
//! What the person changes of a declared slot is kept under their entries of the workspace's
//! record of it, not in this process's memory: the host takes each change
//! ([`ViewStates::take_writes`]) and keeps it, and gives back what it kept when a window opens
//! ([`ViewStates::restore`]), so the place a person picked is still theirs after a restart and
//! on their other devices. What the runtime
//! itself keeps are platform readings (lenses present on a resource, seen marks, deliveries):
//! those are [`ViewStates::set`]'s, and are never written to a record.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::value::Value;

/// Which layer a slot belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StateLayer {
    /// How one person's window shows its resource.
    Window,
    /// Where one person is in a window: scroll, cursor, selection, a draft.
    Lens,
}

impl StateLayer {
    /// The layer as the guest passes it: 0 for `window`, 1 for `lens`.
    pub const fn code(self) -> i32 {
        match self {
            Self::Window => 0,
            Self::Lens => 1,
        }
    }

    /// The layer a guest's `code` names: 0 for `window`, 1 for `lens`.
    pub fn from_code(code: i32) -> Option<Self> {
        match code {
            0 => Some(Self::Window),
            1 => Some(Self::Lens),
            _ => None,
        }
    }
}

/// One slot of view state.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StateKey {
    pub window: String,
    /// The person the slot is one of: the person looking, for `window` and `lens` state; the
    /// person a mark is of, for the runtime's own; `None` for a slot that is no one's.
    pub principal: Option<String>,
    /// The layer a declared state lives in; `None` for the runtime's own slots.
    pub layer: Option<StateLayer>,
    /// The id of the `window` or `lens` declaration, or what the runtime keeps in its own.
    pub state: String,
}

impl StateKey {
    /// How `person`'s window shows its resource: a `window` declaration's slot.
    pub fn window(
        window: impl Into<String>,
        person: impl Into<String>,
        state: impl Into<String>,
    ) -> Self {
        Self {
            window: window.into(),
            principal: Some(person.into()),
            layer: Some(StateLayer::Window),
            state: state.into(),
        }
    }

    /// Where `person` is in the window: a `lens` declaration's slot.
    pub fn lens(
        window: impl Into<String>,
        person: impl Into<String>,
        state: impl Into<String>,
    ) -> Self {
        Self {
            window: window.into(),
            principal: Some(person.into()),
            layer: Some(StateLayer::Lens),
            state: state.into(),
        }
    }

    /// A slot the runtime keeps for itself, outside any view's declarations: a slot that
    /// moves when something views read of the host moves.
    pub fn runtime(
        window: impl Into<String>,
        principal: Option<String>,
        state: impl Into<String>,
    ) -> Self {
        Self {
            window: window.into(),
            principal,
            layer: None,
            state: state.into(),
        }
    }

    /// Whether it is a `window` declaration's slot: how the person's window shows its resource.
    pub fn is_window(&self) -> bool {
        self.layer == Some(StateLayer::Window)
    }
}

/// Every slot of view state the runtime holds.
#[derive(Default)]
pub struct ViewStates {
    slots: HashMap<StateKey, (Value, u64)>,
    /// The value a scenario stages a declaration at (`window place = Task.done`), by the layer
    /// it is declared in and the declaration's id: what a window that reads it shows until its
    /// person sets the slot, where the scenario staged a window the capture lays out.
    seeds: HashMap<StateLayer, HashMap<String, (Value, u64)>>,
    clock: u64,
    /// What the person changed here since the host last took it
    /// ([`ViewStates::take_writes`]), in the order they changed it.
    writes: Vec<(StateKey, Value)>,
}

impl ViewStates {
    /// The value of a slot, when it was ever set.
    pub fn get(&self, key: &StateKey) -> Option<&Value> {
        self.slots.get(key).map(|(v, _)| v)
    }

    /// The slots `person` holds under `window`, each with its generation: what a reading made
    /// of them depends on.
    pub fn slots_of(&self, window: &str, principal: &str) -> Vec<(StateKey, Value, u64)> {
        self.slots
            .iter()
            .filter(|(key, _)| key.window == window && key.principal.as_deref() == Some(principal))
            .map(|(key, (value, generation))| (key.clone(), value.clone(), *generation))
            .collect()
    }

    /// The slot's generation: 0 until it is first set, then larger after every change.
    pub fn generation(&self, key: &StateKey) -> u64 {
        let slot = self.slots.get(key).map_or(0, |(_, g)| *g);
        let seed = key
            .layer
            .and_then(|layer| self.seeds.get(&layer))
            .and_then(|states| states.get(&key.state))
            .map_or(0, |(_, g)| *g);
        slot.max(seed)
    }

    /// Stage the value the declaration `state` starts from in `layer`, as a scenario does: a
    /// window that reads it shows it until the person sets the slot, and it is no one's change
    /// to keep.
    pub fn seed(&mut self, layer: StateLayer, state: &str, value: Value) {
        self.clock = self.clock.saturating_add(1);
        self.seeds
            .entry(layer)
            .or_default()
            .insert(state.to_owned(), (value, self.clock));
    }

    /// The value a scenario staged the declaration `key` names at, when it staged one: what a
    /// window shows where the person has set nothing.
    pub fn seeded(&self, key: &StateKey) -> Option<&Value> {
        self.seeds.get(&key.layer?)?.get(&key.state).map(|(v, _)| v)
    }

    /// A number that moves with every change of any slot.
    pub fn clock(&self) -> u64 {
        self.clock
    }

    /// Change a slot the platform keeps here: a reading the host fills, not the person's state.
    pub fn set(&mut self, key: StateKey, value: Value) {
        self.clock = self.clock.saturating_add(1);
        self.slots.insert(key, (value, self.clock));
    }

    /// Change a slot the person set in a view: kept as [`ViewStates::set`] keeps it, and once
    /// for the host to write under the person's entries of the workspace's record.
    pub fn write(&mut self, key: StateKey, value: Value) {
        self.set(key.clone(), value.clone());
        self.writes.push((key, value));
    }

    /// Give back what the person changed since this was last asked: what the host keeps in
    /// their entries of the workspace's record. A slot changed twice is handed over once,
    /// as it is now.
    pub fn take_writes(&mut self) -> Vec<(StateKey, Value)> {
        let mut writes: Vec<(StateKey, Value)> = Vec::new();
        for (key, _) in std::mem::take(&mut self.writes) {
            let Some((value, _)) = self.slots.get(&key) else {
                continue;
            };
            let value = value.clone();
            match writes.iter_mut().find(|(held, _)| *held == key) {
                Some((_, held)) => *held = value,
                None => writes.push((key, value)),
            }
        }
        writes
    }

    /// Put a slot the person changed elsewhere — another device of theirs, or an earlier run —
    /// back, without handing it back as a change of theirs. A slot already holding the value is
    /// left alone, so views that read it do not run again for nothing.
    pub fn restore(&mut self, key: StateKey, value: Value) -> bool {
        if self.get(&key) == Some(&value) {
            return false;
        }
        self.set(key, value);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(state: &str) -> StateKey {
        StateKey::window("tasks", "ada", state)
    }

    #[test]
    fn a_slot_changed_twice_is_handed_over_once_as_it_is_now() {
        let mut states = ViewStates::default();
        states.write(window("filter"), Value::text("All"));
        states.write(window("filter"), Value::text("Today"));
        assert_eq!(
            states.take_writes(),
            vec![(window("filter"), Value::text("Today"))]
        );
        assert_eq!(states.take_writes(), Vec::new(), "once");
    }

    #[test]
    fn a_reading_the_runtime_sets_is_never_kept_in_a_record() {
        let mut states = ViewStates::default();
        let present = StateKey::runtime("tasks", None, "present");
        states.set(present.clone(), Value::text("ada"));
        assert_eq!(states.take_writes(), Vec::new());
        assert_eq!(states.get(&present), Some(&Value::text("ada")));
    }

    #[test]
    fn restoring_a_slot_that_already_holds_its_value_leaves_it_alone() {
        let mut states = ViewStates::default();
        assert!(states.restore(window("filter"), Value::text("Today")));
        let generation = states.generation(&window("filter"));
        assert!(!states.restore(window("filter"), Value::text("Today")));
        assert_eq!(states.generation(&window("filter")), generation);
        assert_eq!(states.take_writes(), Vec::new(), "a restore is no change");
    }
}
