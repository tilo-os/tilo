//! The ask about the person themselves, put to them from their own avatar
//! ([`crate::chrome::AvatarAsk`]).
//!
//! Open, it is a small popover in the bar's own band, hanging left from the
//! avatar and sized to its words: the question, what answering brings where
//! there is room, "Not now" and the answer. It stays inside the bar, so it
//! never covers a window's bar or content and never takes a click meant for
//! a window: a press anywhere else folds it into a dot on the avatar and
//! still reaches what it was pressed on. Escape folds it too. A click on
//! the avatar while it waits as the dot opens it again.
use super::{
    Affine, BAR_CONTROL_PX, ChromeIntent, Point, Rect, RoundedRect, Scene, TextRenderer, TextRole,
    Theme, TopBarChrome, at, t,
};
use crate::chrome::AvatarAsk;
use ui::text_render::TextWeight;
use vello::peniko::Fill;
use winit::event::WindowEvent;

/// Below this the question elides to a word or two and stops asking
/// anything: the ask then waits as the avatar's dot.
const MIN_QUESTION_PX: f64 = 96.0;

/// Widest the detail line grows before it elides.
const MAX_DETAIL_PX: f64 = 320.0;

/// Where the open ask's parts were painted, for its hit tests.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct AskRects {
    pub pill: Rect,
    pub answer: Rect,
    pub later: Rect,
}

impl TopBarChrome {
    pub(super) const CONTROL_ASK_ANSWER: &'static str = "topbar:ask-answer";
    pub(super) const CONTROL_ASK_LATER: &'static str = "topbar:ask-later";

    /// The ask waiting on the person's avatar, as the host has it now.
    pub(super) fn chrome_avatar_ask(&self) -> Option<AvatarAsk> {
        self.chrome.as_ref()?.avatar_ask()
    }

    /// The bounds of the open ask as last painted; zero while it waits as
    /// the avatar's dot, or there is none.
    #[must_use]
    pub fn avatar_ask_rect(&self) -> Rect {
        self.ask.pill
    }

    /// Where the open ask's answer was painted; zero when it is not open.
    #[must_use]
    pub fn avatar_ask_answer_rect(&self) -> Rect {
        self.ask.answer
    }

    /// The open ask, hanging left from the avatar's left edge at `right_x`
    /// inside the bar's band; returns where the controls before it end.
    pub(super) fn draw_avatar_ask(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        right_x: f64,
    ) -> f64 {
        use ui::widget::{ChipFace, ChipStyle};

        self.ask = AskRects::default();
        let Some(ask) = self.chrome_avatar_ask().filter(|ask| ask.open) else {
            return right_x;
        };
        if self.own_avatar_rect.is_zero_area() {
            return right_x;
        }
        let left_limit = self.center_lane.x1 + f64::from(theme.tokens.spacing.s4);
        let Some(words) = AskWords::fit(&ask, right_x - left_limit, theme, text_renderer) else {
            return right_x;
        };
        let pad = f64::from(theme.tokens.spacing.s2);
        let gap = f64::from(theme.tokens.spacing.s1);
        // The bar's own band: never below it, where the windows' bars are.
        let top = (center_y - BAR_CONTROL_PX / 2.0).max(self.rect.y0);
        let bottom = (center_y + BAR_CONTROL_PX / 2.0).min(self.rect.y1);
        let pill = Rect::new(right_x - words.width(pad, gap), top, right_x, bottom);
        let colors = &theme.tokens.colors;
        let shape = RoundedRect::from_rect(pill, pill.height() / 2.0);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.overlay,
            None,
            &shape,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &shape,
        );
        words.paint(
            scene,
            transform,
            theme,
            text_renderer,
            (pill.x0 + pad, center_y),
        );
        let chip_h = ChipFace::height(theme).min(pill.height() - 2.0);
        let chip_top = center_y - chip_h / 2.0;
        let answer = Rect::new(
            pill.x1 - pad / 2.0 - words.answer_w,
            chip_top,
            pill.x1 - pad / 2.0,
            chip_top + chip_h,
        );
        let later = Rect::new(
            answer.x0 - gap - words.later_w,
            chip_top,
            answer.x0 - gap,
            chip_top + chip_h,
        );
        let hovered = self.hovered_control();
        ChipFace::new(later, &words.later)
            .hovered(hovered.as_deref() == Some(Self::CONTROL_ASK_LATER))
            .paint(scene, transform, text_renderer, theme);
        ChipFace::new(answer, &ask.answer)
            .style(ChipStyle::Live)
            .hovered(hovered.as_deref() == Some(Self::CONTROL_ASK_ANSWER))
            .paint(scene, transform, text_renderer, theme);
        self.ask = AskRects {
            pill,
            answer,
            later,
        };
        pill.x0 - f64::from(theme.tokens.spacing.s3)
    }

    /// The dot on the avatar while an ask waits there folded.
    pub(super) fn draw_avatar_ask_dot(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        if self.own_avatar_rect.is_zero_area()
            || self.chrome_avatar_ask().is_none_or(|ask| ask.open)
        {
            return;
        }
        let avatar = self.own_avatar_rect;
        let radius = 3.5;
        let centre = Point::new(avatar.x1 - radius, avatar.y0 + radius);
        let ring = vello::kurbo::Circle::new(centre, radius + 1.5);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &ring,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.intent.you,
            None,
            &vello::kurbo::Circle::new(centre, radius),
        );
    }

    /// The open ask's share of an event: its answer and "Not now", Escape,
    /// and a press anywhere else folding it. Returns whether the event is
    /// spent; a press elsewhere folds the ask and goes on to whatever it
    /// was pressed on.
    pub(super) fn handle_avatar_ask_event(&self, event: &WindowEvent, handled: &mut bool) -> bool {
        let open = self.chrome_avatar_ask().is_some_and(|ask| ask.open);
        if !open {
            return false;
        }
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        match event {
            WindowEvent::KeyboardInput { event: key, .. }
                if key.state == winit::event::ElementState::Pressed
                    && key.logical_key
                        == winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape) =>
            {
                *handled = chrome.intent(ChromeIntent::CollapseAvatarAsk);
                *handled
            }
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                let Some(p) = self.last_cursor else {
                    return false;
                };
                if self.ask.answer.contains(p) {
                    *handled = chrome.intent(ChromeIntent::AnswerAvatarAsk);
                    true
                } else if self.ask.later.contains(p) {
                    *handled = chrome.intent(ChromeIntent::PutOffAvatarAsk);
                    true
                } else if self.ask.pill.contains(p) {
                    // Its own surface: nothing under it to reach.
                    *handled = true;
                    true
                } else {
                    // The avatar opens the profile card past the ask; a
                    // press anywhere else is the person moving on.
                    if !self.own_avatar_rect.contains(p) {
                        let _ = chrome.intent(ChromeIntent::CollapseAvatarAsk);
                    }
                    false
                }
            }
            _ => false,
        }
    }

    /// What a click on the avatar asks for: the ask waiting as its dot,
    /// else the profile card.
    pub(super) fn avatar_intent(&self) -> ChromeIntent {
        if self.chrome_avatar_ask().is_some_and(|ask| !ask.open) {
            ChromeIntent::OpenAvatarAsk
        } else {
            ChromeIntent::ToggleProfile
        }
    }

    /// Activating the open ask's controls from the keyboard or an agent.
    pub(super) fn activate_ask_control(&self, target: &str) -> Option<bool> {
        let intent = match target {
            Self::CONTROL_ASK_ANSWER => ChromeIntent::AnswerAvatarAsk,
            Self::CONTROL_ASK_LATER => ChromeIntent::PutOffAvatarAsk,
            _ => return None,
        };
        Some(
            self.chrome
                .as_ref()
                .is_some_and(|chrome| chrome.intent(intent)),
        )
    }
}

/// The open ask's words, fitted to the room the bar leaves it.
struct AskWords {
    question: String,
    question_w: f64,
    /// What answering brings, where there is room beside the question.
    detail: Option<String>,
    detail_w: f64,
    later: String,
    later_w: f64,
    answer_w: f64,
}

impl AskWords {
    /// Fit `ask` into `room` px of bar, or `None` where its question would
    /// not read.
    fn fit(
        ask: &AvatarAsk,
        room: f64,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) -> Option<Self> {
        use ui::widget::{ChipFace, ChipLead};

        let tokens = &theme.tokens;
        let pad = f64::from(tokens.spacing.s2);
        let gap = f64::from(tokens.spacing.s1);
        let later = t("notification-not-now");
        let later_w =
            ChipFace::intrinsic_width(&later, ChipLead::None, false, text_renderer, theme);
        let answer_w =
            ChipFace::intrinsic_width(&ask.answer, ChipLead::None, false, text_renderer, theme);
        let (question_style, detail_style) = Self::styles(theme);
        let room = room - 2.0 * pad - later_w - gap - answer_w - gap;
        if room < MIN_QUESTION_PX {
            return None;
        }
        let (question_w, _) = text_renderer.measure_style(&ask.question, None, &question_style);
        let question_w = f64::from(question_w).min(room);
        let question = text_renderer.elide_to_width_style(
            &ask.question,
            ui::num::f32_of_f64(question_w),
            &question_style,
        );
        let detail_room = (room - question_w - pad).min(MAX_DETAIL_PX);
        let detail = (detail_room >= MIN_QUESTION_PX && !ask.detail.is_empty()).then(|| {
            text_renderer.elide_to_width_style(
                &ask.detail,
                ui::num::f32_of_f64(detail_room),
                &detail_style,
            )
        });
        let detail_w = detail.as_ref().map_or(0.0, |detail| {
            f64::from(text_renderer.measure_style(detail, None, &detail_style).0) + pad
        });
        Some(Self {
            question,
            question_w,
            detail,
            detail_w,
            later,
            later_w,
            answer_w,
        })
    }

    fn styles(theme: &Theme) -> (ui::text_render::TextStyle, ui::text_render::TextStyle) {
        let typography = &theme.tokens.typography;
        (
            typography
                .style(TextRole::Caption)
                .with_weight(TextWeight::Semibold),
            typography.style(TextRole::Caption),
        )
    }

    /// The whole pill's width with `pad` at its ends and `gap` between.
    fn width(&self, pad: f64, gap: f64) -> f64 {
        pad + self.question_w + self.detail_w + gap + self.later_w + gap + self.answer_w + pad
    }

    /// The question, then the detail, from `x` on the line through `y`.
    fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        (x, y): (f64, f64),
    ) {
        let colors = &theme.tokens.colors;
        let (question_style, detail_style) = Self::styles(theme);
        let lines = [
            Some((&self.question, colors.text_primary, &question_style, 0.0)),
            self.detail.as_ref().map(|detail| {
                let after = self.question_w + f64::from(theme.tokens.spacing.s2);
                (detail, colors.text_dim, &detail_style, after)
            }),
        ];
        for (text, color, style, dx) in lines.into_iter().flatten() {
            let (_, h) = text_renderer.measure_style(text, None, style);
            text_renderer.draw_style(
                scene,
                text,
                color,
                at(transform, (x + dx, y - f64::from(h) / 2.0)),
                None,
                style,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::{DesktopChromeSnapshot, DesktopChromeSource, OwnChip};
    use crate::composition::{DesktopSurface, SurfaceChrome};
    use crate::intents::{DesktopIntent, LocalViewIntent};
    use crate::resolver::{TileRequest, TileResolution, TileResolver};
    use std::cell::RefCell;
    use std::rc::Rc;
    use tilo_desktop_model::{
        AppKind, DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth,
        Workspace, WorkspaceId,
    };
    use ui::headless::Framed;
    use ui::record::RecordStore;

    struct Unavailable;
    impl TileResolver for Unavailable {
        fn resolve(&mut self, request: TileRequest<'_>) -> TileResolution {
            TileResolution::Unavailable {
                title: request.tab.title.clone(),
                detail: String::new(),
            }
        }
    }

    /// A host with an ask waiting on the person's avatar, answering the
    /// bar's intents about it as a page does.
    struct Asking {
        tree: DesktopTree,
        ask: RefCell<Option<AvatarAsk>>,
        intents: RefCell<Vec<ChromeIntent>>,
    }

    impl DesktopChromeSource for Asking {
        fn snapshot(&self) -> DesktopChromeSnapshot {
            DesktopChromeSnapshot {
                own: Some(OwnChip {
                    name: "Kim Lee".into(),
                    rgb: (0xe0, 0x7a, 0x5f),
                    identity: String::new(),
                }),
                ..DesktopChromeSnapshot::default()
            }
        }
        fn tree(&self) -> DesktopTree {
            self.tree.clone()
        }
        fn intent(&self, intent: ChromeIntent) -> bool {
            {
                let mut ask = self.ask.borrow_mut();
                match intent {
                    ChromeIntent::OpenAvatarAsk | ChromeIntent::CollapseAvatarAsk => {
                        if let Some(ask) = ask.as_mut() {
                            ask.open = intent == ChromeIntent::OpenAvatarAsk;
                        }
                    }
                    ChromeIntent::AnswerAvatarAsk | ChromeIntent::PutOffAvatarAsk => *ask = None,
                    _ => {}
                }
            }
            self.intents.borrow_mut().push(intent);
            true
        }
        fn avatar_ask(&self) -> Option<AvatarAsk> {
            self.ask.borrow().clone()
        }
    }

    /// A strip whose one window runs from the leading edge to well under
    /// the ask, so the window's bar and its controls stand right below it.
    fn tree() -> DesktopTree {
        let mut workspace = Workspace::new(WorkspaceId(1), "Lounge");
        workspace.strips = vec![Strip {
            id: StripId(2),
            name: "Main".into(),
            window_groups: vec![WindowGroup {
                id: 3,
                tabs: vec![Tab {
                    id: 30,
                    title: "Calculator".into(),
                    tile: TileContent::AppPlaceholder {
                        kind: AppKind::Other("so.tilo.calculator".into()),
                        label: "Calculator".into(),
                    },
                    app_title: None,
                }],
                active_tab: 30,
                width: WindowWidth::Fraction(800),
                maximized: false,
                focus_history: Vec::new(),
            }],
        }];
        DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        }
    }

    fn asking_desktop(viewport: (u32, u32)) -> (Framed<DesktopSurface>, Rc<Asking>) {
        ui::i18n::I18nManager::init("en-US");
        let tree = tree();
        let host = Rc::new(Asking {
            tree: tree.clone(),
            ask: RefCell::new(Some(AvatarAsk {
                question: "Keep your name and work?".into(),
                detail: "A passkey brings you back as yourself on this device.".into(),
                answer: "Keep with passkey".into(),
                open: true,
            })),
            intents: RefCell::default(),
        });
        let chrome: crate::chrome::Chrome = host.clone();
        let bar = TopBarChrome::from_tree(&tree).with_chrome(chrome);
        let surface = DesktopSurface::build(
            &tree,
            &RecordStore::in_memory(),
            &mut Unavailable,
            SurfaceChrome {
                topbar: Box::new(bar),
                ..SurfaceChrome::default()
            },
        );
        let mut frame = Framed::new(ui::State::new(), surface, viewport, Theme::dark());
        settle(&mut frame);
        (frame, host)
    }

    fn settle(frame: &mut Framed<DesktopSurface>) {
        for _ in 0..3 {
            let _ = Framed::harness_mut(frame).poll_frame();
        }
    }

    fn ask_rect(frame: &Framed<DesktopSurface>) -> Rect {
        frame
            .topbar()
            .map_or(Rect::ZERO, TopBarChrome::avatar_ask_rect)
    }

    fn windows(frame: &Framed<DesktopSurface>) -> Vec<Rect> {
        frame
            .row(StripId(2))
            .map(|row| {
                row.groups
                    .iter()
                    .map(ui::widget::Widget::get_rect)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Open, the ask stands in the bar's band beside the avatar: no window,
    /// its bar and controls included, is under any part of it.
    #[test]
    fn the_open_ask_never_covers_a_window() {
        for viewport in [(1440, 900), (1280, 720), (1920, 1080)] {
            let (frame, _) = asking_desktop(viewport);
            let ask = ask_rect(&frame);
            assert!(!ask.is_zero_area(), "{viewport:?}: the ask is open");
            let bar = frame
                .topbar()
                .map_or(Rect::ZERO, ui::widget::Widget::get_rect);
            assert!(
                ask.y0 >= bar.y0 && ask.y1 <= bar.y1,
                "{viewport:?}: the ask {ask:?} stays in the bar {bar:?}"
            );
            let windows = windows(&frame);
            assert!(!windows.is_empty(), "{viewport:?}: a window is laid out");
            for window in windows {
                assert!(
                    window.intersect(ask).is_zero_area(),
                    "{viewport:?}: the ask {ask:?} covers the window {window:?}"
                );
            }
        }
    }

    /// A click on the window header right below the open ask reaches the
    /// window (here its maximize control) and folds the ask into the
    /// avatar's dot; a click on the avatar then opens it again.
    #[test]
    fn a_click_on_the_window_header_below_the_ask_reaches_the_window() {
        let (mut frame, host) = asking_desktop((1440, 900));
        let ask = ask_rect(&frame);
        let maximize = frame
            .row(StripId(2))
            .and_then(|row| row.groups.first())
            .and_then(|group| group.presentation_bounds().maximize)
            .unwrap_or_else(|| panic!("the window shows its maximize control"));
        let at = maximize.center();
        assert!(
            ask.x0 <= at.x && at.x <= ask.x1 && at.y > ask.y1,
            "the window's maximize {maximize:?} stands below the ask {ask:?}"
        );
        let _ = frame.take_intents();
        Framed::harness_mut(&mut frame).click_at(at.x, at.y);
        settle(&mut frame);
        assert!(
            frame
                .take_intents()
                .contains(&DesktopIntent::Local(LocalViewIntent::Maximized {
                    group: 3,
                    maximized: true,
                })),
            "the window took the click"
        );
        assert!(
            host.ask.borrow().as_ref().is_some_and(|ask| !ask.open),
            "the click folded the ask into the avatar's dot"
        );

        let avatar = frame
            .topbar()
            .map_or(Rect::ZERO, TopBarChrome::own_avatar_rect);
        assert!(!avatar.is_zero_area(), "the own avatar is painted");
        Framed::harness_mut(&mut frame).click_at(avatar.center().x, avatar.center().y);
        assert!(
            host.ask.borrow().as_ref().is_some_and(|ask| ask.open),
            "the avatar's dot opens the ask again"
        );
    }

    /// Escape folds the open ask; its answer reaches the host.
    #[test]
    fn escape_folds_the_ask_and_its_answer_reaches_the_host() {
        let (mut frame, host) = asking_desktop((1440, 900));
        Framed::harness_mut(&mut frame).key("Escape");
        assert!(host.ask.borrow().as_ref().is_some_and(|ask| !ask.open));

        let _ = host.intent(ChromeIntent::OpenAvatarAsk);
        settle(&mut frame);
        let answer = frame
            .topbar()
            .map_or(Rect::ZERO, TopBarChrome::avatar_ask_answer_rect);
        assert!(!answer.is_zero_area(), "the ask is open again");
        Framed::harness_mut(&mut frame).click_at(answer.center().x, answer.center().y);
        assert!(
            host.intents
                .borrow()
                .contains(&ChromeIntent::AnswerAvatarAsk)
        );
        assert!(host.ask.borrow().is_none(), "answered, the ask goes");
    }
}
