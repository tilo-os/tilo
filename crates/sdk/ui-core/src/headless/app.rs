//! One way through an app: [`HeadlessApp`], the application tests reach a component the way a
//! person, a script and an agent do — by the resources it publishes and the actions on them — in
//! the frame the host draws.
//!
//! A test that drove a component had four ways in, and each proved something else: clicking a named
//! control proves where a widget sits, reading the Automerge document proves what the app stored, a
//! headless scenario proves the same clicks from a JSON file, and the control socket proves the
//! door an agent uses. Only the last is the app's public surface, and
//! [`FrameHarness::invoke_action`](super::FrameHarness::invoke_action) already speaks it — but
//! reaching it from a test meant mounting a `TileHost` by hand, wrapping it in a frame, and knowing
//! which of `call_on`, `invoke_published` and `invoke_bound_action` takes which address.
//!
//! [`HeadlessApp`] mounts an application's component in one frame and opens the door on it:
//! ```ignore
//! let host = ProbeHost::new();
//! let component = host.component(11);
//! let mut app = HeadlessApp::mount(component).unwrap();
//! let answer = app
//!     .call(&tilo_actions::ActionAddress::new("probe:shared", "edit"), serde_json::Map::new())
//!     .unwrap();
//! assert!(answer.completed().is_some(), "the app answered the action");
//! assert_eq!(app.resources()[0].id.0, "probe:shared");
//! ```
//!
//! What a test then asserts on is the state behind the resource, which is
//! what a collaborator sees: the record's document
//! ([`RecordStore::read`](crate::record::RecordStore::read)) and the resources
//! the app publishes back ([`HeadlessApp::resources`]). A click is still
//! available ([`HeadlessApp::frame_mut`]) for the thing a click alone proves —
//! hit-testing, a hit's geometry, what a frame paints — and nothing else.
#![cfg(any(test, feature = "test-support"))]
#![allow(
    clippy::expect_used,
    reason = "a prepared fixture every test draws with: it is in the checkout or the test cannot draw at all"
)]

use super::FrameHarness;
use crate::application::{AppComponent, ComponentRunnerInputs};
use serde_json::Value;
use tilo_actions::{ActionAddress, ResourceSpec};

/// One step of a drive: an action from the headless grammar, a document the app must hold, or a
/// resource it must publish.
pub use super::driver::Step;

/// Everything one drive answers — the document, the inspection tree, the named targets, the resources,
/// and what each step did — and the first step that failed.
pub use super::driver::Outcome;

/// The runner inputs every application test draws with: the pinned font catalogue and the dark theme a
/// capture is usually taken in.
#[must_use]
pub fn fixture_runner_inputs() -> ComponentRunnerInputs {
    ComponentRunnerInputs {
        theme: crate::theme::Theme::dark(),
        text_renderer: crate::font_catalog::fixture_renderer(),
    }
}

/// The window a bare [`HeadlessApp::mount`] gives the app: wide enough for a strip of strips, as the
/// headless runner's default is.
pub const MOUNT_VIEWPORT: (u32, u32) = (800, 600);

/// An application component mounted in a frame, with its door open.
pub struct HeadlessApp {
    frame: FrameHarness,
}

impl HeadlessApp {
    /// Mount `component` in a window of [`MOUNT_VIEWPORT`]. # Errors Returns the setup failure the host
    /// reports when the component cannot be mounted — the same
    /// [`crate::launch::LaunchError::ComponentSetup`] a live window fails with, so a test asserts on
    /// the real recovery owner rather than a harness-only one.
    pub fn mount(component: AppComponent) -> Result<Self, crate::launch::LaunchError> {
        Self::mount_sized(component, MOUNT_VIEWPORT, fixture_runner_inputs())
    }

    /// Mount `component` in a window of `viewport`, drawing with `inputs`.
    pub fn mount_sized(
        component: AppComponent,
        viewport: (u32, u32),
        inputs: ComponentRunnerInputs,
    ) -> Result<Self, crate::launch::LaunchError> {
        Ok(Self {
            frame: FrameHarness::from_component(component, viewport, inputs)?,
        })
    }

    /// The frame and its host, for what only a frame can prove: a click's hit-test, a named control's
    /// geometry, a capture of the pixels.
    #[must_use]
    pub fn frame(&self) -> &FrameHarness {
        &self.frame
    }

    /// The frame and its host, mutable.
    pub fn frame_mut(&mut self) -> &mut FrameHarness {
        &mut self.frame
    }

    /// Invoke an action the app publishes on one of its resources, the way an agent's call, a script's
    /// and a screen reader's reach it: resolved against the resources the app publishes now, refused
    /// when unavailable or unconfirmed, its arguments bound strictly, and then run through the host's
    /// own executor and tick.
    pub fn call(
        &mut self,
        address: &ActionAddress,
        arguments: serde_json::Map<String, Value>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        self.frame.invoke_action(address, &arguments, false, None)
    }

    /// [`HeadlessApp::call`] after the person confirmed this exact request, as the host's confirmation
    /// prompt does for an irreversible action.
    pub fn call_confirmed(
        &mut self,
        address: &ActionAddress,
        arguments: serde_json::Map<String, Value>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        self.frame.invoke_action(address, &arguments, true, None)
    }

    /// Invoke the action `action` of the resource `resource` — the ids the app publishes — with
    /// `arguments`, an object.
    pub fn call_on(
        &mut self,
        resource: &str,
        action: &str,
        arguments: Value,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        let Value::Object(arguments) = arguments else {
            return Err(tilo_actions::DoorError::InvalidArguments(
                "arguments are an object".into(),
            ));
        };
        self.call(&ActionAddress::new(resource, action), arguments)
    }

    /// [`HeadlessApp::call_on`] after the person confirmed this exact request.
    pub fn call_on_confirmed(
        &mut self,
        resource: &str,
        action: &str,
        arguments: Value,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        let Value::Object(arguments) = arguments else {
            return Err(tilo_actions::DoorError::InvalidArguments(
                "arguments are an object".into(),
            ));
        };
        self.call_confirmed(&ActionAddress::new(resource, action), arguments)
    }

    /// The resources the app publishes now — its public surface, which search, the palette and an agent
    /// all read.
    #[must_use]
    pub fn resources(&self) -> Vec<ResourceSpec> {
        self.frame.resources()
    }

    /// Whether the app publishes the action `action` on `resource`.
    #[must_use]
    pub fn publishes(&self, resource: &str, action: &str) -> bool {
        self.contract(resource, action).is_some()
    }

    /// The action's contract, as the app publishes it — what a caller is promised about confirmation,
    /// execution and result — or `None` when the app does not publish it.
    #[must_use]
    pub fn contract(&self, resource: &str, action: &str) -> Option<tilo_actions::ActionSpec> {
        tilo_actions::door::resolve(&self.resources(), &ActionAddress::new(resource, action))
            .cloned()
    }

    /// Run `steps` in order, as a scenario file runs its actions: the answers earlier steps bound fill
    /// later `{{name.path}}` placeholders, and the run stops at the first step that cannot be carried
    /// out.
    pub fn drive(&mut self, steps: &[Step]) -> Outcome {
        super::driver::drive(&mut self.frame, steps)
    }

    /// The semantic tree of the current frame: what a screen reader, an automation script and the
    /// control socket's target list read.
    #[must_use]
    pub fn inspection(&self) -> super::InspectionSnapshot {
        self.frame.inspection()
    }

    /// Draw a frame and let its edits land, as a live session does between two things a person does:
    /// the state the next step judges is the state this frame shows.
    pub fn settle(&mut self) {
        self.frame.relayout();
        self.frame.persist_tick();
    }
}
