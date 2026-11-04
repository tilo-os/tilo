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
