//! Staging a screenshot: the view state a scenario shows.
//!
//! A `--shots` scenario has three parts. What the resource does not hold —
//! stand-ins for live sources, scratch files, blobs, other people's
//! versions, members — is its fixture
//! ([`ResourceApp::fixture`](super::ResourceApp::fixture)), prepared before
//! the view is built. The resource half is ordinary actions
//! ([`ResourceApp::scenario`](super::ResourceApp::scenario)), applied through
//! the host's API like any edit. The view half — a selection, an open
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
//! for `--shots`, and [`HeadlessApp::capture`](super::headless::HeadlessApp::capture)
//! composes the same frame in a test.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use tilo_resource::Resource;
use ui_core::headless::Driver;
use ui_core::launch::LaunchError;

use super::app::ResourceApp;
use super::root::ResourceRoot;

type Stage = Box<dyn FnMut(Option<&str>)>;
type StageList = Vec<(Weak<()>, Stage)>;
type Opener = Box<dyn FnMut(&str) -> Result<(), String>>;
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

    /// Show the item at `anchor` in every view that opens items.
    pub(crate) fn open(&self, anchor: &str) -> Result<(), String> {
        let mut opened = false;
        let mut refusal = None;
        let ran = run_live(&self.openers, |open| match open(anchor) {
            Ok(()) => opened = true,
            Err(reason) => refusal = Some(reason),
        });
        match (ran, opened, refusal) {
            (false, ..) => Err("this app does not open items".into()),
            (true, true, _) | (true, false, None) => Ok(()),
            (true, false, Some(reason)) => Err(reason),
        }
    }
}

/// Stage `scenario` (`None` for a plain `--shots`) on a running `A`: apply
/// its resource actions through the host's API, then let its views and
/// surfaces stage their view state, and run the frames that settles.
pub fn stage<A: ResourceApp>(
    driver: &mut Driver,
    scenario: Option<&str>,
) -> Result<(), LaunchError> {
    let actions = A::scenario(scenario)
        .into_iter()
        .map(|invoke| match invoke.refusal() {
            Some(refusal) => Err(refusal.to_owned()),
            None => Ok((invoke.id, invoke.arguments)),
        })
        .collect();
    stage_with(
        driver,
        scenario,
        &Staging {
            title: super::app::hooks::<A>().title(),
            kind: <A::Resource as Resource>::KIND,
            stage_root,
            input: A::scenario_input,
        },
        actions,
    )
}

/// The members the root the driver hosts shows once its views
/// staged `scenario`; `None` when it hosts no root.
fn stage_root(driver: &mut Driver, scenario: Option<&str>) -> Option<Members> {
    driver.with_widget_mut(|root: &mut ResourceRoot| root.stage(scenario))
}

/// The members a staged capture shows, as the presence frames their
/// windows would send.
type Members = Vec<(
    String,
    ui_core::presence::PresenceKey,
    ui_core::PresencePayload,
)>;

/// A scenario action's id and arguments.
type ScenarioAction = (&'static str, serde_json::Map<String, serde_json::Value>);

/// What staging reads of an app besides its scenario's actions.
struct Staging {
    title: &'static str,
    kind: &'static str,
    stage_root: fn(&mut Driver, Option<&str>) -> Option<Members>,
    input: fn(Option<&str>, &mut Driver) -> Result<(), String>,
}

/// [`stage`], once the scenario's actions are built: each its id and
/// arguments, or why it does not bind.
fn stage_with(
    driver: &mut Driver,
    scenario: Option<&str>,
    app: &Staging,
    actions: Vec<Result<ScenarioAction, String>>,
) -> Result<(), LaunchError> {
    let title = app.title;
    if driver.component_instance_id().is_none() {
        return Err(LaunchError::Operation(format!(
            "{title} is not hosted here"
        )));
    }
    // A scenario's actions are the app's own fixture, declared in code, not a
    // request from a person or an agent: they are applied as confirmed, so a
    // scenario that replaces a whole project stages without a prompt.
    for action in actions {
        let (id, arguments) = action.map_err(LaunchError::Operation)?;
        driver
            .call_confirmed(&tilo_actions::ActionAddress::new(app.kind, id), arguments)
            .map_err(|error| LaunchError::Operation(error.to_string()))?;
    }
    driver.relayout();
    let members = (app.stage_root)(driver, scenario)
        .ok_or_else(|| LaunchError::Operation(format!("{title} is not hosted here")))?;
    if !members.is_empty() {
        driver.update_presence(|presence| {
            for (name, key, payload) in members {
                let user = ui_core::permissions::MemberId::person(name.clone());
                presence.set_display_name(user.clone(), name);
                presence.apply_remote_presence(user, key, payload);
            }
            true
        });
    }
    // The surfaces stage on this sync; the input meets the staged scene.
    driver.relayout();
    (app.input)(scenario, driver).map_err(|error| {
        LaunchError::Operation(format!(
            "{title} scenario {:?} input failed: {error}",
            scenario.unwrap_or("default")
        ))
    })?;
    // Project acknowledged scenario edits before awaiting asynchronous
    // rendering. Transient save feedback belongs to the captured scene;
    // its lifetime is separate from unfinished placeholder work.
    driver
        .flush_component_resources()
        .map_err(LaunchError::Operation)?;
    driver
        .wait_for_visuals(settle_bound())
        .map_err(|error| match scenario {
            Some(name) => {
                LaunchError::Operation(format!("{title} scenario {name:?} did not settle: {error}"))
            }
            None => LaunchError::Operation(format!("{title} did not settle: {error}")),
        })?;
    // The scenario's edits are saved before the frame is taken, as they would
    // be by the time anyone looked: a capture shows the record saved, not a
    // write still in flight.
    driver
        .flush_component_resources()
        .map_err(LaunchError::Operation)?;
    // How long staging took is not a wait anyone saw: the save status shows
    // the state the record is in, not a stall the machine's speed decided.
    driver.with_widget_mut(ResourceRoot::settle_saves_for_capture);
    driver.relayout();
    // Navigation replacement has coherent first-frame geometry. Capture the
    // acknowledged production frame after its visual work above completes;
    // a live progress spinner intentionally never reaches animation idle.
    Ok(())
}

/// How long a capture waits for its surfaces' asynchronous work (a page
/// render, a decode, a fixture read) before refusing to capture a
/// placeholder. `TILO_CAPTURE_SETTLE_MS` shortens it, so a check that
/// provokes work it knows cannot finish (a file that cannot be decoded)
/// observes the refusal without waiting the bound out; a live `--shots`
/// capture reads the same setting.
const SETTLE: ui_core::time::Duration = ui_core::time::Duration::from_secs(30);

fn settle_bound() -> ui_core::time::Duration {
    std::env::var("TILO_CAPTURE_SETTLE_MS")
        .ok()
        .and_then(|value| {
            tilo_failure::ignored!(
                "a settle bound that is not a number leaves the capture its own bound",
                value.trim().parse::<u64>().ok()
            )
        })
        .filter(|millis| *millis > 0)
        .map_or(SETTLE, ui_core::time::Duration::from_millis)
}

/// How long each of `frames` frames of `A`'s capture of `scenario` takes to
/// sync, lay out and draw, after one frame that warms the text caches: what
/// a `--bench` command reports. Where no native runner is linked (a browser
/// page) there is no headless capture to time.
pub fn time_frames<A: ResourceApp>(
    scenario: Option<&str>,
    frames: usize,
) -> Result<Vec<ui_core::time::Duration>, String> {
    #[cfg(tilo_native_runner)]
    {
        let mut app = super::headless::HeadlessApp::<A>::capture(scenario)
            .map_err(|error| error.to_string())?;
        let driver = app.harness();
        driver.relayout();
        Ok((0..frames)
            .map(|_| {
                let start = ui_core::time::now();
                driver.relayout();
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
