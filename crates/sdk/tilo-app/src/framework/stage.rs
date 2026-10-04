//! Staging a screenshot: the view state a scenario shows.
//!
//! A `--shots` scenario has three parts. What the resource does not hold —
//! stand-ins for live sources, scratch files, blobs, other people's
//! versions, collaborators — is its fixture
//! ([`ResourceApp::fixture`](super::ResourceApp::fixture)), prepared before
//! the view is built. The resource half is ordinary actions
//! ([`ResourceApp::scenario`](super::ResourceApp::scenario)), applied through
//! the host's door like any edit. The view half — a selection, an open
//! dialog, a scroll position, a drag in flight — belongs to the views and
//! surfaces that own it, so the framework asks them once the actions are
//! laid out: every [`ViewCx::on_stage`](super::ViewCx::on_stage) callback
//! runs, and every hosted [`ResourceSurface`](super::ResourceSurface) gets
//! [`stage`](super::ResourceSurface::stage) on its next sync, where it has
//! its document. Then the capture waits for the frame to settle: a surface
//! still decoding, rendering a page or reading a fixture says so with the
//! toolkit's `Widget::has_pending_visual_work`.
//!
//! [`stage`] is the whole capture after the fixture: the launcher calls it
//! for `--shots`, and [`TestApp::capture`](super::testing::HeadlessApp::capture)
//! composes the same frame in a test.

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

/// The stage callbacks of one app's views, each tied to the view scope that
/// registered it (a row of an `Each` drops its callbacks with the row).
pub(crate) struct Stages(Rc<RefCell<StageList>>);

impl Clone for Stages {
    fn clone(&self) -> Self {
        Self(Rc::clone(&self.0))
    }
}

impl Stages {
    pub(crate) fn new() -> Self {
        Self(Rc::default())
    }

    pub(crate) fn push(&self, alive: Weak<()>, stage: Stage) {
        self.0.borrow_mut().push((alive, stage));
    }

    /// Run every live callback for `scenario`, in registration order.
    pub(crate) fn run(&self, scenario: Option<&str>) {
        // Take the list so a callback may register another without a
        // reentrant borrow.
        let mut current = std::mem::take(&mut *self.0.borrow_mut());
        current.retain_mut(|(alive, stage)| {
            if alive.strong_count() == 0 {
                return false;
            }
            stage(scenario);
            true
        });
        let mut list = self.0.borrow_mut();
        current.append(&mut list);
        *list = current;
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
    let collaborators = harness
        .with_widget_mut(|root: &mut ResourceRoot<A::Resource>| root.stage(scenario))
        .ok_or_else(|| LaunchError::Operation(format!("{} is not hosted here", A::TITLE)))?;
    if !collaborators.is_empty() {
        harness.update_presence(|presence| {
            for (name, key, payload) in collaborators {
                let user = ui_core::permissions::UserId(name.clone());
                presence.set_display_name(user, name.clone());
                presence.apply_remote_presence(name, key, payload);
            }
            true
        });
    }
    harness
        .wait_for_visuals(SETTLE)
        .map_err(|error| match scenario {
            Some(name) => LaunchError::Operation(format!(
                "{} scenario {name:?} did not settle: {error}",
                A::TITLE
            )),
            None => LaunchError::Operation(format!("{} did not settle: {error}", A::TITLE)),
        })
}

/// How long a capture waits for its surfaces' asynchronous work (a page
/// render, a decode, a fixture read) before refusing to capture a
/// placeholder.
const SETTLE: ui_core::time::Duration = ui_core::time::Duration::from_secs(30);

/// The instance the harness hosts `A` under.
fn root_instance<A: ResourceApp>(harness: &FrameHarness) -> Result<u64, LaunchError> {
    harness
        .component_instance_id()
        .ok_or_else(|| LaunchError::Operation(format!("{} is not hosted here", A::TITLE)))
}
