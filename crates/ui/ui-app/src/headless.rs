//! Driving a `ui` app without a display: the names an inspector, a test, a capture, a script and
//! the CLI read a driven app through, beside the [`Driver`] that runs it.
//!
//! Every app launched through the standard lifecycle takes a `--headless-drive '<json>'` flag:
//! ```text
//! ebook --headless-drive '{"actions":[{"click_named":"library_item[0]"}]}'
//! ebook --headless-drive '{"dump":"/tmp/after.json"}'
//! ```
//!
//! The first process changes state and exits (flushing its store); the
//! second loads the store and dumps the document as JSON. Diffing the two
//! dumps is the persistence proof for the app's document.

pub use crate::app::driver::controls::{
    CONTROL_RESOURCE_PREFIX, Control, ControlInput, agent_acting,
};

pub use crate::app::driver::drive::{
    Bindings, Outcome, Step, StepEntry, StepStatus, drive, resource_json, steps_of,
};

pub use crate::app::driver::fixtures::FIXTURE_PICTURE;
#[cfg(any(test, feature = "test-support"))]
pub use crate::app::driver::fixtures::MOUNT_VIEWPORT;
#[cfg(any(test, feature = "test-support"))]
#[cfg(feature = "model")]
pub use crate::app::driver::fixtures::fixture_runner_inputs;

pub use crate::app::driver::frame::{
    CAPTURE_FRAME_INTERVAL, FrameDemand, FrameOutput, FrameRenderInput, FrameSchedule,
    capture_frames, frame_widget,
};

pub use crate::app::driver::metrics::RelayoutStats;
pub use crate::app::driver::viewport;

#[cfg(feature = "model")]
pub use crate::app::driver::FollowedView;
pub use crate::app::driver::{
    Driver, DriverInputs, FrameOptions, Framed, OwedSettle, WINDOW_PARTS, WindowBarNotice,
    WindowBarNoticeButton,
};
#[cfg(feature = "model")]
pub use crate::app::driver::{ShownDevice, ShownHost};

pub use crate::app::driver::chrome::SidebarShown;

#[cfg(any(test, feature = "test-support"))]
pub use crate::app::driver::test_support;

pub use crate::inspection::{
    ACCESSIBILITY_AUDIT_VERSION, AccessibilityAudit, AccessibilityIssue, InspectionNode,
    InspectionSnapshot, NATIVE_APP_AUDIT_VERSION, NativeAppAudit, NativeAppIssue,
};

pub use crate::inspection::{InspectionPatchStats, write_inspection_patch};

pub(crate) use crate::inspection::{
    audit_inspection_node, collect_inspection, collect_named_targets, collect_targets,
    compare_expectation, effect_control_ids, find_semantic_target, inspection_has_semantics,
};

/// Viewport a driven frame lays out at unless a spec overrides it, as the launch code defines it.
pub use crate::launch::DEFAULT_VIEWPORT;

pub use tilo_headless_schema::{
    Action, Capture, Invocation, Press, PressVerb, SemanticAction, SemanticExpectation, Spec,
    parse_spec,
};

/// The widget at a desktop-sized window, laid out in the light theme: a described component's
/// instance as the component tests read it.
#[cfg(any(test, feature = "test-support"))]
pub fn harness<W: crate::widget::Widget + 'static>(root: W) -> Driver {
    Driver::for_root(
        crate::State::new(),
        root,
        (900, 600),
        tilo_ui_base::theme::Theme::light(),
    )
}
