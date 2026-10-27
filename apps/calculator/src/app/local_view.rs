//! What a Calculator window shows, kept as the window's own presentation
//! ([`ui::application_view::ApplicationViewState`]): its mode, the graphs'
//! views and which row each graph has selected. The calculations and the
//! graphs' rows are the workspace's record; this is how one window looks at
//! them. A reopened window shows what it showed, and History opens a past
//! version as the window shows it, its graphs drawn by the same view.

use ui::application_view::{ApplicationFocusRestore, ApplicationLocalView, ApplicationViewState};
use ui::view::ThawError;

use super::CalculatorApp;
use crate::model::{Mode, View2d, View3d};

/// The version of the payload this build writes and reads.
const VERSION: u32 = 1;

#[derive(serde::Serialize, serde::Deserialize)]
struct CalculatorView {
    mode: usize,
    view2d: [f64; 3],
    view3d: [f64; 3],
    active_fn: usize,
    active_surface: usize,
}

fn app_id() -> &'static str {
    <crate::resource::CalculatorShell as ui::framework::ResourceApp>::ID
}

impl CalculatorView {
    fn of(app: &CalculatorApp) -> Self {
        let model = &app.model;
        Self {
            mode: model.mode.index(),
            view2d: [model.view2d.cx, model.view2d.cy, model.view2d.scale],
            view3d: [model.view3d.yaw, model.view3d.pitch, model.view3d.zoom],
            active_fn: model.active_fn,
            active_surface: model.active_surface,
        }
    }

    fn decode(local: &ApplicationLocalView) -> Result<Self, ThawError> {
        if local.app_id != app_id() {
            return Err(ThawError::BindingMismatch);
        }
        if local.version != VERSION {
            return Err(ThawError::UnsupportedVersion {
                found: local.version,
                supported: VERSION,
            });
        }
        let view: Self = serde_json::from_slice(&local.payload)
            .map_err(|error| ThawError::State(error.to_string()))?;
        let finite = view
            .view2d
            .iter()
            .chain(&view.view3d)
            .all(|v| v.is_finite());
        if !finite || view.view2d[2] <= 0.0 || view.view3d[2] <= 0.0 || view.mode > 2 {
            return Err(ThawError::InvalidView(
                "calculator view out of range".into(),
            ));
        }
        Ok(view)
    }
}

impl ApplicationViewState for CalculatorApp {
    fn capture_application_view(&self) -> Result<Option<ApplicationLocalView>, ThawError> {
        Ok(Some(ApplicationLocalView {
            app_id: app_id().into(),
            version: VERSION,
            payload: serde_json::to_vec(&CalculatorView::of(self))
                .map_err(|error| ThawError::State(error.to_string()))?,
        }))
    }

    fn restore_application_view(
        &mut self,
        local: &ApplicationLocalView,
        _doc: &automerge::AutoCommit,
    ) -> Result<ApplicationFocusRestore, ThawError> {
        let view = CalculatorView::decode(local)?;
        let [cx, cy, scale] = view.view2d;
        let [yaw, pitch, zoom] = view.view3d;
        self.model.view2d = View2d { cx, cy, scale };
        self.model.view3d = View3d { yaw, pitch, zoom };
        // Clamped to the rows the record has once they are presented.
        self.model.active_fn = view.active_fn;
        self.model.active_surface = view.active_surface;
        // The mode only: a window that shows a graph adds no starter row.
        self.set_mode(Mode::from_index(view.mode));
        self.relayout();
        self.dirty = true;
        Ok(ApplicationFocusRestore::Generic)
    }
}
