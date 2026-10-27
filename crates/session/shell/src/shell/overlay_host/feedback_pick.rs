//! "Report a problem…" on the native desktop: the avatar menu's entry and
//! the Report problem shortcut open [`crate::shell::feedback_pick`]'s picker
//! over the whole screen; the windows other processes draw are asked over
//! their shell-ui connections, and the composer opens on the report the
//! picks become.

use ui::floating_surface::FloatingSurfaceHostMessage;
use ui::pick_wire::PickAnswer;
use ui::time::Instant;
use vello::kurbo::Point;
use winit::event::WindowEvent;

use super::ShellOverlayHost;
use crate::shell::feedback_pick::{PickWindow, ReportContext};

impl ShellOverlayHost {
    /// Whether reports have a destination here: the avatar menu offers
    /// "Report a problem…" and the shortcut opens the picker only then.
    pub(crate) fn feedback_configured(&self) -> bool {
        self.chrome
            .as_ref()
            .is_some_and(|chrome| crate::shell::feedback_entry::configured(&chrome.borrow()))
    }

    /// Point at what is wrong: the picker over the screen, where reports
    /// have a destination.
    pub(crate) fn open_feedback_picker(&mut self) {
        if !self.feedback_configured() || self.feedback_picker.is_open() {
            return;
        }
        self.close_bar_popovers_for(None);
        let windows = self.pick_windows();
        self.feedback_picker.open(windows, ui::time::now());
        self.bump_chrome();
    }

    /// Whether the person is picking: the shell takes every key and the
    /// pointer everywhere until they finish or leave.
    pub(crate) fn feedback_picker_open(&self) -> bool {
        self.feedback_picker.is_open()
    }

    /// The windows on screen another process draws, front-most first.
    fn pick_windows(&self) -> Vec<PickWindow> {
        let Some(chrome) = self.chrome.as_ref() else {
            return Vec::new();
        };
        let shown: Vec<_> = {
            let state = chrome.borrow();
            let tree = state.tree();
            crate::shell::input_policy::foreign_tiles(&tree, self)
        };
        let apps: Vec<_> = {
            let state = chrome.borrow();
            shown
                .into_iter()
                .filter_map(|hit| {
                    let app = state.group_app_id(hit.strip, hit.group)?;
                    Some((hit, app.as_str().to_owned()))
                })
                .collect()
        };
        apps.into_iter()
            .map(|(hit, app)| {
                let (x, y) = crate::shell::foreign::store().corner_origin(hit.surface_id, hit.rect);
                PickWindow {
                    surface: hit.surface_id,
                    rect: hit.rect,
                    origin: Point::new(x, y),
                    // The app's display name, or none: a report never
                    // names an app by its id, which only routes.
                    name: crate::shell::launcher::display_name_for_app_id(&app).unwrap_or_default(),
                    record: self
                        .selection_requests
                        .window_of(&app)
                        .map(|resource| resource.kind.clone()),
                    connections: self.selection_requests.connections_of(&app),
                    app,
                }
            })
            .collect()
    }

    /// Take `event` while picking; whether it was taken.
    pub(super) fn route_feedback_picker_event(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::keyboard::ModifiersState,
    ) -> bool {
        if !self.feedback_picker.is_open() {
            return false;
        }
        let cursor = self.pointer_position();
        let mut picker = std::mem::take(&mut self.feedback_picker);
        let taken = picker.handle_event(event, cursor, modifiers, &*self, ui::time::now());
        self.feedback_picker = picker;
        self.follow_feedback_picker();
        self.bump_chrome();
        taken
    }

    /// Advance the picker: give up on windows that did not answer, open the
    /// composer on a finished report, and the questions to send to which
    /// shell-ui connections now.
    pub(crate) fn route_feedback_pick(
        &mut self,
        now: Instant,
    ) -> Vec<(u64, FloatingSurfaceHostMessage)> {
        let mut picker = std::mem::take(&mut self.feedback_picker);
        let changed = picker.tick(&*self, now);
        self.feedback_picker = picker;
        if changed {
            self.follow_feedback_picker();
            self.bump_chrome();
        }
        self.feedback_picker.take_outbox()
    }

    /// `app_id` (as the launcher vouched for it) answered `request`;
    /// whether anything changed.
    pub(crate) fn receive_pick_answer(
        &mut self,
        app_id: &str,
        request: u64,
        answer: PickAnswer,
    ) -> bool {
        let mut picker = std::mem::take(&mut self.feedback_picker);
        let changed = picker.answered(app_id, request, answer, &*self, ui::time::now());
        self.feedback_picker = picker;
        if changed {
            self.follow_feedback_picker();
            self.bump_chrome();
        }
        changed
    }

    /// When the picker next needs [`Self::route_feedback_pick`].
    pub(crate) fn feedback_pick_deadline(&self, now: Instant) -> Option<Instant> {
        self.feedback_picker.deadline(now)
    }

    /// Hand a finished report to the composer, and bring it up.
    fn follow_feedback_picker(&mut self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let names = {
            let state = chrome.borrow();
            let own = crate::shell::topbar_chrome::own_person_name(&state);
            let local_user = state.active_local_user();
            let (rows, own) = tilo_desktop::collaborators::collaborator_roster_rows_named(
                &state.tree().desktop,
                &local_user,
                Some(own.as_str()),
            );
            rows.into_iter()
                .filter(|row| !row.is_agent)
                .map(|row| row.label)
                .chain(own)
                .collect()
        };
        let Some(draft) = self.feedback_picker.take_report(&ReportContext { names }) else {
            return;
        };
        // The picture of the picked parts is not taken on this desktop yet:
        // the report carries their texts and places.
        let picked = tilo_app::feedback::PickedReport {
            draft,
            crops: Vec::new(),
        };
        if let Err(error) = crate::shell::feedback_entry::report(&mut chrome.borrow_mut(), picked) {
            tracing::debug!(?error, "feedback report did not open");
        }
    }

    /// Draw the picker over everything while it is open.
    pub(super) fn draw_feedback_picker(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
    ) {
        let bounds = self.rect;
        self.feedback_picker
            .draw(scene, transform, text, theme, bounds, ui::time::now());
    }

    /// Note how long drawing the shell took, for "this was slow".
    pub(super) fn feedback_frame_built(&mut self, since: Instant) {
        let built = ui::time::now().saturating_duration_since(since);
        self.feedback_picker
            .frame_built(u32::try_from(built.as_micros()).unwrap_or(u32::MAX));
    }
}
