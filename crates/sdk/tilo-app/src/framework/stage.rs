//! Staging a screenshot: the view state a scenario shows.
//!
//! A `--shots` scenario is two halves. The resource half is ordinary actions
//! ([`ResourceApp::scenario`](super::ResourceApp::scenario)), applied through
//! the host's door like any edit. The view half — a selection, an open
//! dialog, a scroll position, a drag in flight — belongs to the views and
//! surfaces that own it, so the framework asks them: every
//! [`ViewCx::on_stage`](super::ViewCx::on_stage) callback runs, and every
//! hosted [`ResourceSurface`](super::ResourceSurface) gets
//! [`stage`](super::ResourceSurface::stage) on its next sync, where it has its
//! document.
//!
//! [`stage`] is the whole capture: the launcher calls it for `--shots`, and
//! tests call it (or [`TestApp::stage`](super::testing::HeadlessApp::stage))
//! to compose the same frame.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use tilo_resource::Resource;
use ui_core::bound_dispatch::BoundActionAddress;
use ui_core::headless::FrameHarness;
use ui_core::launch::LaunchError;

use super::app::ResourceApp;
use super::root::ResourceRoot;

type Stage = Box<dyn FnMut(Option<&str>)>;
type StageList = Vec<(Weak<()>, Stage)>;
type Opener = Box<dyn FnMut(&str)>;
type OpenerList = Vec<(Weak<()>, Opener)>;

/// The view-state callbacks of one app's views — how each stages a
/// screenshot scenario, and how each shows the item a search hit opens —
/// tied to the view scope that registered it (a row of an `Each` drops its
/// callbacks with the row).
pub(crate) struct Stages {
    stages: Rc<RefCell<StageList>>,
    openers: Rc<RefCell<OpenerList>>,
}

impl Clone for Stages {
    fn clone(&self) -> Self {
        Self {
            stages: Rc::clone(&self.stages),
            openers: Rc::clone(&self.openers),
        }
    }
}

/// Run every live callback of `list`, in registration order. The list is
/// taken first so a callback may register another without a reentrant
/// borrow.
fn run_live<F: ?Sized>(
    list: &RefCell<Vec<(Weak<()>, Box<F>)>>,
    mut call: impl FnMut(&mut F),
) -> bool {
    let mut current = std::mem::take(&mut *list.borrow_mut());
    let mut ran = false;
    current.retain_mut(|(alive, callback)| {
        if alive.strong_count() == 0 {
            return false;
        }
        call(callback);
        ran = true;
        true
    });
    let mut kept = list.borrow_mut();
    current.append(&mut kept);
    *kept = current;
    ran
}

impl Stages {
    pub(crate) fn new() -> Self {
        Self {
            stages: Rc::default(),
            openers: Rc::default(),
        }
    }

    pub(crate) fn push(&self, alive: Weak<()>, stage: Stage) {
        self.stages.borrow_mut().push((alive, stage));
    }

    /// Run every live callback for `scenario`, in registration order.
    pub(crate) fn run(&self, scenario: Option<&str>) {
        run_live(&self.stages, |stage| stage(scenario));
    }

    pub(crate) fn push_opener(&self, alive: Weak<()>, opener: Opener) {
        self.openers.borrow_mut().push((alive, opener));
    }

    /// Whether any live view shows the items search hits open.
    pub(crate) fn opens(&self) -> bool {
        self.openers
            .borrow()
            .iter()
            .any(|(alive, _)| alive.strong_count() > 0)
    }

    /// Show the item at `anchor` in every view that opens items. Returns
    /// whether any did.
    pub(crate) fn open(&self, anchor: &str) -> bool {
        run_live(&self.openers, |open| open(anchor))
    }
}

/// Stage `scenario` (`None` for a plain `--shots`) on a running `A`: apply
/// its resource actions through the host's door, then let its views and
/// surfaces stage their view state, and run the frames that settles.
///
/// # Errors
/// Returns the host's refusal of a scenario action, or an error when the
/// harness does not host `A`.
pub fn stage<A: ResourceApp>(
    harness: &mut FrameHarness,
    scenario: Option<&str>,
) -> Result<(), LaunchError> {
    let root = root_instance::<A>(harness)?;
    // A scenario's actions are the app's own fixture, declared in code, not a
    // request from a person or an agent: they are applied as confirmed, so a
    // scenario that replaces a whole project stages without a prompt.
    for invoke in A::scenario(scenario) {
        let address = BoundActionAddress {
            instance_id: root,
            address: tilo_actions::ActionAddress::new(<A::Resource as Resource>::KIND, invoke.id),
        };
        harness
            .invoke_bound_action_confirmed(&address, &invoke.arguments)
            .map_err(|error| LaunchError::Operation(error.to_string()))?;
    }
    harness.relayout();
    harness
        .with_widget_mut(|root: &mut ResourceRoot<A::Resource>| root.stage(scenario))
        .ok_or_else(|| LaunchError::Operation(format!("{} is not hosted here", A::TITLE)))?;
    Ok(())
}

/// The instance the harness hosts `A` under.
fn root_instance<A: ResourceApp>(harness: &FrameHarness) -> Result<u64, LaunchError> {
    harness
        .component_instance_id()
        .ok_or_else(|| LaunchError::Operation(format!("{} is not hosted here", A::TITLE)))
}
