//! The wasm host layer every guest in Tilo executes on.
//!
//! A guest interface belongs to its host: a tilolang app imports the `tl` table, a theme's
//! painter set imports nothing, and another app's model imports an ABI of its own. What does not
//! belong to any one of them is what running wasm costs and how it fails: the fuel a call may
//! burn, the wall clock it may take, the memory, tables and instances an instance may hold, the
//! one text for a spent budget or a refused growth, and the rule that an instance whose call
//! failed is never asked again. Those live here, once, so a host names its own limits and keeps
//! only its own interface.
//!
//! The engine is wasmi, which every host has and the browser build's only engine. The compiled
//! tier a native runtime executes beside it, wasmtime, lives with that runtime
//! (`tilolang-runtime`) and builds on these limits, failures and texts; it is never a feature
//! here, where any one build turning it on would put a compiler in every engine library that
//! links this layer.
//!
//! Fuel bounds a guest by the work it does, so it stops a loop; it does not bound a call that
//! waits inside a host function, nor one that executes on a runtime whose instructions are not
//! counted the way the interpreter counts them. [`Limits::deadline`] bounds a call by the clock
//! instead, where the engine can interrupt guest code: wasmtime's epoch interruption, driven by
//! a ticker that advances only while a call is in flight. wasmi has no interruption, so
//! on it the deadline is unenforced and fuel is the only bound; a host that needs both executes
//! on wasmtime.

mod failure;
pub mod wasmi;

pub use failure::{DEADLINE_EXCEEDED, Deadline, Failure, GROWTH_LIMITED, OUT_OF_FUEL, trap_text};

/// Per-call and per-instance limits.
///
/// A host passes its own values: the numbers are policy, and stay with the host that made the
/// call about what its guests may spend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Fuel for one call into a module, nested calls excluded.
    pub fuel_per_call: u64,
    /// Wall clock one call into a module may take, nested calls excluded. Enforced where the
    /// engine interrupts its guests (wasmtime's epochs) and not on wasmi, which cannot be
    /// interrupted; `None` leaves a call unbounded in time.
    pub deadline: Option<std::time::Duration>,
    /// Linear memory one instance may reach, bytes.
    pub max_memory_bytes: usize,
    /// Linear memories one instance may hold.
    pub max_memories: usize,
    /// Tables one instance may hold.
    pub max_tables: usize,
    /// Instances one store may hold.
    pub max_instances: usize,
}

/// Which imports a module may name when it is loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportPolicy {
    /// None at all: a guest with no clock, no I/O and nothing to ask.
    None,
    /// Whatever the host links.
    Any,
}

/// The rule every host keeps for an instance after a call: whether it may be called again.
///
/// A call that did not succeed may have stopped the guest part way through writing its own
/// memory, so its instance is never reused: a host with a pool drops it, and a host holding a
/// single instance keeps it only as broken. `E` is whatever the host failed with: [`Failure`] for
/// an engine failure, or the host's own error for a guest's interface.
pub fn reusable<T, E>(outcome: &Result<T, E>) -> bool {
    outcome.is_ok()
}

#[cfg(test)]
mod tests;
