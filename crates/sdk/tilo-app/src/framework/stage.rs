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
//! its document. What a person does in the scene — text typed, a drag held
//! over its target — is input
//! ([`ResourceApp::scenario_input`](super::ResourceApp::scenario_input)),
//! driven through the window's event path after that. Then the capture
//! waits for the frame to settle: a surface
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
    let staged = Staged {
        title: A::TITLE,
        kind: <A::Resource as Resource>::KIND,
        actions: A::scenario(scenario)
            .into_iter()
            .map(|invoke| (invoke.id, invoke.arguments))
            .collect(),
        input: A::scenario_input,
    };
    staged.stage(harness, scenario)
}

/// What [`stage`] stages of an app, without its type: the app's title and
/// resource kind, the scenario's actions and its input.
struct Staged {
    title: &'static str,
    kind: &'static str,
    actions: Vec<(&'static str, serde_json::Map<String, serde_json::Value>)>,
    input: fn(Option<&str>, &mut FrameHarness) -> Result<(), String>,
}

impl Staged {
    /// [`stage`] of this app.
    fn stage(self, harness: &mut FrameHarness, scenario: Option<&str>) -> Result<(), LaunchError> {
        let root = harness
            .component_instance_id()
            .ok_or_else(|| LaunchError::Operation(format!("{} is not hosted here", self.title)))?;
        // A scenario's actions are the app's own fixture, declared in code, not a
        // request from a person or an agent: they are applied as confirmed, so a
        // scenario that replaces a whole project stages without a prompt.
        for (id, arguments) in &self.actions {
            let address = BoundActionAddress {
                instance_id: root,
                address: tilo_actions::ActionAddress::new(self.kind, *id),
            };
            harness
                .invoke_bound_action_confirmed(&address, arguments)
                .map_err(|error| LaunchError::Operation(error.to_string()))?;
        }
        harness.relayout();
        let collaborators = harness
            .with_widget_mut(|root: &mut ResourceRoot| root.stage(scenario))
            .ok_or_else(|| LaunchError::Operation(format!("{} is not hosted here", self.title)))?;
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
        // The surfaces stage on this sync; the input meets the staged scene.
        harness.relayout();
        (self.input)(scenario, harness).map_err(|error| {
            LaunchError::Operation(format!(
                "{} scenario {:?} input failed: {error}",
                self.title,
                scenario.unwrap_or("default")
            ))
        })?;
        // Project acknowledged scenario edits before awaiting asynchronous
        // rendering. Transient save feedback belongs to the captured scene;
        // its lifetime is separate from unfinished placeholder work.
        harness
            .flush_component_records()
            .map_err(LaunchError::Operation)?;
        harness
            .wait_for_visuals(SETTLE)
            .map_err(|error| match scenario {
                Some(name) => LaunchError::Operation(format!(
                    "{} scenario {name:?} did not settle: {error}",
                    self.title
                )),
                None => LaunchError::Operation(format!("{} did not settle: {error}", self.title)),
            })?;
        // The scenario's edits are saved before the frame is taken, as they would
        // be by the time anyone looked: a capture shows the record saved, not a
        // write still in flight.
        harness
            .flush_component_records()
            .map_err(LaunchError::Operation)?;
        harness.relayout();
        // Navigation replacement has coherent first-frame geometry. Capture the
        // acknowledged production frame after its visual work above completes;
        // a live progress spinner intentionally never reaches animation idle.
        Ok(())
    }
}

/// How long a capture waits for its surfaces' asynchronous work (a page
/// render, a decode, a fixture read) before refusing to capture a
/// placeholder.
const SETTLE: ui_core::time::Duration = ui_core::time::Duration::from_secs(30);

/// How long each of `frames` frames of `A`'s capture of `scenario` takes to
/// sync, lay out and draw, after one frame that warms the text caches: what
/// a `--bench` command reports. Where no native runner is linked (a browser
/// page) there is no headless capture to time.
///
/// # Errors
/// Returns why the capture could not be built, or that this build has no
/// headless capture.
pub fn time_frames<A: ResourceApp>(
    scenario: Option<&str>,
    frames: usize,
) -> Result<Vec<ui_core::time::Duration>, String> {
    #[cfg(tilo_native_runner)]
    {
        let mut app = super::testing::HeadlessApp::<A>::capture(scenario)
            .map_err(|error| error.to_string())?;
        let harness = app.harness();
        harness.relayout();
        Ok((0..frames)
            .map(|_| {
                let start = ui_core::time::now();
                harness.relayout();
                start.elapsed()
            })
            .collect())
    }
    #[cfg(not(tilo_native_runner))]
    {
        let _ = (scenario, frames);
        Err("timing frames needs the native runner's headless capture".into())
    }
}
