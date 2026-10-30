//! One way through an app: [`HeadlessApp`], the application tests reach a
//! component the way a person, a script and an agent do — by the resources it
//! publishes and the actions on them — in the frame the host draws.
//!
//! A test that drove a component had four ways in, and each proved something
//! else. Clicking a named control proves where a widget sits; reading the
//! Automerge document proves what the app stored; a headless scenario proves
//! the same clicks from a JSON file; and the control socket proves the door
//! an agent uses. Only the last is the app's public surface, and
//! [`FrameHarness::invoke_action`](super::FrameHarness::invoke_action) already
//! speaks it — but reaching it from a test meant mounting a `TileHost` by
//! hand, wrapping it in a frame, and knowing which of `call_on`,
//! `invoke_published` and `invoke_bound_action` takes which address.
//!
//! [`HeadlessApp`] mounts an application's component in one frame and opens
//! the door on it:
//!
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

/// One step of a drive: an action from the headless grammar, a document the
/// app must hold, or a resource it must publish.
///
/// It is the vocabulary a scenario already speaks
/// ([`super::driver::Step`]), so a failing test's steps are a spec someone can
/// replay with `--headless-drive`. The step a test about state usually means is
/// an invocation — one published action, by address, through the door every
/// other caller uses — and a resource or document expectation, which asserts on
/// what the app publishes or stores at that point. A click stays available for
/// the thing a click alone proves: hit-testing, a hit's geometry, what a frame
/// paints.
pub use super::driver::Step;

/// Everything one drive answers — the document, the inspection tree, the named
/// targets, the resources, and what each step did — and the first step that
/// failed.
pub use super::driver::Outcome;

/// The runner inputs every application test draws with: the pinned font
/// catalogue, so a frame's text is the same on every machine, and the dark
/// theme — the one a capture is usually taken in.
#[must_use]
pub fn fixture_runner_inputs() -> ComponentRunnerInputs {
    let catalog = crate::font_catalog::FontCatalog::controlled_fixture(
        crate::font_catalog::checkout_root(),
        "en-US",
    );
    ComponentRunnerInputs {
        theme: crate::theme::Theme::dark(),
        text_renderer: crate::text_render::TextRenderer::with_font_catalog(&catalog)
            .expect("the prepared font fixture loads"),
    }
}

/// The window a bare [`HeadlessApp::mount`] gives the app: wide enough for a
/// strip of strips, as the headless runner's default is.
pub const MOUNT_VIEWPORT: (u32, u32) = (800, 600);

/// An application component mounted in a frame, with its door open.
pub struct HeadlessApp {
    frame: FrameHarness,
}

impl HeadlessApp {
    /// Mount `component` in a window of [`MOUNT_VIEWPORT`].
    ///
    /// Its runner inputs are the controlled fixture every application test
    /// draws with: the pinned font catalogue and the dark theme.
    ///
    /// # Errors
    ///
    /// Returns the setup failure the host reports when the component cannot
    /// be mounted — the same [`crate::launch::LaunchError::ComponentSetup`] a
    /// live window fails with, so a test asserts on the real recovery owner
    /// rather than a harness-only one.
    pub fn mount(component: AppComponent) -> Result<Self, crate::launch::LaunchError> {
        Self::mount_sized(component, MOUNT_VIEWPORT, fixture_runner_inputs())
    }

    /// Mount `component` in a window of `viewport`, drawing with `inputs`.
    ///
    /// # Errors
    ///
    /// As [`HeadlessApp::mount`].
    pub fn mount_sized(
        component: AppComponent,
        viewport: (u32, u32),
        inputs: ComponentRunnerInputs,
    ) -> Result<Self, crate::launch::LaunchError> {
        Ok(Self {
            frame: FrameHarness::from_component(component, viewport, inputs)?,
        })
    }

    /// The frame and its host, for what only a frame can prove: a click's
    /// hit-test, a named control's geometry, a capture of the pixels.
    #[must_use]
    pub fn frame(&self) -> &FrameHarness {
        &self.frame
    }

    /// The frame and its host, mutable.
    pub fn frame_mut(&mut self) -> &mut FrameHarness {
        &mut self.frame
    }

    /// Invoke an action the app publishes on one of its resources, the way an
    /// agent's call, a script's and a screen reader's reach it: resolved
    /// against the resources the app publishes now, refused when unavailable
    /// or unconfirmed, its arguments bound strictly, and then run through the
    /// host's own executor and tick.
    ///
    /// # Errors
    ///
    /// Returns the door's refusal: unknown or hidden address, unavailable
    /// action, missing confirmation, invalid arguments, or a failed run.
    pub fn call(
        &mut self,
        address: &ActionAddress,
        arguments: serde_json::Map<String, Value>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        self.frame.invoke_action(address, &arguments, false, None)
    }

    /// [`HeadlessApp::call`] after the person confirmed this exact request,
    /// as the host's confirmation prompt does for an irreversible action.
    ///
    /// # Errors
    ///
    /// As [`HeadlessApp::call`], except that no confirmation is required.
    pub fn call_confirmed(
        &mut self,
        address: &ActionAddress,
        arguments: serde_json::Map<String, Value>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        self.frame.invoke_action(address, &arguments, true, None)
    }

    /// Invoke the action `action` of the resource `resource` — the ids the
    /// app publishes — with `arguments`, an object.
    ///
    /// # Errors
    ///
    /// As [`HeadlessApp::call`], and when `arguments` is not an object.
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
    ///
    /// # Errors
    ///
    /// As [`HeadlessApp::call_on`], except that no confirmation is required.
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

    /// The resources the app publishes now — its public surface, which
    /// search, the palette and an agent all read.
    #[must_use]
    pub fn resources(&self) -> Vec<ResourceSpec> {
        self.frame.resources()
    }

    /// Whether the app publishes the action `action` on `resource`.
    #[must_use]
    pub fn publishes(&self, resource: &str, action: &str) -> bool {
        self.contract(resource, action).is_some()
    }

    /// The action's contract, as the app publishes it — what a caller is
    /// promised about confirmation, execution and result — or `None` when
    /// the app does not publish it.
    #[must_use]
    pub fn contract(&self, resource: &str, action: &str) -> Option<tilo_actions::ActionSpec> {
        tilo_actions::door::resolve(&self.resources(), &ActionAddress::new(resource, action))
            .cloned()
    }

    /// Run `steps` in order, as a scenario file runs its actions: the answers
    /// earlier steps bound fill later `{{name.path}}` placeholders, and the
    /// run stops at the first step that cannot be carried out.
    ///
    /// This is [`super::driver::drive`] — the same function `--headless-drive`
    /// runs its steps through, on the same `FrameHarness::perform_bound`, so
    /// what a test proves here is what the app's own process does. The app is
    /// left where the last step put it, so a test keeps driving it or asserts
    /// on the state it reached.
    pub fn drive(&mut self, steps: &[Step]) -> Outcome {
        super::driver::drive(&mut self.frame, steps)
    }

    /// The semantic tree of the current frame: what a screen reader, an
    /// automation script and the control socket's target list read.
    #[must_use]
    pub fn inspection(&self) -> super::InspectionSnapshot {
        self.frame.inspection()
    }

    /// Draw a frame and let its edits land, as a live session does between two
    /// things a person does: the state the next step judges is the state this
    /// frame shows.
    pub fn settle(&mut self) {
        self.frame.relayout();
        self.frame.persist_tick();
    }
}
