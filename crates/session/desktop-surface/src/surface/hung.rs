//! "Not responding": a window whose app stopped answering says so, and the
//! person decides whether to keep waiting or quit it.
//!
//! An app that locks up leaves its window on the desktop as it last drew it,
//! taking presses that never come back and looking like any other window. The
//! compositor notices the silence — it pings each app it shows and expects an
//! answer — and tells the desk which windows those are
//! ([`DesktopHandle::set_unresponsive_windows`]). This card is what the person
//! sees on such a window: what happened, and their two ways out. It sits over
//! the window, takes the presses that land on it, and goes as soon as the app
//! answers again or quitting it closes the window.
//!
//! What the person chooses is queued on the handle
//! ([`DesktopHandle::take_liveness_choices`]) and carried out by the host that
//! owns the compositor, the same way a script or the shell's own view reaches
//! it: through the desk's `wait-window` and `force-quit-app` verbs.

use std::cell::Cell;

use tilo_desktop_model::TabId;
use ui::TextRole;
use ui::a11y::{AccessNode, AccessState, Role};
use ui::icons::{self, Icon};
use ui::taffy::{Dimension, LengthPercentageAuto, NodeId, Position, Size, Style, TaffyTree};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::floating_surface::{paint_card, painted_bounds};
use ui::widget::{
    Button, ButtonFace, ButtonRole, ControlSize, ModelPath, TaffyTreeExt as _, Widget,
};
use ui::winit::event::{ElementState, MouseButton, WindowEvent};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use vello::peniko::Fill;

use super::{DesktopSurface, LivenessChoice};
use crate::i18n::{t, t_args};

/// A window whose app stopped answering, as the host last told the desk.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Hung {
    /// The tab of the window saying it.
    pub tab: TabId,
    /// The window's name, from its bar.
    pub title: String,
    /// Where the whole window stands in the surface's coordinates: the card sits over its middle.
    pub over: Rect,
}

/// A card as painted: its frame and its two faces, for presses.
#[derive(Clone, Debug)]
struct Painted {
    tab: TabId,
    title: String,
    card: Rect,
    buttons: [(Rect, LivenessChoice); 2],
}

/// The cards over the windows whose app stopped answering.
#[derive(Default)]
pub(crate) struct HungApps {
    rect: Rect,
    /// The windows that say it, as the host last placed them.
    hung: Vec<Hung>,
    /// What the last frame left on screen.
    painted: Vec<Painted>,
    pointer: Option<Point>,
    /// Which face the pointer is over, and which it holds down.
    hovered: Option<(TabId, LivenessChoice)>,
    pressed: Option<(TabId, LivenessChoice)>,
    /// What the person chose, for the host to carry out.
    chosen: Vec<(TabId, LivenessChoice)>,
    /// The windows the person answered since the host last said which windows stopped answering:
    /// their cards stay away until it says again, so a press is answered once and not drawn back
    /// before the host acts on it.
    answered: Vec<TabId>,
    /// The revision `answered` belongs to: the host's revision the cards were laid out for.
    revision: u64,
    /// The host's revision now. A new one lays the cards out again, so the cards a host reports
    /// reach the desk in the frame it reports them in.
    wanted: u64,
    dirty: Cell<bool>,
}

impl HungApps {
    /// The windows that say they stopped responding, as of the desk's last layout, at the host's
    /// revision `revision` .
    pub(super) fn set_hung(&mut self, revision: u64, mut hung: Vec<Hung>) {
        if self.revision != revision {
            self.revision = revision;
            self.answered.clear();
        }
        hung.retain(|card| !self.answered.contains(&card.tab));
        if self.hung != hung {
            self.hung = hung;
            self.pressed = None;
            self.hovered = None;
            self.dirty.set(true);
        }
    }

    /// The host's revision `revision` : a new one means the windows that say they stopped
    /// responding may have changed, and the next layout places their cards.
    pub(super) fn follow(&mut self, revision: u64) {
        self.wanted = revision;
    }

    /// What the person chose on a card, once.
    pub(super) fn take_chosen(&mut self) -> Vec<(TabId, LivenessChoice)> {
        std::mem::take(&mut self.chosen)
    }

    /// Where the card of `tab` stands and what its faces are, as painted.
    #[cfg(test)]
    pub(super) fn card_of(&self, tab: TabId) -> Option<(Rect, Vec<(Rect, LivenessChoice)>)> {
        self.painted
            .iter()
            .find(|painted| painted.tab == tab)
            .map(|painted| (painted.card, painted.buttons.to_vec()))
    }

    /// The face of the card at `point` , if any.
    fn under(&self, point: Point) -> Option<(TabId, LivenessChoice)> {
        self.painted.iter().find_map(|card| {
            card.buttons
                .iter()
                .find(|(rect, _)| rect.contains(point))
                .map(|(_, choice)| (card.tab, *choice))
        })
    }

    /// Whether `point` lands on a card at all: its own presses reach neither the window below nor
    /// the desktop.
    fn on_card(&self, point: Point) -> bool {
        self.painted.iter().any(|card| card.card.contains(point))
    }

    fn choose(&mut self, tab: TabId, choice: LivenessChoice) {
        self.pressed = None;
        self.chosen.push((tab, choice));
        if !self.answered.contains(&tab) {
            self.answered.push(tab);
        }
        // The card goes at once, so the press answers where it was made; a window that is still
        // silent says so again once the host says so.
        self.hung.retain(|hung| hung.tab != tab);
        self.painted.retain(|painted| painted.tab != tab);
        self.dirty.set(true);
    }

    fn heading_style(theme: &Theme) -> ui::text_render::TextStyle {
        theme.text(TextRole::Body).with_weight(TextWeight::Semibold)
    }

    fn title_style(theme: &Theme) -> ui::text_render::TextStyle {
        theme.text(TextRole::Caption)
    }

    /// Lay a card for each window over its middle, kept inside the surface.
    fn place(&self, theme: &Theme, text: &mut TextRenderer) -> Vec<Painted> {
        let spacing = &theme.tokens.spacing;
        let pad = f64::from(spacing.s4);
        let gap = f64::from(spacing.s3);
        let margin = f64::from(spacing.s2);
        let icon = f64::from(theme.tokens.metrics.icon_md);
        let button = ButtonFace::height(ControlSize::Regular, theme);
        let heading = t("hung-app-title");
        let wait = t("hung-app-wait");
        let quit = t("hung-app-force-quit");
        let heading_style = Self::heading_style(theme);
        let title_style = Self::title_style(theme);
        let (heading_w, heading_h) = text.measure_style(&heading, None, &heading_style);
        let (_, title_h) = text.measure_style("", None, &title_style);
        let wait_w = ButtonFace::new(Rect::ZERO, &wait).intrinsic_width(text, theme);
        let quit_w = ButtonFace::new(Rect::ZERO, &quit)
            .role(ButtonRole::Destructive)
            .intrinsic_width(text, theme);
        let faces = wait_w + gap + quit_w;
        let area = self.rect;
        self.hung
            .iter()
            .map(|hung| {
                let (title_w, _) = text.measure_style(&hung.title, None, &title_style);
                let widest = (f64::from(heading_w) + icon + gap)
                    .max(f64::from(title_w))
                    .max(faces);
                let width = (widest + 2.0 * pad).clamp(240.0, 380.0);
                let row = f64::from(heading_h).max(icon);
                let height = pad + row + gap + f64::from(title_h) + gap + button + pad;
                let over = hung.over;
                let x = (over.center().x - width / 2.0).clamp(
                    area.x0 + margin,
                    (area.x1 - margin - width).max(area.x0 + margin),
                );
                let y = (over.center().y - height / 2.0).clamp(
                    area.y0 + margin,
                    (area.y1 - margin - height).max(area.y0 + margin),
                );
                let card = Rect::new(x, y, x + width, y + height);
                // Both ways out stand together at the card's foot, the person's hand on the sadder
                // one last.
                let top = card.y1 - pad - button;
                let quit_rect = Rect::new(card.x1 - pad - quit_w, top, card.x1 - pad, top + button);
                let wait_rect = Rect::new(
                    quit_rect.x0 - gap - wait_w,
                    top,
                    quit_rect.x0 - gap,
                    top + button,
                );
                Painted {
                    tab: hung.tab,
                    title: hung.title.clone(),
                    card,
                    buttons: [
                        (wait_rect, LivenessChoice::Wait),
                        (quit_rect, LivenessChoice::ForceQuit),
                    ],
                }
            })
            .collect()
    }

    /// Paint one card: its ground, the warning mark, what happened, whose window it is, and its two
    /// ways out.
    fn paint(
        &self,
        painted: &Painted,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let spacing = &theme.tokens.spacing;
        let pad = f64::from(spacing.s4);
        let gap = f64::from(spacing.s3);
        let icon = f64::from(theme.tokens.metrics.icon_md);
        let colors = &theme.tokens.colors;
        let card = painted.card;
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &painted_bounds(card, theme),
        );
        paint_card(scene, transform, card, theme);
        let heading_style = Self::heading_style(theme);
        let title_style = Self::title_style(theme);
        let heading = t("hung-app-title");
        let (_, heading_h) = text.measure_style(&heading, None, &heading_style);
        let top = card.y0 + pad;
        let center_y = top + icon / 2.0;
        let mark = Rect::from_center_size(
            Point::new(card.x0 + pad + icon / 2.0, center_y),
            (icon, icon),
        );
        icons::draw_icon(
            scene,
            transform,
            mark,
            Icon::TriangleAlert,
            colors.semantic.warning,
            icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        text.draw_style(
            scene,
            &heading,
            colors.text_primary,
            transform.pre_translate(Vec2::new(
                card.x0 + pad + icon + gap,
                top + (icon - f64::from(heading_h)) / 2.0,
            )),
            None,
            &heading_style,
        );
        let room = card.width() - 2.0 * pad;
        let title = text.elide_to_width_style(
            &painted.title,
            ui::num::f32_of_f64(room + 0.5),
            &title_style,
        );
        text.draw_style(
            scene,
            &title,
            colors.text_dim,
            transform.pre_translate(Vec2::new(card.x0 + pad, top + icon + gap)),
            Some(ui::num::f32_of_f64(room)),
            &title_style,
        );
        for (rect, choice) in &painted.buttons {
            let (label, role) = match choice {
                LivenessChoice::Wait => (t("hung-app-wait"), ButtonRole::Secondary),
                LivenessChoice::ForceQuit => (t("hung-app-force-quit"), ButtonRole::Destructive),
            };
            let here = (painted.tab, *choice);
            Button::paint(
                scene,
                transform,
                text,
                theme,
                &ButtonFace::new(*rect, &label)
                    .role(role)
                    .hovered(self.hovered == Some(here))
                    .pressed(self.pressed == Some(here)),
            );
        }
        scene.pop_layer();
    }
}

impl Widget for HungApps {
    fn name(&self) -> &'static str {
        "HungApps"
    }

    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        _theme: &Theme,
        map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            position: Position::Absolute,
            inset: ui::taffy::Rect {
                left: LengthPercentageAuto::length(0.0),
                top: LengthPercentageAuto::length(0.0),
                right: LengthPercentageAuto::auto(),
                bottom: LengthPercentageAuto::auto(),
            },
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        };
        tree.widget_leaf(style, map, self)
    }

    ui::rect_geometry!();

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO || self.revision != self.wanted
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &ui::presence::PresenceCtx,
    ) {
        self.painted = self.place(theme, text);
        let painted = self.painted.clone();
        for card in &painted {
            self.paint(card, scene, transform, text, theme);
        }
        self.dirty.set(false);
    }

    fn needs_redraw(&self) -> bool {
        self.dirty.get()
    }

    /// A press on a card's face is the person's choice; a press anywhere on the card is the card's,
    /// never the silent window's below.
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = Some(ctx.logical(position));
                let hovered = self.under(self.pointer.unwrap_or(Point::ZERO));
                if hovered.is_some() && hovered != self.hovered {
                    self.hovered = hovered;
                    self.dirty.set(true);
                }
            }
            WindowEvent::CursorLeft { .. } => {
                self.pointer = None;
                if self.hovered.take().is_some() {
                    self.dirty.set(true);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let Some(point) = self.pointer else {
                    return;
                };
                if let Some(face) = self.under(point) {
                    self.pressed = Some(face);
                    self.dirty.set(true);
                }
                if self.on_card(point) {
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                let Some(held) = self.pressed.take() else {
                    return;
                };
                self.dirty.set(true);
                if self
                    .pointer
                    .is_some_and(|point| self.under(point) == Some(held))
                {
                    self.choose(held.0, held.1);
                }
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<ui::winit::window::CursorIcon> {
        self.under(point)
            .map(|_| ui::winit::window::CursorIcon::Pointer)
    }

    /// The cards for assistive technology: what happened to each window,
    /// with its Wait and its Force quit.
    fn accessibility(&self) -> Option<AccessNode> {
        if self.hung.is_empty() {
            return None;
        }
        let wait = t("hung-app-wait");
        let quit = t("hung-app-force-quit");
        let face = |label: &str, verb: &str, tab: TabId| {
            AccessNode::new(Role::Button)
                .with_name(label.to_owned())
                .with_action_target(format!("hung-app:{verb}:{tab}"))
                .with_state(AccessState::new().focusable(true))
        };
        Some(
            AccessNode::new(Role::Group)
                .with_hashed_key("hung-apps")
                .with_name(t("hung-app-title"))
                .with_children(
                    self.hung
                        .iter()
                        .map(|hung| {
                            AccessNode::new(Role::Group)
                                .with_hashed_key(format!("hung-app-{}", hung.tab))
                                .with_name(t_args(
                                    "hung-app-card",
                                    &[("window", hung.title.as_str().into())],
                                ))
                                .with_children(vec![
                                    face(&wait, "wait", hung.tab),
                                    face(&quit, "quit", hung.tab),
                                ])
                        })
                        .collect(),
                ),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        _state: &mut ui::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        if !matches!(action, ui::a11y::Action::Activate) {
            return false;
        }
        let Some(rest) = target.strip_prefix("hung-app:") else {
            return false;
        };
        let Some((verb, tab)) = rest.split_once(':') else {
            return false;
        };
        let Ok(tab) = tab.parse::<TabId>() else {
            return false;
        };
        let choice = match verb {
            "wait" => LivenessChoice::Wait,
            "quit" => LivenessChoice::ForceQuit,
            _ => return false,
        };
        self.choose(tab, choice);
        true
    }
}

impl DesktopSurface {
    /// Lay a card over each window whose app stopped answering, as the host last told the desk.
    pub(super) fn place_hung_apps(&mut self) {
        let revision = self.shared.borrow().unresponsive_revision;
        let hung = {
            let shared = self.shared.borrow();
            if shared.unresponsive.is_empty() {
                Vec::new()
            } else {
                shared
                    .tree
                    .tabs()
                    .filter_map(|tab| {
                        let tilo_desktop_model::TileContent::Foreign { surface_id, .. } = &tab.tile
                        else {
                            return None;
                        };
                        if !shared.unresponsive.contains(surface_id) {
                            return None;
                        }
                        Some(Hung {
                            tab: tab.id,
                            title: tab.title.clone(),
                            over: shared.shown_tabs.window_rect(tab.id)?,
                        })
                    })
                    .collect()
            }
        };
        self.hung_apps.set_hung(revision, hung);
    }

    /// Hand what the person chose on a card to the host that carries it out.
    pub(super) fn drain_liveness_choices(&mut self) {
        let chosen = self.hung_apps.take_chosen();
        if chosen.is_empty() {
            return;
        }
        let mut shared = self.shared.borrow_mut();
        shared.liveness.extend(chosen);
        shared.wake.wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DesktopHandle;
    use tilo_desktop_model::{IdMinter, Tab};
    use ui::widget::TileHost;

    struct Notices;

    impl crate::TileSource for Notices {
        fn tile(&mut self, _: &tilo_desktop_model::Workspace, tab: &Tab) -> TileHost {
            TileHost::new(ui::widget::TileContent::Notice {
                title: tab.title.clone(),
                detail: String::new(),
            })
        }
    }

    /// A desk whose one window is the compositor's window `window` , drawn once, and its handle.
    fn desk(window: u64) -> (ui::headless::FrameHarness, DesktopHandle, TabId) {
        let mut minter = IdMinter::new(11);
        let tab = Tab {
            id: minter.mint(),
            title: "Notes".to_owned(),
            tile: tilo_desktop_model::TileContent::Foreign {
                app_id: "so.tilo.notes".to_owned(),
                surface_id: window,
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        };
        let id = tab.id;
        let tree = super::super::seed_tree(&mut minter, "Desk", vec![tab]);
        let (surface, handle) = DesktopSurface::new(tree, 11, Box::new(Notices));
        let mut frame = ui::headless::FrameHarness::for_root(
            ui::state::State::new(),
            surface,
            (1280, 800),
            Theme::dark(),
        );
        frame.relayout();
        frame.poll_frame().expect("a frame draws the desk");
        (frame, handle, id)
    }

    /// A window whose app stopped answering says so over the window itself,
    /// and the desk offers to wait for its app or to quit it.
    #[test]
    fn a_window_whose_app_stopped_answering_says_so_over_it() {
        let (mut frame, handle, tab) = desk(91);
        assert!(handle.set_unresponsive_windows(vec![91]));
        assert_eq!(handle.unresponsive_tabs(), vec![tab]);
        frame.request_layout();
        frame.poll_frame().expect("a frame draws the card");

        let surface = frame.root::<DesktopSurface>();
        let window = handle.window_rect(tab).expect("the window stands");
        let (card, buttons) = surface
            .hung_apps
            .card_of(tab)
            .unwrap_or_else(|| panic!("the window says it stopped answering"));
        assert!(
            window.contains(card.center()),
            "the card {card:?} stands over its window {window:?}"
        );
        assert_eq!(buttons.len(), 2, "Wait and Force quit");
        let viewport = surface.get_rect();
        assert!(
            card.x0 >= viewport.x0
                && card.x1 <= viewport.x1
                && card.y0 >= viewport.y0
                && card.y1 <= viewport.y1,
            "the card {card:?} stays inside the desk {viewport:?}"
        );
        assert!(
            surface.hung_apps.accessibility().is_some(),
            "and says so to assistive technology"
        );

        // The app answers again: nothing is left to say.
        assert!(handle.set_unresponsive_windows(Vec::new()));
        frame.request_layout();
        frame.poll_frame().expect("a frame draws the desk");
        assert!(
            frame
                .root::<DesktopSurface>()
                .hung_apps
                .card_of(tab)
                .is_none(),
            "an answering window shows no card"
        );
    }

    /// Wait is the person's choice on the card, given to the host that owns the compositor; the
    /// card goes at once, so the press answers.
    #[test]
    fn waiting_on_the_card_is_the_persons_choice() {
        let (mut frame, handle, tab) = desk(91);
        assert!(handle.set_unresponsive_windows(vec![91]));
        frame.request_layout();
        frame.poll_frame().expect("a frame draws the card");
        let wait = frame
            .root::<DesktopSurface>()
            .hung_apps
            .card_of(tab)
            .map(|(_, buttons)| buttons[0])
            .expect("a card");
        assert_eq!(wait.1, LivenessChoice::Wait);
        frame.click_at(wait.0.center().x, wait.0.center().y);
        assert_eq!(
            handle.take_liveness_choices(),
            vec![(tab, LivenessChoice::Wait)]
        );
        assert!(
            frame
                .root::<DesktopSurface>()
                .hung_apps
                .card_of(tab)
                .is_none(),
            "the card goes as the choice is made"
        );
        assert!(handle.take_liveness_choices().is_empty(), "and once only");
    }

    /// Force quit is the other choice, and it names the same window.
    #[test]
    fn force_quitting_on_the_card_names_the_window() {
        let (mut frame, handle, tab) = desk(91);
        assert!(handle.set_unresponsive_windows(vec![91]));
        frame.request_layout();
        frame.poll_frame().expect("a frame draws the card");
        let quit = frame
            .root::<DesktopSurface>()
            .hung_apps
            .card_of(tab)
            .map(|(_, buttons)| buttons[1])
            .expect("a card");
        assert_eq!(quit.1, LivenessChoice::ForceQuit);
        frame.click_at(quit.0.center().x, quit.0.center().y);
        assert_eq!(
            handle.take_liveness_choices(),
            vec![(tab, LivenessChoice::ForceQuit)]
        );
    }
}
