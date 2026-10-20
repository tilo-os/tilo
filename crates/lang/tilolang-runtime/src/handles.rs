//! The handle table (implementation design 4.1 and 4.3).
//!
//! Guest code names structured values by `i32` handles. Each invocation of guest code runs in
//! a [`Frame`] with its own table; a handle encodes the serial of its frame, so a number from
//! another invocation (or another module's invocation) is refused instead of aliasing a
//! value. Frames die with the call; only values the host moves into the retained table
//! (action references kept by a view tree) outlive it.

use std::collections::HashMap;

use crate::flow::Journal;
use crate::ids::{ModuleHash, Principal, RunId};
use crate::space::Space;
use crate::store::Txn;
use crate::value::{ResourceRef, Value};
use crate::view::NodeBuild;
use crate::view_state::StateKey;

/// Bits of a handle that index the frame's table.
const INDEX_RANGE: u32 = 0x10_0000;
/// Frame serials wrap at this many.
const SERIAL_RANGE: u32 = 0x800;

/// A value the host keeps beyond one invocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Retained(pub u64);

pub struct ActionState {
    pub txn: Txn,
    pub action: String,
    /// The first resource passed in; the home of the action's outbox entries.
    pub primary: Option<ResourceRef>,
    /// The principal the runtime authenticated for this invocation: what `invoker` reads.
    pub invoker: Principal,
    /// The recorded suggestion this run approves, if any; see `ExecCtx::origin`.
    pub origin: Option<String>,
    /// Blocks to evaluate once the action has committed (`run_block`).
    pub queued: Vec<QueuedBlock>,
}

/// A block an action asked the host to run after it commits.
pub struct QueuedBlock {
    /// The resource whose `invocation` field records the outcome.
    pub part: ResourceRef,
    pub field: String,
    pub source: String,
}

pub struct FlowState {
    pub run: RunId,
    pub journal: Journal,
    pub cursor: usize,
    pub invoker: Principal,
}

#[derive(Default)]
pub struct ViewState {
    /// The space this run of the view was offered.
    pub space: Space,
    /// The window the view runs in: what its `window` and `lens` state is kept under.
    pub window: String,
    /// The person looking: whose `lens` state the view reads.
    pub viewer: Principal,
    /// Every view state slot read, with its generation at the time.
    pub state_reads: Vec<(StateKey, u64)>,
    /// The value each slot read had, its default included while it was never set.
    pub state_seen: Vec<(StateKey, Value)>,
    /// Every `space` field read, with the value it had.
    pub space_reads: Vec<(String, f64)>,
    /// Every `(resource, field)` read (the resource as `ResourceRef::id_string`), the view's
    /// dependencies.
    pub reads: Vec<(String, String)>,
    pub nodes: Vec<NodeBuild>,
}

pub enum FrameKind {
    Pure,
    Action(ActionState),
    Flow(Box<FlowState>),
    View(ViewState),
}

pub struct Frame {
    serial: u32,
    vals: Vec<Value>,
    /// The module whose code runs in this frame: the author of what it creates.
    pub module: ModuleHash,
    pub kind: FrameKind,
}

#[derive(Default)]
pub struct Frames {
    stack: Vec<Frame>,
    next_serial: u32,
    retained: HashMap<u64, Value>,
    next_retained: u64,
}

impl Frames {
    pub fn push(&mut self, module: ModuleHash, kind: FrameKind) {
        self.next_serial = self.next_serial.wrapping_add(1);
        self.stack.push(Frame {
            serial: self.next_serial.checked_rem(SERIAL_RANGE).unwrap_or(0),
            vals: Vec::new(),
            module,
            kind,
        });
    }

    pub fn pop(&mut self) -> Option<Frame> {
        self.stack.pop()
    }

    ///
    /// # Errors
    ///
    /// A message when no guest frame is running.
    pub fn top(&self) -> Result<&Frame, String> {
        self.stack
            .last()
            .ok_or_else(|| "import called with no guest frame".to_owned())
    }

    ///
    /// # Errors
    ///
    /// A message when no guest frame is running.
    pub fn top_mut(&mut self) -> Result<&mut Frame, String> {
        self.stack
            .last_mut()
            .ok_or_else(|| "import called with no guest frame".to_owned())
    }

    /// Put a value in the current frame and return its handle.
    ///
    /// # Errors
    ///
    /// A message when no frame is running or the frame's table is full.
    pub fn alloc(&mut self, v: Value) -> Result<i32, String> {
        let frame = self.top_mut()?;
        let index = u32::try_from(frame.vals.len())
            .ok()
            .and_then(|n| n.checked_add(1))
            .filter(|n| *n < INDEX_RANGE)
            .ok_or_else(|| "handle table full: too many values in one invocation".to_owned())?;
        frame.vals.push(v);
        let handle = frame.serial.wrapping_mul(INDEX_RANGE).wrapping_add(index);
        i32::try_from(handle).map_err(|_| "handle out of range".to_owned())
    }

    fn slot(&self, h: i32) -> Result<(&Frame, usize), String> {
        let frame = self.top()?;
        let raw = u32::try_from(h).map_err(|_| format!("invalid handle {h}"))?;
        let serial = raw.checked_div(INDEX_RANGE).unwrap_or(0);
        let index = raw.checked_rem(INDEX_RANGE).unwrap_or(0);
        if index == 0 || serial != frame.serial {
            return Err(format!("invalid handle {h}: not issued to this invocation"));
        }
        let at = usize::try_from(index.saturating_sub(1)).map_err(|e| e.to_string())?;
        Ok((frame, at))
    }

    ///
    /// # Errors
    ///
    /// A message when the handle was not issued to the running invocation.
    pub fn get(&self, h: i32) -> Result<&Value, String> {
        let (frame, at) = self.slot(h)?;
        frame
            .vals
            .get(at)
            .ok_or_else(|| format!("invalid handle {h}"))
    }

    ///
    /// # Errors
    ///
    /// A message when the handle was not issued to the running invocation.
    pub fn get_mut(&mut self, h: i32) -> Result<&mut Value, String> {
        let (_, at) = self.slot(h)?;
        self.top_mut()?
            .vals
            .get_mut(at)
            .ok_or_else(|| format!("invalid handle {h}"))
    }

    /// Keep a value beyond the current invocation.
    pub fn retain(&mut self, v: Value) -> Retained {
        self.next_retained = self.next_retained.wrapping_add(1);
        self.retained.insert(self.next_retained, v);
        Retained(self.next_retained)
    }

    pub fn retained(&self, r: Retained) -> Option<&Value> {
        self.retained.get(&r.0)
    }

    pub fn release(&mut self, r: Retained) {
        self.retained.remove(&r.0);
    }

    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    pub fn retained_count(&self) -> usize {
        self.retained.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module() -> ModuleHash {
        ModuleHash("m".into())
    }

    #[test]
    fn handles_are_scoped_to_their_frame() {
        let mut f = Frames::default();
        f.push(module(), FrameKind::Pure);
        let h = f.alloc(Value::Int(7)).unwrap();
        assert_eq!(f.get(h).unwrap(), &Value::Int(7));
        f.push(module(), FrameKind::Pure);
        assert!(
            f.get(h).is_err(),
            "an outer frame's handle must not resolve"
        );
        f.pop();
        assert!(f.get(h).is_ok());
        f.pop();
        f.push(module(), FrameKind::Pure);
        assert!(
            f.get(h).is_err(),
            "a cleared frame's handle must not resolve"
        );
        assert!(f.get(0).is_err());
        assert!(f.get(-1).is_err());
    }

    #[test]
    fn retained_values_outlive_frames() {
        let mut f = Frames::default();
        f.push(module(), FrameKind::Pure);
        let r = f.retain(Value::text("keep"));
        f.pop();
        assert_eq!(f.retained(r), Some(&Value::text("keep")));
        f.release(r);
        assert_eq!(f.retained_count(), 0);
    }
}
