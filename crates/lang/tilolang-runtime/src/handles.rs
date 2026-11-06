//! The handle table.
//!
//! Guest code names structured values by `i32` handles. Each invocation of guest code runs in
//! a [`Frame`] with its own table; a handle encodes the serial of its frame, so a number from
//! another invocation (or another module's invocation) is refused instead of aliasing a
//! value. Frames die with the call; only values the host moves into the retained table
//! (action references kept by a view tree) outlive it. A resource or action reference records
//! the module whose code made it, and keeps that author when it is passed to other code: that
//! is the authorship chain the authority decides on.

use std::collections::HashMap;

use crate::flow::Journal;
use crate::ids::{ModuleHash, Principal, RunId};
use crate::space::Space;
use crate::store::Txn;
use crate::value::{ActionRef, ResourceRef, Value};
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
    /// What the person a suggestion was accepted from proposed, if this run is one: recorded in
    /// its change so history says who suggested it ([`crate::store::Version::suggested_by`]).
    pub suggested_by: Option<Principal>,
    /// The actions the action proposed (`suggest act(args)`), recorded once it has committed:
    /// a proposal is workspace state of its own, never part of this action's transaction.
    pub suggestions: Vec<(Principal, ActionRef)>,
    /// Blocks to evaluate once the action has committed (`run_block`).
    pub queued: Vec<QueuedBlock>,
    /// Set for a scenario's action, the only one that may act as a named person.
    pub scenario: bool,
    /// The people a scenario acts as (`as who at time { ... }`), innermost last, each with the
    /// time it acts at when it names one; `invoker` and `now` read the last one, or `invoker`
    /// and the invocation's time when there is none.
    pub acting_as: Vec<(Principal, Option<String>)>,
    /// What a scenario makes resources from (`from r { ... }`), innermost last: the change it
    /// commits records it as what they were made from, as an action invoked from `r` does.
    pub made_from: Vec<ResourceRef>,
    /// What the change records about its cause; a scenario commits each `as` block under it,
    /// as the person and at the time the block names.
    pub note: Box<crate::store::ChangeNote>,
    /// The resources the transaction changed, as [`ResourceRef::id_string`] writes them.
    pub touched: std::collections::BTreeSet<String>,
    /// The effects the invocation read, as its journal: what the change records beside the
    /// action, so a person can see what it fetched, what it drew and when it ran
    /// (convergence.md, "Actions").
    pub effects: EffectLog,
}

/// The effects an invocation read, and where it reads next: a replay answers from them instead
/// of doing them again.
#[derive(Default)]
pub struct EffectLog {
    pub entries: Vec<crate::flow::JournalEntry>,
    pub cursor: usize,
    /// The clock and the person's zone, read once per invocation: `now` and every day or wall
    /// time of day read in their zone answer with it, so one invocation sees one time.
    pub clock: Option<crate::flow::Clock>,
}

impl EffectLog {
    /// The entry this invocation recorded for the read it is making now, when it made it
    /// before: a replay's answer.
    pub fn next_entry(&mut self) -> Option<crate::flow::JournalEntry> {
        let entry = self.entries.get(self.cursor)?.clone();
        self.cursor = self.cursor.saturating_add(1);
        Some(entry)
    }

    /// Records a read for the invocation to replay.
    pub fn push(&mut self, entry: crate::flow::JournalEntry) {
        self.entries.push(entry);
        self.cursor = self.cursor.saturating_add(1);
    }
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
    /// Who proposed the run, when the person running it accepted a suggestion.
    pub suggested_by: Option<Principal>,
    /// The clock and the person's zone, read once per run: a run resumed tomorrow reads the
    /// moment it started ([`crate::flow::JournalEntry::Clock`]).
    pub clock: Option<crate::flow::Clock>,
}

/// Which invocation's journal the code running records its effects in: the innermost action or
/// flow, past the frames of pure code and of calls into another package's code. Code that runs
/// in neither — a view's, a block's — reads the host's clock and zone live, and records
/// nothing: a view is drawn again when the day turns, not replayed.
pub enum Effects<'a> {
    Action(&'a mut ActionState),
    Flow(&'a mut FlowState),
}

impl Effects<'_> {
    /// The clock and the zone this invocation already read, if it read them.
    pub fn pinned_clock(&self) -> Option<crate::flow::Clock> {
        match self {
            Self::Action(a) => a.effects.clock.clone(),
            Self::Flow(f) => f.clock.clone(),
        }
    }

    /// The entry the invocation recorded for the read it is making now, when it made it
    /// before: a replay's answer.
    pub fn next_entry(&mut self) -> Option<crate::flow::JournalEntry> {
        match self {
            Self::Action(a) => a.effects.next_entry(),
            Self::Flow(f) => {
                let entry = f.journal.entries.get(f.cursor)?.clone();
                f.cursor = f.cursor.saturating_add(1);
                Some(entry)
            }
        }
    }

    /// Pins what the clock read answered, without recording it again: a replay answering from
    /// the entry the invocation recorded the first time.
    pub fn pin(&mut self, clock: crate::flow::Clock) {
        match self {
            Self::Action(a) => a.effects.clock = Some(clock),
            Self::Flow(f) => f.clock = Some(clock),
        }
    }

    /// Pins the clock and zone the invocation read, and records the read where a replay finds
    /// it.
    pub fn pin_clock(&mut self, clock: crate::flow::Clock) {
        let entry = crate::flow::JournalEntry::Clock(clock.clone());
        self.pin(clock);
        match self {
            Self::Action(a) => a.effects.push(entry),
            Self::Flow(f) => {
                f.journal.entries.push(entry);
                f.cursor = f.cursor.saturating_add(1);
            }
        }
    }
}

#[derive(Default)]
pub struct ViewState {
    /// The space this run of the view was offered.
    pub space: Space,
    /// The window the view runs in: what its `window` and `lens` state is kept under.
    pub window: String,
    /// The person looking: whose `window` and `lens` state the view reads.
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
    /// When the time it read (`today` in a query or a filter) next reads differently, in
    /// seconds since 1970 (UTC): it runs again then. `None` when it read no time.
    pub changes_at: Option<i64>,
    /// What the view saw of the collections it read item by item.
    pub(crate) colls: Vec<crate::colls::CollSeen>,
    /// The listings of those collections, read once per run.
    pub(crate) listings: Vec<(
        std::rc::Rc<crate::value::CollRef>,
        std::rc::Rc<crate::store::Listing>,
    )>,
    pub nodes: Vec<NodeBuild>,
}

pub enum FrameKind {
    Pure,
    Action(ActionState),
    Flow(Box<FlowState>),
    View(ViewState),
    /// The frame of a call into another package's `fn`: it runs as that package's own code, so
    /// what it creates is authored there, and what it reads is what the view that called it
    /// showed.
    Call,
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
    /// The time `now` reads in the running invocation: read from the clock at its first use
    /// and kept until the outermost frame returns, so one invocation sees one time.
    pub now: Option<String>,
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
        let frame = self.stack.pop();
        if self.stack.is_empty() {
            self.now = None;
        }
        frame
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

    /// How many calls into other packages' code are running: one per frame a `call_fn` pushed.
    pub fn calls(&self) -> usize {
        self.stack
            .iter()
            .filter(|f| matches!(f.kind, FrameKind::Call))
            .count()
    }

    /// The innermost frame that decides what the code running sees: a call into another
    /// package's `fn` sees who its caller sees, and reads what its caller reads.
    pub fn deciding(&self) -> Option<&Frame> {
        self.stack
            .iter()
            .rev()
            .find(|f| !matches!(f.kind, FrameKind::Call))
    }

    /// The state of the view being run, looking past the frames of pure code and of calls into
    /// other packages' code: those read what the view showed, and the view runs again when what
    /// they read changes, as if it had read it itself. None when the code running was not called
    /// from a view (an action's, a flow's, a block's).
    pub fn view(&self) -> Option<&ViewState> {
        for frame in self.stack.iter().rev() {
            match &frame.kind {
                FrameKind::Call | FrameKind::Pure => {}
                FrameKind::View(vs) => return Some(vs),
                FrameKind::Action(_) | FrameKind::Flow(_) => return None,
            }
        }
        None
    }

    /// [`Self::view`], to record in.
    pub fn view_mut(&mut self) -> Option<&mut ViewState> {
        for frame in self.stack.iter_mut().rev() {
            match &mut frame.kind {
                FrameKind::Call | FrameKind::Pure => {}
                FrameKind::View(vs) => return Some(vs),
                FrameKind::Action(_) | FrameKind::Flow(_) => return None,
            }
        }
        None
    }

    /// The state of the flow being run, looking past the frames of calls into other packages'
    /// code: what such a call reads the flow read, and a run that is replayed reads it again
    /// from the journal it made then, rather than from a store the flow itself has since
    /// changed. None when the code running is not part of a flow.
    pub fn flow(&self) -> Option<&FlowState> {
        for frame in self.stack.iter().rev() {
            match &frame.kind {
                FrameKind::Call => {}
                FrameKind::Flow(fs) => return Some(fs),
                FrameKind::Pure | FrameKind::Action(_) | FrameKind::View(_) => return None,
            }
        }
        None
    }

    /// [`Self::flow`], to journal a read in.
    pub fn flow_mut(&mut self) -> Option<&mut FlowState> {
        for frame in self.stack.iter_mut().rev() {
            match &mut frame.kind {
                FrameKind::Call => {}
                FrameKind::Flow(fs) => return Some(fs),
                FrameKind::Pure | FrameKind::Action(_) | FrameKind::View(_) => return None,
            }
        }
        None
    }

    /// The journal the code running records its effects in: the innermost action or flow, past
    /// the frames of calls into other packages' code. None in a view or outside either, which
    /// read the host's clock live.
    pub fn effects(&mut self) -> Option<Effects<'_>> {
        for frame in self.stack.iter_mut().rev() {
            match &mut frame.kind {
                FrameKind::Call | FrameKind::Pure => {}
                FrameKind::Action(a) => return Some(Effects::Action(a)),
                FrameKind::Flow(f) => return Some(Effects::Flow(f)),
                FrameKind::View(_) => return None,
            }
        }
        None
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
