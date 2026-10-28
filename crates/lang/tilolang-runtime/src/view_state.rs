//! View state (workspace-model.md sections 1, 4 and 7): what a view declares with `window`
//! and `lens`.
//!
//! `window` state belongs to a window and is shared by everyone looking at it; `lens` state
//! belongs to one person looking at one window. Both live here, in the runtime, keyed by the
//! window's id, the person for a lens, and the declaration's id. A slot holds the value in the
//! form the guest hands it over: Int, Float and Bool boxed like `some_i`, everything else as
//! it is. Every change bumps the slot's generation, which is how a view that read the slot
//! knows to run again.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::value::Value;

/// Which layer a slot belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StateLayer {
    Window,
    Lens,
}

impl StateLayer {
    /// The layer as the guest passes it: 0 for `window`, 1 for `lens`.
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
    /// The person, for a lens; `None` for window state.
    pub principal: Option<String>,
    /// The id of the `window` or `lens` declaration.
    pub state: String,
}

/// Every slot of view state the runtime holds.
#[derive(Default)]
pub struct ViewStates {
    slots: HashMap<StateKey, (Value, u64)>,
    clock: u64,
}

impl ViewStates {
    /// The value of a slot, when it was ever set.
    pub fn get(&self, key: &StateKey) -> Option<&Value> {
        self.slots.get(key).map(|(v, _)| v)
    }

    /// The slot's generation: 0 until it is first set, then larger after every change.
    pub fn generation(&self, key: &StateKey) -> u64 {
        self.slots.get(key).map_or(0, |(_, g)| *g)
    }

    /// A number that moves with every change of any slot.
    pub fn clock(&self) -> u64 {
        self.clock
    }

    /// Change a slot.
    pub fn set(&mut self, key: StateKey, value: Value) {
        self.clock = self.clock.saturating_add(1);
        self.slots.insert(key, (value, self.clock));
    }
}
