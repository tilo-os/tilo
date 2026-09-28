//! The shared workspace-settings lens and People popover over the browser
//! desktop. This peer never governs a room, so both are read-only views of
//! the admitted desktop: policy controls paint disabled and roster actions
//! that need a governor are absent.

use tilo_desktop::collaborators::{
    CollaboratorsHit, CollaboratorsModel, CollaboratorsOverlay, collaborator_roster_rows,
};
use tilo_desktop::workspace_settings::overlay::{
    EventOutcome, WorkspaceSettingsModel, WorkspaceSettingsOverlay,
};
use tilo_desktop::workspace_settings::{GrantDraft, NoAppDirectory, WsTab};
use tilo_desktop_model::DesktopTree;
use ui::clock::Instant;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

/// What a consumed event asks the desktop host to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OverlayRequest {
    /// Open the active workspace's Activity.
    OpenActivity,
}

struct SettingsLens {
    overlay: WorkspaceSettingsOverlay,
    tab: WsTab,
    open: bool,
}

#[derive(Default)]
pub(crate) struct DesktopOverlays {
    settings: Option<SettingsLens>,
    collaborators: Option<CollaboratorsOverlay>,
    cursor: Option<Point>,
    /// Something changed that the next frame must paint.
    changed: bool,
}

impl DesktopOverlays {
    pub(crate) fn toggle_settings(&mut self) {
        self.changed = true;
        match &mut self.settings {
            Some(lens) => lens.open = !lens.open,
            None => {
                self.collaborators = None;
                self.settings = Some(SettingsLens {
                    overlay: WorkspaceSettingsOverlay::default(),
                    tab: WsTab::General,
                    open: true,
                });
            }
        }
    }

    /// Open the workspace settings on `tab`, as its top-bar control and
    /// tab row do.
    pub(crate) fn open_settings(&mut self, tab: WsTab) {
        self.changed = true;
        self.collaborators = None;
        match &mut self.settings {
            Some(lens) => {
                lens.open = true;
                lens.tab = tab;
            }
            None => {
                self.settings = Some(SettingsLens {
                    overlay: WorkspaceSettingsOverlay::default(),
                    tab,
                    open: true,
                });
            }
        }
    }

    pub(crate) fn close_settings(&mut self) {
        if let Some(lens) = self.settings.as_mut().filter(|lens| lens.open) {
            lens.open = false;
            self.changed = true;
        }
    }

    /// The tab the workspace settings show, while they are open.
    pub(crate) fn settings_tab(&self) -> Option<WsTab> {
        self.settings
            .as_ref()
            .filter(|lens| lens.open)
            .map(|lens| lens.tab)
    }

    pub(crate) fn toggle_collaborators(&mut self) {
        self.changed = true;
        if self.collaborators.take().is_none() {
            if let Some(lens) = &mut self.settings {
                lens.open = false;
            }
            self.collaborators = Some(CollaboratorsOverlay::default());
        }
    }

    /// Whether an overlay is painted and must be drawn again.
    pub(crate) fn needs_redraw(&self) -> bool {
        self.changed
            || self
                .settings
                .as_ref()
                .is_some_and(|lens| lens.overlay.is_animating(Instant::now()))
    }

    fn is_open(&self) -> bool {
        self.settings.as_ref().is_some_and(|lens| lens.open) || self.collaborators.is_some()
    }

    pub(crate) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        viewport: Rect,
        (tree, local_user): (&DesktopTree, &String),
    ) {
        self.changed = false;
        if let Some(lens) = &mut self.settings {
            lens.overlay.layout(viewport);
            match lens
                .overlay
                .begin_draw(scene, transform, theme, lens.open, None)
            {
                Some(plan) => {
                    lens.overlay.draw_contents(
                        scene,
                        plan.transform,
                        text,
                        theme,
                        &WorkspaceSettingsModel {
                            desktop: tree,
                            tab: lens.tab,
                            grant_draft: GrantDraft::default(),
                            local_user,
                            acting_user: local_user,
                            editing_name: None,
                            name_selection: ui::text_selection::Selection::default(),
                            governs_active_room: false,
                            owner_controls: false,
                            apps: &NoAppDirectory,
                        },
                    );
                    WorkspaceSettingsOverlay::finish_draw(scene, plan);
                }
                None => self.settings = None,
            }
        }
        if let Some(overlay) = &mut self.collaborators {
            let rows = collaborator_roster_rows(tree, local_user);
            let own_initial = local_user
                .chars()
                .next()
                .unwrap_or('\u{2022}')
                .to_ascii_uppercase();
            let (red, green, blue) = ui::presence::user_color(local_user);
            overlay.draw(
                scene,
                transform,
                text,
                theme,
                &CollaboratorsModel {
                    host: viewport,
                    rows: &rows,
                    own_initial,
                    own_color: vello::peniko::Color::from_rgb8(red, green, blue),
                    unavailable: None,
                    may_invite: false,
                    followed_id: None,
                    following_pairs: &[],
                    own_status: None,
                    own_activity: None,
                    own_status_automatic: false,
                    status_sharing: ui::StatusSharing::default(),
                    status_editing: false,
                    screen_exposure: None,
                },
                self.cursor,
            );
        }
    }

    /// Route `event` to an open overlay. Returns whether it consumed the
    /// event, and what it asks of the host.
    pub(crate) fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        member_count: impl FnOnce() -> usize,
    ) -> (bool, Option<OverlayRequest>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Some(Point::new(position.x / scale, position.y / scale));
        }
        if !self.is_open() {
            return (false, None);
        }
        match event {
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.logical_key == Key::Named(NamedKey::Escape) =>
            {
                self.close();
                (true, None)
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let Some(point) = self.cursor else {
                    return (true, None);
                };
                self.changed = true;
                (true, self.press(point, member_count))
            }
            WindowEvent::KeyboardInput { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. } => (true, None),
            _ => (false, None),
        }
    }

    fn press(
        &mut self,
        point: Point,
        member_count: impl FnOnce() -> usize,
    ) -> Option<OverlayRequest> {
        if let Some(overlay) = &mut self.collaborators {
            match overlay.hit_test(point) {
                CollaboratorsHit::Dismiss => self.collaborators = None,
                CollaboratorsHit::RoomActivity => {
                    self.collaborators = None;
                    return Some(OverlayRequest::OpenActivity);
                }
                CollaboratorsHit::PreviousPeople => {
                    overlay.change_people_page(false, member_count());
                }
                CollaboratorsHit::NextPeople => overlay.change_people_page(true, member_count()),
                _ => {}
            }
            return None;
        }
        if let Some(lens) = self.settings.as_mut().filter(|lens| lens.open) {
            if !lens.overlay.rect().contains(point) {
                lens.open = false;
                return None;
            }
            match lens.overlay.hit_test(point, lens.tab) {
                EventOutcome::Close => lens.open = false,
                EventOutcome::SelectTab(tab) => lens.tab = tab,
                _ => {}
            }
        }
        None
    }

    fn close(&mut self) {
        self.changed = true;
        self.collaborators = None;
        if let Some(lens) = &mut self.settings {
            lens.open = false;
        }
    }
}
