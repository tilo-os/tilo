//! Each press of the person's at the window is a call of its own: it has an invocation id, goes
//! through the dispatch as the app's call of its own action, where it is admitted as the person
//! whose press it was and kept as its `tilo.invocations` record, and runs here at once, in-process,
//! under that id. What it writes is stamped with it, and the calls it makes continue it.
//!
//! The dispatch hands the press back to this instance, which already ran it: the instance answers
//! it with how it came out rather than running it again. A call the press made waits until then,
//! so the dispatch knows the press it continues; the press itself never waits on the dispatch.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_actions::remote::InvokeRequest;
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};
use tilolang_tilo::instance::PressCalls;
use tilolang_tilo::resources::ResourceStore;

use super::{SERVING, Serving};

/// The presses the dispatch has not handed back, or that have not ended, by their invocation.
#[derive(Default)]
pub(super) struct Presses {
    open: HashMap<InvocationId, Press>,
}

/// One press on its way through the dispatch.
#[derive(Default)]
struct Press {
    /// The id the dispatch handed it back under, once it did.
    delivered: Option<u64>,
    /// How it ended, while the dispatch has not handed it back.
    ended: Option<InvocationUpdate>,
    /// The calls it made before the dispatch handed it back, in order.
    held: Vec<InvokeRequest>,
}

/// What the dispatch handing a press back comes to.
pub(super) struct HandedBack {
    /// How the press ended, when it already did: its answer.
    pub ended: Option<InvocationUpdate>,
    /// The calls it made meanwhile, to send now.
    pub held: Vec<InvokeRequest>,
}

impl Presses {
    /// The press `press` began.
    fn open(&mut self, press: InvocationId) {
        self.open.insert(press, Press::default());
    }

    /// The child call `request`, made while `served` runs: given back to send now, unless
    /// `served` is a press the dispatch has not handed back, which keeps it until it does.
    pub(super) fn hold(
        &mut self,
        served: InvocationId,
        request: InvokeRequest,
    ) -> Option<InvokeRequest> {
        match self.open.get_mut(&served) {
            Some(press) if press.delivered.is_none() => {
                press.held.push(request);
                None
            }
            _ => Some(request),
        }
    }

    /// The dispatch handed `press` back under `id`. `None` when it is no press of this
    /// instance's: an invocation to run.
    pub(super) fn handed_back(&mut self, press: InvocationId, id: u64) -> Option<HandedBack> {
        let open = self.open.get_mut(&press)?;
        let handed = HandedBack {
            ended: open.ended.take(),
            held: std::mem::take(&mut open.held),
        };
        if handed.ended.is_some() {
            self.open.remove(&press);
        } else {
            open.delivered = Some(id);
        }
        Some(handed)
    }

    /// `press` ended as `update`: the id to answer it under now, when the dispatch handed it
    /// back already; else it is answered when it does.
    pub(super) fn ended(
        &mut self,
        press: InvocationId,
        update: InvocationUpdate,
    ) -> Option<(u64, InvocationUpdate)> {
        let open = self.open.get_mut(&press)?;
        if let Some(id) = open.delivered {
            self.open.remove(&press);
            return Some((id, update));
        }
        open.ended = Some(update);
        None
    }

    /// The dispatch refused `press`: the calls it made, which nothing continues. `None` when it
    /// is no press waiting for the dispatch.
    pub(super) fn refused(&mut self, press: InvocationId) -> Option<Vec<InvokeRequest>> {
        if self
            .open
            .get(&press)
            .is_none_or(|open| open.delivered.is_some())
        {
            return None;
        }
        self.open.remove(&press).map(|open| open.held)
    }

    /// The id the dispatch handed `press` back under, once it did.
    pub(super) fn delivered(&self, press: InvocationId) -> Option<u64> {
        self.open.get(&press).and_then(|open| open.delivered)
    }

    /// Whether a press is still on its way: running, or not yet answered by the dispatch.
    pub(super) fn any_open(&self) -> bool {
        !self.open.is_empty()
    }

    /// The press the dispatch handed back under `id`, if one was.
    pub(super) fn delivered_as(&self, id: u64) -> Option<InvocationId> {
        self.open
            .iter()
            .find(|(_, press)| press.delivered == Some(id))
            .map(|(press, _)| *press)
    }
}

/// What makes each of the person's presses in a window a call: its id, the writes it stamps and
/// the calls it continues, while it runs ([`PressCalls`]).
pub(super) struct PressLine {
    pub store: ResourceStore,
    pub presses: Rc<RefCell<Presses>>,
    /// The person at the window, whose press it is.
    pub person: String,
    /// Whether the instance has a dispatch to record a press with: a document on this device
    /// alone has none, and its presses run as no call.
    pub recorded: bool,
}

impl PressCalls for PressLine {
    fn begin(&self, address: &ActionAddress) -> Option<InvocationId> {
        // What the window does while it serves a call its person signed (a visitor's page) is
        // that call's already.
        if !self.recorded || self.store.serving().is_some() {
            return None;
        }
        let press = InvocationId::new();
        self.presses.borrow_mut().open(press);
        self.store.serve(Some(press.0.to_string()));
        SERVING.with(|slot| {
            *slot.borrow_mut() = Some(Serving {
                invocation: press,
                person: Some(self.person.clone()),
                action: address.action.clone(),
            });
        });
        Some(press)
    }

    fn end(&self) {
        SERVING.with(|slot| *slot.borrow_mut() = None);
        self.store.serve(None);
    }
}
