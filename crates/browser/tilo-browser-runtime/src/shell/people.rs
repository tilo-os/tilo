//! The people overlay and following, in the page.
//!
//! Pinned out of the People popover, the roster floats over the windows
//! (`tilo_desktop::people_overlay`), waiting at an edge as a desk widget
//! does and kept there in the person's settings as the widgets keep
//! theirs. Its peek draws each person's screen from their live layout,
//! the windows on records this page holds pictured live from its own
//! replica. Following someone brings forward, here, the window of what
//! they are in each time it changes, under the same banner the native
//! shell shows; the person's own input takes their view back.
//!
//! The overlay's code may come in its own module the first time the roster
//! is pinned ([`crate::pinned_roster`]); following and the banner are here
//! from the start.

use settings_store::desk_widgets::{PEOPLE_OVERLAY, PeopleOverlayPrefs};
use tilo_desktop::collaborators::RosterRow;
use tilo_desktop::desk_edges::DockEdge;
use tilo_desktop::follow_banner::{BannerFrame, FollowBanner};
use tilo_desktop::here::Person;
use tilo_desktop::people_overlay::{LivePictures, PeopleHit, PeopleOverlayModel, peer_screens};
use tilo_desktop_model::{DesktopTree, TabId};
use tilo_desktop_surface::DesktopHandle;
use ui::kurbo::{Affine, Point, Rect};
use ui::tokens::{EffectiveMotion, MotionSource};
use ui::vello::Scene;
use ui::winit::event::{ElementState, Modifiers, WindowEvent};
use ui::winit::keyboard::{Key, NamedKey};
use ui::{TextRenderer, Theme};

use tilo_session_core::presence::{Follow, Place, Step, Target};

use crate::pinned_roster::PinnedRoster;

/// What the overlay and the banner need from the page this frame.
pub(super) struct Frame<'a> {
    /// The page's desktop, the bar included.
    pub rect: Rect,
    pub rows: &'a [RosterRow],
    /// This page's person, whose own row leads the list.
    pub own: Option<&'a RosterRow>,
    pub people: &'a [Person],
    pub tree: &'a DesktopTree,
    /// The records this page shows, by the window showing each, as
    /// presence names them: `(tab, app, its short digest)`.
    pub records: &'a [(TabId, String, [u8; 8])],
    pub cursor: Option<Point>,
    /// Where the windows' content starts, below their bars: the banner
    /// hangs there, as the page's notices do, clear of the windows' own
    /// controls.
    pub content_top: f64,
}

/// The overlay, following and the banner across frames.
pub(super) struct PagePeople {
    /// The pinned roster, once its code is in and it was first pinned.
    overlay: Option<Box<dyn PinnedRoster>>,
    /// Whether the roster is pinned (shown once its code is in).
    pinned: bool,
    edge: DockEdge,
    banner: FollowBanner,
    /// Whom this page follows, and what of their place it already shows: the same machine the native session runs.
    follow: Follow,
    motion: EffectiveMotion,
    /// The desktop it was last drawn over.
    rect: Rect,
    /// What assistive technology pressed since the host last looked.
    pressed: Vec<PeopleHit>,
    /// The bar's people panel as a reader finds it while it is open (the
    /// host paints it), and the control pressed on it since the host last
    /// looked, by its target.
    panel: Option<ui::a11y::AccessNode>,
    panel_pressed: Option<String>,
}

impl PagePeople {
    /// As the person's settings left it: pinned or not, at its edge.
    pub(super) fn new() -> Self {
        let prefs = PEOPLE_OVERLAY.get();
        Self {
            overlay: None,
            pinned: prefs.pinned,
            edge: prefs.edge,
            banner: FollowBanner::default(),
            follow: Follow::default(),
            motion: EffectiveMotion::default(),
            rect: Rect::ZERO,
            pressed: Vec::new(),
            panel: None,
            panel_pressed: None,
        }
    }

    /// What the bar's people panel shows a reader now; `None` once closed.
    pub(super) fn set_panel(&mut self, panel: Option<ui::a11y::AccessNode>) {
        self.panel = panel;
    }

    /// The panel control a reader pressed since the last call, by target.
    pub(super) fn take_panel_pressed(&mut self) -> Option<String> {
        self.panel_pressed.take()
    }

    /// What assistive technology pressed on the overlay since the last
    /// call, for the host to apply as it applies a press.
    pub(super) fn take_pressed(&mut self) -> Vec<PeopleHit> {
        std::mem::take(&mut self.pressed)
    }

    pub(super) fn is_pinned(&self) -> bool {
        self.pinned
    }

    /// The roster while pinned: made the first time its code is in (asking
    /// for it until then).
    fn roster(&mut self) -> Option<&mut (dyn PinnedRoster + 'static)> {
        if self.overlay.is_none() && self.pinned {
            let make = crate::pinned_roster::code()?;
            let mut overlay = make(self.edge);
            overlay.show(true);
            self.overlay = Some(overlay);
        }
        self.overlay.as_deref_mut()
    }

    /// The roster as it is now, if made.
    fn made(&self) -> Option<&dyn PinnedRoster> {
        self.overlay.as_deref()
    }

    /// Pin the roster to the desk, or put it back in the bar; kept.
    pub(super) fn pin(&mut self, pinned: bool) {
        self.pinned = pinned;
        if let Some(overlay) = self.overlay.as_deref_mut() {
            overlay.show(pinned);
        }
        if let Err(error) = PEOPLE_OVERLAY.update(|prefs| PeopleOverlayPrefs { pinned, ..prefs }) {
            web_sys::console::warn_1(&format!("keeping the people overlay's pin: {error}").into());
        }
    }

    pub(super) fn followed(&self) -> Option<&str> {
        self.follow.followed()
    }

    /// Follow `id`, or stop following them. The banner finds their row in
    /// the next frame's roster.
    pub(super) fn toggle_follow(&mut self, id: &str) {
        if self.follow.followed() == Some(id) {
            self.stop_following(false);
            return;
        }
        self.follow.follow(Some(id.to_owned()));
        self.banner
            .started(None, self.motion, MotionSource::Pointer, ui::time::now());
    }

    /// Stop following; `by_own_input` when the person took their view back
    /// by doing something of their own (the banner says so a moment).
    pub(super) fn stop_following(&mut self, by_own_input: bool) {
        if !self.follow.follow(None) {
            return;
        }
        self.banner.ended(
            by_own_input,
            self.motion,
            MotionSource::Pointer,
            ui::time::now(),
        );
    }

    /// While following: bring forward the window of what they are in, each
    /// time that changes; where no window here shows it (a workspace whose
    /// layout is each person's own), `open_app` shows their app here.
    /// Returns whether anything was brought forward.
    pub(super) fn track(
        &mut self,
        people: &[Person],
        handle: &DesktopHandle,
        open_app: &dyn Fn(&str) -> bool,
    ) -> bool {
        let places: Vec<Place<'_>> = people
            .iter()
            .map(|person| Place {
                id: &person.id,
                tab: person.tab,
                app: person.followed_app(),
            })
            .collect();
        match self.follow.step(&places) {
            Step::Bring(Target::Window(tab)) => handle.focus_tab(tab),
            Step::Bring(Target::App(app)) => open_app(&app),
            Step::Gone => {
                // They left: following them ends with them.
                self.stop_following(false);
                false
            }
            Step::Rest => false,
        }
    }

    pub(super) fn needs_animation_frame(&self) -> bool {
        self.made().is_some_and(PinnedRoster::needs_animation_frame)
            || self.banner.is_moving(ui::time::now())
    }

    pub(super) fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        [
            self.made().and_then(PinnedRoster::next_wake_deadline),
            self.banner.next_wake_deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Whether `point` is on the overlay, its peek, or the banner.
    pub(super) fn contains(&self, point: Point) -> bool {
        self.made().is_some_and(|overlay| overlay.contains(point))
            || self.banner.chip_rect().contains(point)
    }

    pub(super) fn wants_pointer_capture(&self) -> bool {
        self.made().is_some_and(PinnedRoster::wants_pointer_capture)
    }

    pub(super) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        frame: &Frame<'_>,
    ) {
        self.motion = theme.motion();
        self.rect = frame.rect;
        let bar = f64::from(tilo_desktop::topbar::topbar_height(theme));
        let below_bar = Rect::new(
            frame.rect.x0,
            frame.rect.y0 + bar,
            frame.rect.x1,
            frame.rect.y1,
        );
        let following = self
            .follow
            .followed()
            .map(|id| frame.rows.iter().find(|row| row.id == id).cloned());
        let kept = self.banner.draw(
            scene,
            transform,
            text,
            theme,
            BannerFrame {
                work_area: below_bar,
                viewport: Rect::new(
                    below_bar.x0,
                    below_bar.y0.max(frame.content_top),
                    below_bar.x1,
                    below_bar.y1,
                ),
                window_bars: &[],
                cursor: frame.cursor,
                motion: self.motion,
            },
            following,
        );
        if !kept {
            self.stop_following(false);
        }
        let followed = self.follow.followed().map(str::to_owned);
        let Some(overlay) = self.roster().filter(|overlay| overlay.is_visible()) else {
            ui::window_pictures::want(&[]);
            return;
        };
        let screens = peer_screens(frame.people, frame.tree, frame.records, &|app| {
            crate::desktop::known_application(app).map(str::to_owned)
        });
        // Live pictures only of the windows the open peek shows.
        let wanted: Vec<u64> = overlay
            .peeked()
            .and_then(|id| screens.iter().find(|screen| screen.id == id))
            .into_iter()
            .flat_map(|screen| &screen.windows)
            .filter_map(|window| window.picture)
            .collect();
        ui::window_pictures::want(&wanted);
        overlay.draw(
            scene,
            transform,
            text,
            theme,
            &PeopleOverlayModel {
                host: below_bar,
                occupied: &[],
                rows: frame.rows,
                screens: &screens,
                followed_id: followed.as_deref(),
                own: frame.own,
            },
            &mut LivePictures,
        );
    }

    /// Route `event`; marks it handled when it was the overlay's or the
    /// banner's. Returns what the host applies.
    pub(super) fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        cursor: Option<Point>,
        handled: &mut bool,
    ) -> Option<PeopleHit> {
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            ..
        } = event
            && let Some(point) = cursor
            && self.banner.stop_rect().contains(point)
        {
            self.stop_following(false);
            *handled = true;
            return None;
        }
        let hit = self
            .overlay
            .as_deref_mut()
            .and_then(|overlay| overlay.handle_event(event, scale, handled));
        if let Some(PeopleHit::Moved(edge)) = &hit {
            self.edge = *edge;
        }
        if let Some(PeopleHit::Moved(edge)) = &hit
            && let Err(error) = PEOPLE_OVERLAY.update(|prefs| PeopleOverlayPrefs {
                edge: *edge,
                ..prefs
            })
        {
            web_sys::console::warn_1(&format!("keeping the people overlay's edge: {error}").into());
        }
        hit
    }

    /// A key, before the desktop's: the overlay's while it holds the
    /// keyboard; while following, Escape stops and any other key takes the
    /// view back. Returns whether the key was taken.
    pub(super) fn key(
        &mut self,
        event: &WindowEvent,
        modifiers: &Modifiers,
    ) -> (bool, Option<PeopleHit>) {
        let WindowEvent::KeyboardInput { event: key, .. } = event else {
            return (false, None);
        };
        if key.state != ElementState::Pressed {
            return (self.made().is_some_and(PinnedRoster::composing), None);
        }
        if let Some(hit) = self
            .overlay
            .as_deref_mut()
            .and_then(|overlay| overlay.key(&key.logical_key, modifiers.state()))
        {
            return (true, hit);
        }
        if self.follow.followed().is_some() {
            let escape = matches!(key.logical_key, Key::Named(NamedKey::Escape));
            self.stop_following(!escape);
            return (escape, None);
        }
        (false, None)
    }

    /// A press on the desktop, not on the overlay or the banner: the
    /// person going back to their own view.
    pub(super) fn pressed_elsewhere(&mut self) {
        self.stop_following(true);
    }
}

/// The overlay and the banner as assistive technology reads them. The
/// host paints them and routes their pointer and keys itself; as a widget
/// they only say what is there and take what a reader presses.
impl ui::widget::Widget for PagePeople {
    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        _theme: &Theme,
        map: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn ui::widget::Widget>,
    ) -> ui::taffy::NodeId {
        use ui::widget::TaffyTreeExt as _;
        let style = ui::taffy::Style {
            position: ui::taffy::Position::Absolute,
            ..Default::default()
        };
        taffy.widget_node(style, &[], map, self)
    }

    fn apply_layout(
        &mut self,
        _taffy: &ui::taffy::TaffyTree<()>,
        _node: ui::taffy::NodeId,
        _origin: Point,
    ) {
    }

    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text: &mut TextRenderer,
        _theme: &Theme,
        _presence: &ui::presence::PresenceCtx,
    ) {
    }

    fn handle_event(&mut self, _event: &WindowEvent, _ctx: &mut ui::widget::EventCtx<'_>) {}

    fn children(&self) -> Vec<&dyn ui::widget::Widget> {
        Vec::new()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn ui::widget::Widget + '_)> {
        Vec::new()
    }

    fn needs_redraw(&self) -> bool {
        false
    }

    fn is_layout_dirty(&self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "PeopleOverlay"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// The bar's people panel while open, the pinned roster (who is here,
    /// each with their controls) and the banner of whom this view follows,
    /// with its Stop.
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Role};
        let mut children: Vec<AccessNode> = self.panel.iter().cloned().collect();
        if let Some(overlay) = self.made().filter(|overlay| overlay.is_visible()) {
            children.push(
                AccessNode::new(Role::Group)
                    .with_hashed_key("people-overlay")
                    .with_name(tilo_desktop_surface::i18n::t("a11y-people-overlay"))
                    .with_children(overlay.accessibility_children()),
            );
        }
        children.extend(self.banner.accessibility());
        if children.is_empty() {
            return None;
        }
        Some(AccessNode::new(Role::Group).with_children(children))
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        _state: &mut ui::widget::Edits<'_>,
        _path: &ui::widget::ModelPath,
    ) -> bool {
        if target.starts_with("people:") {
            if matches!(action, ui::a11y::Action::Activate) {
                self.panel_pressed = Some(target.to_owned());
            }
            return self.panel.is_some();
        }
        if target == tilo_desktop::follow_banner::STOP_TARGET {
            if matches!(action, ui::a11y::Action::Activate) {
                self.stop_following(false);
            }
            return true;
        }
        let Some(overlay) = self.overlay.as_deref_mut() else {
            return false;
        };
        let ours = overlay
            .accessibility_children()
            .iter()
            .any(|node| node.action_target.as_deref() == Some(target));
        if let Some(hit) = overlay.perform_semantic_action(action, target) {
            self.pressed.push(hit);
        }
        ours
    }
}
