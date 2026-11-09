//! The ask about the person themselves, put to them from their own avatar
//! ([`crate::chrome::AvatarAsk`]).
//!
//! Open, it is a small popover in the bar's own band, hanging left from the
//! avatar and sized to its words: the question, what answering brings where
//! there is room, "Not now" and the answer. Where it does not fit beside the
//! clock's lane it takes that lane too, over the glance value it covers.
//! Where the band cannot hold its words at all (a narrow bar, a compact one),
//! it opens as a floating card under the band, hanging from the avatar, with
//! the question on its first line and its chips on the next: the same words,
//! the same controls, so the question is always shown where it can be
//! answered. In the band it never covers a window's bar or content; the card
//! under it stands above the windows, and takes the clicks in it, so the ask
//! is never a surface the person cannot reach. A press anywhere else folds it
//! into a dot on the avatar and still reaches what it was pressed on. Escape
//! folds it too. A click on the avatar while it waits as the dot opens it
//! again.
use super::{
    Affine, BAR_CONTROL_PX, ChromeIntent, Point, Rect, Scene, TextRenderer, TextRole, Theme,
    TopBarChrome, t,
};
use crate::chrome::AvatarAsk;
use ui::text_render::TextWeight;
use vello::peniko::Fill;
use winit::event::WindowEvent;

/// Below this the question elides to a word or two and stops asking anything: the ask then waits as
/// the avatar's dot.
const MIN_QUESTION_PX: f64 = 96.0;

/// Widest the detail line grows before it elides.
const MAX_DETAIL_PX: f64 = 320.0;

/// Where the open ask's parts were painted, for its hit tests: the surface it stands on (the pill
/// in the band, or the card under it), and its chips.
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

    /// The bounds of the open ask as last painted; zero while it waits as the avatar's dot, or
    /// there is none.
    #[must_use]
    pub fn avatar_ask_rect(&self) -> Rect {
        self.ask.pill
    }

    /// Where the open ask's answer was painted; zero when it is not open.
    #[must_use]
    pub fn avatar_ask_answer_rect(&self) -> Rect {
        self.ask.answer
    }

    /// The open ask, hanging left from the avatar's left edge at `right_x` inside the bar's band;
    /// returns where the controls before it end.
    pub(super) fn draw_avatar_ask(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        right_x: f64,
    ) {
        use ui::widget::{ChipFace, ChipStyle};

        self.ask = AskRects::default();
        let Some(ask) = self.chrome_avatar_ask().filter(|ask| ask.open) else {
            return;
        };
        // It hangs from the avatar the bar draws for the person's own chip,
        // before that avatar is painted: the chip, not last frame's rect.
        if self.own_chip().is_none() {
            return;
        }
        // Beside the clock where its words fit there; a question the bar cannot hold beside the
        // lane takes the lane's free room up to the clock, and, with no room there either, the
        // clock too, rather than waiting where it cannot be read or answered.
        let gap = f64::from(theme.tokens.spacing.s4);
        let limits = [
            self.center_lane.x1 + gap,
            self.center_rect.x1 + gap,
            self.center_lane.x0 + gap,
        ];
        let mut words = None;
        for limit in limits {
            words = AskWords::fit(&ask, right_x - limit, theme, text_renderer);
            if words.is_some() {
                break;
            }
        }
        let Some(words) = words else {
            // A bar too narrow for the words beside the avatar: the ask opens under the band, where
            // a card holds them.
            self.draw_avatar_ask_below(scene, transform, theme, text_renderer, right_x);
            return;
        };
        let pad = f64::from(theme.tokens.spacing.s2);
        let gap = f64::from(theme.tokens.spacing.s1);
        // The bar's own band: never below it, where the windows' bars are.
        let top = (center_y - BAR_CONTROL_PX / 2.0).max(self.rect.y0);
        let bottom = (center_y + BAR_CONTROL_PX / 2.0).min(self.rect.y1);
        let pill = Rect::new(right_x - words.width(pad, gap), top, right_x, bottom);
        // The pill is a floating panel's body: the overlay ground and its inner hairline, from the
        // shared painter, on the bar.
        ui::widget::FloatingPanelFace::new()
            .body_empty(true)
            .paint_body(scene, transform, pill, theme);
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
    }

    /// The open ask where the bar's band cannot hold its words: the same ask as a floating card
    /// under the band, hanging from the avatar, the question and what answering brings on its first
    /// line and its chips on the next. It stands above the windows, so the question is always read
    /// and answered where it is shown.
    pub(super) fn draw_avatar_ask_below(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        right_x: f64,
    ) {
        use ui::widget::floating_surface::{self, PanelAlign, PopoverSide};
        use ui::widget::{ChipFace, ChipStyle};

        let Some(ask) = self.chrome_avatar_ask().filter(|ask| ask.open) else {
            return;
        };
        let tokens = &theme.tokens;
        // Every floating card keeps the layer's standard margin from the screen's edges: the same
        // room other anchored cards leave.
        let margin = floating_surface::viewport_margin(theme);
        let widest = (self.rect.width() - 2.0 * margin).max(0.0);
        let pad = f64::from(tokens.spacing.s3);
        let Some(words) = AskWords::fit_below(&ask, widest, theme, text_renderer) else {
            return;
        };
        let gap = f64::from(tokens.spacing.s2);
        let chip_h = ChipFace::height(theme);
        let line_h = words.line_height(text_renderer, theme);
        let question_line = words.question_w + words.detail_w;
        let chips_w = words.later_w + gap + words.answer_w;
        let card_h = pad + line_h + gap + chip_h + pad;
        // Under the band, hanging from the avatar. The bar knows its own band, not the window under
        // it: the card stands in the room the band leaves below it, which is where the desk's
        // windows begin.
        let below = f64::from(tokens.spacing.s2);
        let window = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y1 + below + card_h + margin,
        );
        let room = floating_surface::panel_room(window, theme);
        let card_w = (question_line.max(chips_w) + 2.0 * pad)
            .min(widest)
            .min(room.width());
        let anchor = Rect::new(
            right_x,
            self.rect.y1,
            right_x + BAR_CONTROL_PX,
            self.rect.y1,
        );
        let (card, _) = floating_surface::place_aligned(
            anchor,
            (card_w, card_h),
            PopoverSide::Below,
            PanelAlign::Trailing,
            below,
            room,
        );
        floating_surface::paint_card(scene, transform, card, theme);
        words.paint(
            scene,
            transform,
            theme,
            text_renderer,
            (card.x0 + pad, card.y0 + pad + line_h / 2.0),
        );
        let chip_top = card.y1 - pad - chip_h;
        let answer = Rect::new(
            card.x1 - pad - words.answer_w,
            chip_top,
            card.x1 - pad,
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
            pill: card,
            answer,
            later,
        };
    }

    /// The dot on the avatar while an ask waits there folded: put off by the person, or one the bar
    /// had no room to open.
    pub(super) fn draw_avatar_ask_dot(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        if self.own_avatar_rect.is_zero_area()
            || self.chrome_avatar_ask().is_none_or(|ask| {
                // The pill drawn beside the avatar this frame speaks for it.
                ask.open && !self.ask.pill.is_zero_area()
            })
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
            // Escape folds the ask only once the person has moved to it: the bar holds their
            // focus. A card that arrived unasked stands beside the avatar while they work and
            // never answers the keys they are typing where they are — their window's Escape,
            // their search's Escape — so it shows without taking the keyboard from them.
            WindowEvent::KeyboardInput { event: key, .. }
                if self.focused
                    && key.state == winit::event::ElementState::Pressed
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
                    // The avatar opens the profile card past the ask; a press anywhere else is the
                    // person moving on.
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
    /// Fit `ask` into `room` px of bar, or `None` where its question would not read: the chips
    /// stand beside the question on the pill's one line.
    fn fit(
        ask: &AvatarAsk,
        room: f64,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) -> Option<Self> {
        Self::fit_in(ask, room, true, theme, text_renderer)
    }

    /// Fit `ask` into the `room` px a card under the bar leaves for its question line, its chips
    /// standing on a row of their own; `None` where its question would not read.
    fn fit_below(
        ask: &AvatarAsk,
        room: f64,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) -> Option<Self> {
        Self::fit_in(ask, room, false, theme, text_renderer)
    }

    fn fit_in(
        ask: &AvatarAsk,
        room: f64,
        chips_beside: bool,
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
        let beside = if chips_beside {
            later_w + gap + answer_w + gap
        } else {
            0.0
        };
        let room = room - 2.0 * pad - beside;
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

    /// How tall the question's line stands: the two facts beside each other.
    fn line_height(&self, text_renderer: &mut TextRenderer, theme: &Theme) -> f64 {
        use ui::widget::primitives::FactsFace;
        let facts = self.facts();
        FactsFace::new(&facts).height(text_renderer, theme)
    }

    /// The question and, where there is room, what answering it brings: two facts laid out with
    /// space between, never joined into one string.
    fn facts(&self) -> Vec<ui::widget::Fact> {
        let mut facts = vec![ui::widget::Fact::text(&self.question).priority(1)];
        if let Some(detail) = &self.detail {
            facts.push(ui::widget::Fact::text(detail));
        }
        facts
    }

    /// The question, then the detail beside it, from `x` on the line through `y` : two facts laid
    /// out with space between, never joined into one string, so the detail gives way first when the
    /// bar is narrow.
    fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        (x, y): (f64, f64),
    ) {
        use ui::widget::primitives::FactsFace;
        // Two separate facts with space between, never one joined string: the question stays and
        // the detail gives way first when the bar is narrow, as the ask's own fit already elides
        // them.
        let facts = self.facts();
        let height = FactsFace::new(&facts).height(text_renderer, theme);
        FactsFace::new(&facts).paint(
            &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
            Rect::new(
                x,
                y - height / 2.0,
                x + self.question_w + self.detail_w,
                y + height / 2.0,
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::about::{AboutLinks, AboutVariant, TiloMenu};
    use crate::chrome::{DesktopChromeSnapshot, DesktopChromeSource, OwnChip};
    use crate::desktop_host::{DesktopChrome, DesktopRules};
    use std::cell::RefCell;
    use std::rc::Rc;
    use tilo_desktop_model::commands::DesktopCommand;
    use tilo_desktop_model::{
        AppKind, DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth,
        Workspace, WorkspaceSlot,
    };
    use tilo_desktop_surface::{DesktopHandle, DesktopSurface, TileSource};
    use ui::headless::Framed;
    use ui::widget::TileHost;

    /// The desk's windows, with their content as plain notices.
    struct Notices;

    impl TileSource for Notices {
        fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
            TileHost::new(ui::widget::TileContent::Notice {
                title: tab.title.clone(),
                detail: String::new(),
            })
        }
    }

    /// The desk the chrome stands over, answering what the chrome asks of it as the session does.
    struct Rules {
        handle: DesktopHandle,
    }

    impl DesktopRules for Rules {
        fn tree(&self) -> DesktopTree {
            self.handle.read(Clone::clone)
        }

        fn author(&self) -> String {
            "Kim Lee".into()
        }

        fn may(&self, _: &DesktopCommand) -> Result<(), String> {
            Ok(())
        }

        fn may_create_workspace(&self) -> Result<(), String> {
            Ok(())
        }
    }

    /// A host with an ask waiting on the person's avatar, answering the bar's intents about it as a
    /// page does.
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

    fn tree() -> DesktopTree {
        let mut workspace = Workspace::new(WorkspaceSlot(1), "Lounge");
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
                    view: String::new(),
                    pinned: false,
                }],
                active_tab: 30,
                width: WindowWidth::Fraction(800),
                maximized: false,
                focus_history: Vec::new(),
            }],
        }];
        DesktopTree {
            active: workspace.slot,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        }
    }

    /// The real chrome over a desk with one window, at `viewport` .
    fn asking_desktop(viewport: (u32, u32)) -> (Framed<DesktopChrome>, Rc<Asking>) {
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
        let (_desk, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(Notices));
        let chrome: crate::chrome::Chrome = host.clone();
        let bar = TopBarChrome::from_tree(&tree).with_chrome(chrome);
        let chrome = DesktopChrome::new(
            handle.clone(),
            "g".into(),
            Rc::new(Rules { handle }),
            bar,
            TiloMenu::new(AboutVariant::Native, &AboutLinks::default()),
            false,
        );
        let mut frame = Framed::new(ui::State::new(), chrome, viewport, Theme::dark());
        settle(&mut frame);
        (frame, host)
    }

    fn settle(frame: &mut Framed<DesktopChrome>) {
        for _ in 0..3 {
            let _ = Framed::harness_mut(frame).poll_frame();
        }
    }

    fn ask_rect(frame: &Framed<DesktopChrome>) -> Rect {
        frame.bar().avatar_ask_rect()
    }

    fn bar_rect(frame: &Framed<DesktopChrome>) -> Rect {
        ui::widget::Widget::get_rect(frame.bar())
    }

    /// Open beside the avatar in a bar with room, the ask stands in the bar's band, its words and
    /// its controls inside it.
    #[test]
    fn the_open_ask_stays_in_the_bar_band() {
        for viewport in [(1440, 900), (1280, 720), (1920, 1080)] {
            let (frame, _) = asking_desktop(viewport);
            let ask = ask_rect(&frame);
            assert!(!ask.is_zero_area(), "{viewport:?}: the ask is open");
            let bar = bar_rect(&frame);
            assert!(
                ask.y0 >= bar.y0 && ask.y1 <= bar.y1,
                "{viewport:?}: the ask {ask:?} stays in the bar {bar:?}"
            );
            assert!(
                ask.x0 >= bar.x0 && ask.x1 <= bar.x1,
                "{viewport:?}: the ask {ask:?} stays on the bar's width {bar:?}"
            );
            let answer = frame.bar().avatar_ask_answer_rect();
            assert!(
                ask.x0 <= answer.x0 && answer.x1 <= ask.x1,
                "{viewport:?}: the answer {answer:?} stands inside the ask {ask:?}"
            );
        }
    }

    /// Escape folds the open ask into the avatar's dot; opened again, its answer reaches the host.
    #[test]
    fn escape_folds_the_ask_and_its_answer_reaches_the_host() {
        let (mut frame, host) = asking_desktop((1440, 900));
        Framed::harness_mut(&mut frame).key("Escape");
        assert!(host.ask.borrow().as_ref().is_some_and(|ask| !ask.open));
        settle(&mut frame);
        assert!(
            ask_rect(&frame).is_zero_area(),
            "folded, only the dot is painted"
        );

        let _ = host.intent(ChromeIntent::OpenAvatarAsk);
        settle(&mut frame);
        let answer = frame.bar().avatar_ask_answer_rect();
        assert!(!answer.is_zero_area(), "the ask is open again");
        Framed::harness_mut(&mut frame).click_at(answer.center().x, answer.center().y);
        assert!(
            host.intents
                .borrow()
                .contains(&ChromeIntent::AnswerAvatarAsk)
        );
        assert!(host.ask.borrow().is_none(), "answered, the ask goes");
    }

    /// A bar too narrow for the ask's words beside the avatar opens it as a card under the band,
    /// hanging from the avatar: the question and its chips stand there, and the card takes its own
    /// presses.
    #[test]
    fn a_narrow_bar_opens_the_ask_as_a_card_under_the_band() {
        let (mut frame, host) = asking_desktop((420, 800));
        let bar = bar_rect(&frame);
        let ask = ask_rect(&frame);
        assert!(
            !ask.is_zero_area(),
            "the ask is open, as a card under the band"
        );
        assert!(
            ask.y0 >= bar.y1,
            "the card {ask:?} stands under the bar {bar:?}"
        );
        assert!(
            ask.x0 >= 0.0 && ask.x1 <= 420.0 && ask.x1 <= bar.x1,
            "the card {ask:?} stays on the bar's width"
        );

        // Its own surface takes the press: nothing under it, and the ask stays open rather than
        // folding as a press elsewhere would.
        host.intents.borrow_mut().clear();
        let at = Point::new(ask.x0 + 8.0, ask.y0 + 8.0);
        Framed::harness_mut(&mut frame).click_at(at.x, at.y);
        settle(&mut frame);
        assert!(
            host.intents.borrow().is_empty(),
            "the card at {at:?} took the press"
        );
        assert!(
            host.ask.borrow().as_ref().is_some_and(|ask| ask.open),
            "a press on the card does not fold it"
        );

        // Its answer stands in the card, and answering there reaches the host.
        let answer = frame.bar().avatar_ask_answer_rect();
        assert!(
            ask.y0 <= answer.y0 && answer.y1 <= ask.y1,
            "the answer {answer:?} is inside the card {ask:?}"
        );
        Framed::harness_mut(&mut frame).click_at(answer.center().x, answer.center().y);
        assert!(
            host.intents
                .borrow()
                .contains(&ChromeIntent::AnswerAvatarAsk),
            "the answer reaches the host"
        );
        assert!(host.ask.borrow().is_none(), "answered, the ask goes");
    }

    /// Folded on a narrow bar, the ask waits as the avatar's dot and opens from it again.
    #[test]
    fn a_narrow_bars_dot_opens_the_ask_again() {
        let (mut frame, host) = asking_desktop((420, 800));
        Framed::harness_mut(&mut frame).key("Escape");
        assert!(
            host.ask.borrow().as_ref().is_some_and(|ask| !ask.open),
            "Escape folds the ask"
        );
        settle(&mut frame);
        assert!(
            ask_rect(&frame).is_zero_area(),
            "folded, only the dot is painted"
        );
        let avatar = frame.bar().own_avatar_rect();
        assert!(!avatar.is_zero_area(), "the own avatar is painted");
        Framed::harness_mut(&mut frame).click_at(avatar.center().x, avatar.center().y);
        assert!(
            host.ask.borrow().as_ref().is_some_and(|ask| ask.open),
            "the avatar's dot opens the ask again"
        );
        settle(&mut frame);
        let bar = bar_rect(&frame);
        let ask = ask_rect(&frame);
        assert!(
            !ask.is_zero_area() && ask.y0 >= bar.y1,
            "the ask opens as a card under the band {ask:?}, not in the band {bar:?}"
        );
    }
}
