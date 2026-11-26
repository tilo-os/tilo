//! The History fixtures the app runner's tests share: an app whose framework History has two
//! committed changes, laid out and painted as the native runner does.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::wildcard_imports,
    reason = "the app runner's tests and the toolkit's above it use different helpers of this module; a fixture's unwraps are the assertion that it built"
)]

use super::*;
use crate::State;
use crate::history_mode::*;
use crate::host_chrome::model::HostCommand;
use automerge::{ROOT, transaction::Transactable};

pub static HISTORY: HistoryDefinition = HistoryDefinition {
    app_id: "dev.tilo.native-history-fixture",
    scopes: &[HistoryScopeDefinition {
        key: "document",
        title: "Native notebook",
        presentation: HistoryPresentation::Domain {
            create: crate::history_mode::test_preview,
        },
    }],
};
/// An app whose registered framework History has two committed changes.
pub fn app() -> App {
    app_on(historical_state())
}
/// A state whose framework History has two committed changes.
pub fn historical_state() -> State {
    let mut state = State::new();
    state.doc.put(ROOT, "body", "old").unwrap();
    state.commit_action(None, None);
    state.doc.put(ROOT, "body", "live").unwrap();
    state.commit_action(None, None);
    state
}
/// An app on `state` whose History is the framework's registered History.
pub fn app_on(state: State) -> App {
    app_with_root(state, Box::new(crate::widget::Column::new()))
}
/// [`app_with_root`]'s managed root: `root` under the framework's registered History.
pub fn history_root(root: Box<dyn tilo_ui_base::widget::Widget>) -> ManagedRoot {
    let registration = HistoryRegistration::new(
        &HISTORY,
        vec![HistorySourceRegistration::HostDocument {
            scope: "document",
            resource: HistoryResource::new(
                "native-fixture",
                "lifetime-904",
                HISTORY.app_id,
                crate::DocumentId::for_tab(904),
            ),
        }],
    )
    .unwrap();
    ManagedRoot::new(root, registration)
}
/// An app on `state` showing `root` under the framework's registered History.
pub fn app_with_root(state: State, root: Box<dyn tilo_ui_base::widget::Widget>) -> App {
    App::from_managed_root_with_controlled_inputs(state, history_root(root)).unwrap()
}
/// Lay out and paint one 900×650 frame, as the native runner does.
#[cfg(feature = "native")]
pub fn layout(app: &mut App) {
    app.init_layout();
    let root = app.layout.root_node.unwrap();
    assert!(run_layout_pass(app, root, 900, 650, 1.0, true, None).is_some());
    let mut scene = vello::Scene::new();
    draw_frame_content(app, &mut scene, 900, 650, 1.0, None);
    complete_native_frame_content(app, |_| {});
}
/// [`app`] where the host's own chrome presents History, laid out.
/// An app whose platform menus list its History, served the history of its document.
#[cfg(feature = "native")]
pub fn native_chrome_app() -> App {
    let mut app = app();
    app.prepare_native_presentation(crate::host_chrome::HostChrome::Native);
    layout(&mut app);
    app
}

/// The host commands a native chrome presents, as the tests above the toolkit ask for them.
impl App {
    #[doc(hidden)]
    pub fn host_commands_for_test(&mut self, with_recent_changes: bool) -> Vec<HostCommand> {
        self.host_commands(with_recent_changes)
    }

    #[doc(hidden)]
    pub fn invoke_host_command_for_test(
        &mut self,
        invocation: &tilo_actions::ControlInvocation,
    ) -> bool {
        self.invoke_host_command(invocation)
    }

    #[doc(hidden)]
    pub fn invoke_host_shortcut_for_test(
        &mut self,
        service: tilo_ui_base::platform::HostShortcut,
    ) -> bool {
        self.invoke_host_shortcut(service)
    }

    #[doc(hidden)]
    pub fn native_input_snapshot_for_test(&self) -> Option<serde_json::Value> {
        self.native_input_snapshot()
    }

    #[doc(hidden)]
    pub fn native_admission_snapshot_for_test(
        &self,
    ) -> tilo_startup_metrics::NativeAdmissionSnapshot {
        self.native_admission_snapshot()
    }
}

/// One prepared component's frame: laid out and drawn as a window's runner does, without a window.
#[cfg(feature = "native")]
pub fn draw_prepared_component_test_frame(app: &mut App) {
    super::draw_prepared_component_test_frame(app);
}
