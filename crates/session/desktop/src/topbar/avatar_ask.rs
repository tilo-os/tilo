//! The ask about the person themselves, put to them from their own avatar
//! ([`crate::chrome::AvatarAsk`]).
//!
//! Open, it is a small pill in the bar's own band, left of the avatar and of
//! the faces, people button and notices beside it, sized to its words: the
//! question, what answering brings where there is room, "Not now" and the
//! answer. Where it does not fit beside the clock's lane it takes that lane
//! too, over the glance value it covers, and a bar narrow enough to leave no
//! room anywhere gives it the band itself: the ask stands on the bar and on
//! nothing else, whatever the width.
//!
//! It never leaves the band, so it can never cover a window's bar or content,
//! and it can never be covered by the cards the bar opens: those hang below
//! the band, from the control that opened them. Where the band has no room for
//! it at all — while this view follows someone, the middle lane is the
//! banner's that says whose view it is and offers Stop — it waits as the
//! avatar's dot until the band can hold it again.
//!
//! A press anywhere else folds it into a dot on the avatar and still reaches
//! what it was pressed on. Escape folds it too. A click on the avatar while it
//! waits as the dot opens it again.
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

/// Where the open ask's parts were painted, for its hit tests: the pill it stands on in the band,
/// and its chips.
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

    /// The open ask in the bar's band, ending at `right_x`: left of the avatar and of everything
    /// else the bar draws on its right, so it never stands over them.
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
        let gap = f64::from(theme.tokens.spacing.s4);
        let seat = f64::from(theme.tokens.spacing.s3);
        // The band's rooms for it, right to left. First the ones that keep the bar's own glance
        // values — beside the clock's lane, the lane's free room up to the clock, the clock's lane —
        // then the strip past the workspace's own chip, then the band itself: a bar too narrow to
        // keep them all gives the ask the band, so the question stands on the bar and on nothing
        // else, whatever the width.
        // While this view follows someone the lane is the banner's, which says whose view it is and
        // offers Stop: the ask never takes it (behind "Following Claude Code" its words were cut
        // off), and stands either side of it instead.
        let past_workspace = self.ws_pill_rect.x1 + gap;
        let mut rooms = vec![Room::beside(right_x, self.center_lane.x1 + gap)];
        if self.following {
            rooms.push(Room::beside(self.center_lane.x0 - gap, past_workspace));
        } else {
            rooms.push(Room::beside(right_x, self.center_rect.x1 + gap));
            rooms.push(Room::beside(right_x, self.center_lane.x0 + gap));
            rooms.push(Room::beside(right_x, past_workspace));
            rooms.push(Room::band(self.rect.x0 + seat, self.rect.x1 - seat));
        }
        // The question stands whole wherever a room holds it. Its decline gives way first — a press
        // anywhere else folds the ask — so the question is never cut where its own words fit beside
        // the answer alone. Only where no room holds it whole do its words shorten, in the first
        // room that held them at all: taking the first room that held any words cut "Keep your name
        // and work?" short beside a clock with room to spare.
        let mut words = None;
        let mut shortened = None;
        for with_later in [true, false] {
            for room in &rooms {
                let Some(fit) = AskWords::fit(&ask, room.width(), with_later, theme, text_renderer)
                else {
                    continue;
                };
                if fit.question == ask.question {
                    words = Some((fit, room));
                    break;
                }
                shortened.get_or_insert((fit, room));
            }
            if words.is_some() {
                break;
            }
        }
        // Nowhere in the band for its words: it waits as the avatar's dot.
        let Some((words, room)) = words.or(shortened) else {
            return;
        };
        let pad = f64::from(theme.tokens.spacing.s2);
        let gap = f64::from(theme.tokens.spacing.s1);
        let (right_x, width) = (room.right, room.pill_width(words.width(pad, gap)));
        // The bar's own band: never below it, where the windows' bars are.
        let top = (center_y - BAR_CONTROL_PX / 2.0).max(self.rect.y0);
        let bottom = (center_y + BAR_CONTROL_PX / 2.0).min(self.rect.y1);
        let pill = Rect::new(right_x - width, top, right_x, bottom);
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
        let later = words.later.as_ref().map_or(Rect::ZERO, |later| {
            let rect = Rect::new(
                answer.x0 - gap - words.later_w,
                chip_top,
                answer.x0 - gap,
                chip_top + chip_h,
            );
            let hovered = self.hovered_control();
            ChipFace::new(rect, later)
                .hovered(hovered.as_deref() == Some(Self::CONTROL_ASK_LATER))
                .paint(scene, transform, text_renderer, theme);
            rect
        });
        let hovered = self.hovered_control();
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

/// A room the bar's band gives the ask: the right end its pill hangs from, the leftmost edge it
/// may reach, and whether it stands the room's whole width there.
struct Room {
    right: f64,
    limit: f64,
    /// The band itself, where the ask is what the bar is saying: it takes the room whole, so
    /// nothing the bar draws stands clipped at its edge.
    spans: bool,
}

impl Room {
    /// A room the ask takes only the width its words need, the bar's own controls past it kept.
    fn beside(right: f64, limit: f64) -> Self {
        Self {
            right,
            limit,
            spans: false,
        }
    }

    /// The band from `left` to `right`, taken whole.
    fn band(left: f64, right: f64) -> Self {
        Self {
            right,
            limit: left,
            spans: true,
        }
    }

    fn width(&self) -> f64 {
        self.right - self.limit
    }

    /// The pill's width for words this wide: the band's whole room where it spans it, and the
    /// words' own width otherwise.
    fn pill_width(&self, words: f64) -> f64 {
        if self.spans { self.width() } else { words }
    }
}

/// The open ask's words, fitted to the room the bar leaves it.
struct AskWords {
    question: String,
    question_w: f64,
    /// What answering brings, where there is room beside the question.
    detail: Option<String>,
    detail_w: f64,
    /// The decline, worn where the band has room for it beside the question: a bar too narrow for
    /// both asks only the answer, and a press anywhere else declines.
    later: Option<String>,
    later_w: f64,
    answer_w: f64,
}

impl AskWords {
    /// Fit `ask` into `room` px of bar, or `None` where its question would not read: the chips
    /// stand beside the question on the pill's one line, `with_later` naming whether the decline
    /// stands beside the answer.
    fn fit(
        ask: &AvatarAsk,
        room: f64,
        with_later: bool,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) -> Option<Self> {
        use ui::widget::{ChipFace, ChipLead};

        let tokens = &theme.tokens;
        let pad = f64::from(tokens.spacing.s2);
        let gap = f64::from(tokens.spacing.s1);
        let later = with_later.then(|| t("notification-not-now"));
        let later_w = later.as_ref().map_or(0.0, |later| {
            ChipFace::intrinsic_width(later, ChipLead::None, false, text_renderer, theme)
        });
        let answer_w =
            ChipFace::intrinsic_width(&ask.answer, ChipLead::None, false, text_renderer, theme);
        let (question_style, detail_style) = Self::styles(theme);
        let beside = if with_later {
            later_w + gap + answer_w + gap
        } else {
            answer_w + gap
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
        let decline = if self.later.is_some() {
            self.later_w + gap
        } else {
            0.0
        };
        pad + self.question_w + self.detail_w + gap + decline + self.answer_w + pad
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
                // The person may invite, as on the hosted page: the bar's right side carries the
                // invite button too.
                invite: crate::chrome::Invite::Offered,
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
        asking_desktop_over(viewport, &tree())
    }

    /// The real chrome over `tree` , at `viewport` , the ask open.
    fn asking_desktop_over(
        viewport: (u32, u32),
        tree: &DesktopTree,
    ) -> (Framed<DesktopChrome>, Rc<Asking>) {
        ui::i18n::I18nManager::init("en-US");
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
        let bar = TopBarChrome::from_tree(tree).with_chrome(chrome);
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

    /// With others here, the ask stands clear of their faces, the people button, the invite button
    /// and the person's own avatar wherever the band has room beside them, and keeps the workspace's
    /// own chip. The clock in turn keeps clear of the faces and the buttons beside them. A bar too
    /// narrow for all of that gives the ask the band itself, which the band's own test holds.
    #[test]
    fn the_open_ask_never_stands_over_the_faces_or_the_people_button() {
        let mut crowded = tree();
        crowded.workspaces[0].collaborators = vec![
            tilo_desktop_model::Collaborator::new("agent", "Claude Code"),
            tilo_desktop_model::Collaborator::new("ada", "Ada Lovelace"),
        ];
        for viewport in [(1280, 800), (1024, 768), (720, 900), (390, 844), (360, 780)] {
            let (frame, _) = asking_desktop_over(viewport, &crowded);
            let bar = frame.bar();
            let ask = [bar.ask.pill, bar.ask.answer, bar.ask.later];
            assert!(!ask[0].is_zero_area(), "{viewport:?}: the ask is open");
            let others = [
                ("faces", bar.avatar_stack_rect),
                ("people button", bar.people_button_rect),
                ("invite", bar.invite_rect),
                ("own avatar", bar.own_avatar_rect),
            ];
            assert!(
                !bar.collaborators_rect().is_zero_area(),
                "{viewport:?}: who is here stands on the bar"
            );
            // The clock and the bar's right side stand apart: the faces that do not fit join the
            // badge, and the clock moves left of the buttons, or aside.
            for (name, rect) in others {
                assert!(
                    rect.intersect(bar.center_rect).is_zero_area(),
                    "{viewport:?}: the {name} {rect:?} stands over the clock {:?}",
                    bar.center_rect
                );
            }
            // Below this the band has no room beside them at all: the ask is the band's.
            if viewport.0 < 720 {
                continue;
            }
            for part in ask {
                assert!(
                    part.intersect(bar.ws_pill_rect).is_zero_area(),
                    "{viewport:?}: the ask's {part:?} stands over the workspace {:?}",
                    bar.ws_pill_rect
                );
            }
            for part in ask {
                for (name, rect) in others {
                    assert!(
                        part.intersect(rect).is_zero_area(),
                        "{viewport:?}: the ask's {part:?} stands over the {name} {rect:?}"
                    );
                }
            }
        }
    }

    /// While this view follows someone, the middle lane is the banner's: the ask stands beside it
    /// or waits as the avatar's dot, and never in it, at every width.
    #[test]
    fn the_open_ask_never_stands_in_the_following_banner() {
        let mut followed = tree();
        let mut agent = tilo_desktop_model::Collaborator::new("agent", "Claude Code");
        agent.following = true;
        followed.workspaces[0].collaborators = vec![agent];
        for viewport in [(1280, 800), (1024, 768), (390, 844)] {
            let (frame, _) = asking_desktop_over(viewport, &followed);
            let bar = frame.bar();
            assert!(bar.following, "{viewport:?}: the bar follows");
            let lane = bar.center_lane;
            assert!(
                !lane.is_zero_area(),
                "{viewport:?}: the banner keeps its lane"
            );
            let bar_rect = bar_rect(&frame);
            for part in [bar.ask.pill, bar.ask.answer, bar.ask.later] {
                if part.is_zero_area() {
                    continue;
                }
                assert!(
                    part.intersect(lane).is_zero_area(),
                    "{viewport:?}: the ask's {part:?} stands in the banner's lane {lane:?}"
                );
                assert!(
                    part.y0 >= bar_rect.y0 && part.y1 <= bar_rect.y1,
                    "{viewport:?}: the ask's {part:?} leaves the bar's band {bar_rect:?}"
                );
            }
            // Beside the banner with the band's room to spare, the question stands there rather
            // than waiting as the dot.
            if viewport.0 == 1280 {
                assert!(
                    !bar.ask.pill.is_zero_area(),
                    "{viewport:?}: the ask stands beside the banner"
                );
                assert!(
                    bar.ask.pill.x1 <= lane.x0,
                    "{viewport:?}: the ask {:?} stands before the banner's lane {lane:?}",
                    bar.ask.pill
                );
            }
        }
    }

    /// The ask stands in the bar's band at every width, with faces on the bar and on a phone: it is
    /// the band's or the avatar's dot, never a card under the band, where the windows' own bars are.
    #[test]
    fn the_open_ask_stays_in_the_bar_band() {
        let mut crowded = tree();
        crowded.workspaces[0].collaborators = vec![
            tilo_desktop_model::Collaborator::new("agent", "Claude Code"),
            tilo_desktop_model::Collaborator::new("ada", "Ada Lovelace"),
        ];
        for viewport in [
            (1920, 1080),
            (1440, 900),
            (1280, 800),
            (1280, 720),
            (1024, 768),
            (720, 900),
            (420, 800),
            (390, 844),
            (360, 780),
        ] {
            for tree in [&tree(), &crowded] {
                let (frame, _) = asking_desktop_over(viewport, tree);
                let ask = ask_rect(&frame);
                assert!(!ask.is_zero_area(), "{viewport:?}: the ask is open");
                let bar = bar_rect(&frame);
                assert!(
                    ask.y0 >= bar.y0 && ask.y1 <= bar.y1,
                    "{viewport:?}: the ask {ask:?} stays in the bar's band {bar:?}"
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
    }

    /// The cards the bar opens hang under the band, from the control that opened them: a face's or
    /// the person's own avatar's. The ask is the band's, so no card ever meets it, and answering
    /// stays where it is shown.
    #[test]
    fn the_open_ask_never_meets_a_card_the_bar_opens() {
        use ui::widget::floating_surface::{PanelAlign, PopoverSide, panel_room, place_aligned};

        let mut crowded = tree();
        crowded.workspaces[0].collaborators = vec![
            tilo_desktop_model::Collaborator::new("agent", "Claude Code"),
            tilo_desktop_model::Collaborator::new("ada", "Ada Lovelace"),
        ];
        let theme = Theme::dark();
        let gap = f64::from(theme.tokens.spacing.s1);
        for viewport in [(1280, 800), (720, 900), (390, 844)] {
            let (frame, _) = asking_desktop_over(viewport, &crowded);
            let bar = frame.bar();
            let bar_rect = bar_rect(&frame);
            let window = Rect::new(0.0, 0.0, f64::from(viewport.0), f64::from(viewport.1));
            let room = panel_room(window, &theme);
            let faces = bar.avatar_stack_rect;
            let anchors = [("own avatar", bar.own_avatar_rect), ("faces", faces)];
            for (name, anchor) in anchors {
                if anchor.is_zero_area() {
                    continue;
                }
                // The card a face or the avatar opens: under the band, trailing the anchor, in the
                // room the layer keeps from the window's edges.
                let (card, _) = place_aligned(
                    anchor,
                    (280.0, 320.0),
                    PopoverSide::Below,
                    PanelAlign::Trailing,
                    gap,
                    room,
                );
                assert!(
                    card.y0 >= bar_rect.y1,
                    "{viewport:?}: the {name}'s card {card:?} hangs under the band {bar_rect:?}"
                );
                for part in [bar.ask.pill, bar.ask.answer, bar.ask.later] {
                    assert!(
                        part.intersect(card).is_zero_area(),
                        "{viewport:?}: the ask's {part:?} stands under the {name}'s card {card:?}"
                    );
                }
            }
        }
    }

    /// Where the bar has room for the whole question beside the clock, the question stands whole:
    /// it is not cut short in the lane's first room when a wider room beside it holds all of it.
    #[test]
    fn the_whole_question_stands_where_the_bar_has_room() {
        for viewport in [
            (1024, 768),
            (1100, 800),
            (1180, 800),
            (1280, 800),
            (1440, 900),
        ] {
            let (frame, _) = asking_desktop(viewport);
            let bar = frame.bar();
            let theme = Theme::dark();
            let (style, _) = AskWords::styles(&theme);
            let mut text = TextRenderer::new();
            let (whole, _) = text.measure_style("Keep your name and work?", None, &style);
            let room = bar.ask.later.x0 - bar.ask.pill.x0;
            assert!(
                room >= f64::from(whole),
                "{viewport:?}: the question needs {whole} px but stands in {room} px"
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

    /// A bar too narrow for the ask's words beside its own controls opens it across the band: the
    /// question and its chips stand there, on the bar and on nothing else, and the pill takes its
    /// own presses.
    #[test]
    fn a_narrow_bar_opens_the_ask_across_the_band() {
        let (mut frame, host) = asking_desktop((420, 800));
        let bar = bar_rect(&frame);
        let ask = ask_rect(&frame);
        assert!(!ask.is_zero_area(), "the ask is open, across the band");
        assert!(
            ask.y0 >= bar.y0 && ask.y1 <= bar.y1,
            "the ask {ask:?} stays in the bar's band {bar:?}"
        );
        assert!(
            ask.x0 >= bar.x0 && ask.x1 <= bar.x1,
            "the ask {ask:?} stays on the bar's width"
        );

        // Its own surface takes the press: nothing under it, and the ask stays open rather than
        // folding as a press elsewhere would.
        host.intents.borrow_mut().clear();
        let at = Point::new(ask.x0 + 8.0, ask.y0 + 8.0);
        Framed::harness_mut(&mut frame).click_at(at.x, at.y);
        settle(&mut frame);
        assert!(
            host.intents.borrow().is_empty(),
            "the pill at {at:?} took the press"
        );
        assert!(
            host.ask.borrow().as_ref().is_some_and(|ask| ask.open),
            "a press on the pill does not fold it"
        );

        // Its answer stands in the pill, and answering there reaches the host.
        let answer = frame.bar().avatar_ask_answer_rect();
        assert!(
            ask.y0 <= answer.y0 && answer.y1 <= ask.y1,
            "the answer {answer:?} is inside the pill {ask:?}"
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

    /// Folded on a narrow bar, the ask waits as the avatar's dot and opens from it again, in the
    /// band.
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
            !ask.is_zero_area() && ask.y0 >= bar.y0 && ask.y1 <= bar.y1,
            "the ask opens in the band {ask:?}, not under it {bar:?}"
        );
    }
}
