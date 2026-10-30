//! The banners' surface: the desktop's banner stack in the corner of the
//! display in use, the banners about the whole machine on the others and a
//! ringing call's edge, over the area the desktop gives them.
//!
//! The surface shows what the desktop lent it ([`Link`]) and puts what the
//! person does to it as [`Ask`]s. Without a desktop (its own window, a
//! capture, a test) it shows a fixed desktop's banners
//! ([`crate::fixture`]) and keeps what is asked of them.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_desktop::notifications::{Banner, BannerFeed, NotificationStack};
use ui::framework::{EffectAnswer, EffectCx, Effected, LiveRow, ResourceSurface, SurfaceCx};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Rect};
use ui::widget::{EventCtx, Widget};
use ui::winit::event::{ElementState, KeyEvent, WindowEvent};

use crate::link::{Ask, Command, Link, Placement, Shown, TakesCommands};
use crate::resource::{BannerEffect, Banners};

/// The banners on screen. What they show is shared with the link, so the
/// desktop's input (a key while the banners hold the keyboard, the pointer
/// over a window under them) is taken the moment it is routed.
pub struct BannerSurface {
    state: Rc<RefCell<State>>,
}

struct State {
    stack: NotificationStack,
    link: Link,
    /// No desktop lent the banners: they show the fixed desktop's.
    own: bool,
    /// The scenario the fixed desktop stages.
    scenario: Option<String>,
    /// The generation of the feed shown.
    shown: u64,
    rect: Rect,
    /// The machine-wide banners and call edges the last frame drew.
    machine: Vec<Rect>,
    call_edges: Vec<Rect>,
    /// Focus was asked for before the stack was first laid out.
    focus_pending: bool,
    dirty: bool,
}

impl BannerSurface {
    /// The banners over the desktop's `link`, or over the fixed desktop when
    /// no desktop lends one.
    pub fn new(link: Option<Link>) -> Self {
        let state = Rc::new(RefCell::new(State::new(link)));
        let taker: Rc<RefCell<dyn TakesCommands>> = state.clone();
        state
            .borrow()
            .link
            .take_commands_with(Rc::downgrade(&taker));
        Self { state }
    }

    /// The link the banners and their desktop talk over.
    pub fn link(&self) -> Link {
        self.state.borrow().link.clone()
    }

    /// What the stack shows.
    pub fn feed(&self) -> BannerFeed {
        self.state.borrow().stack.feed().clone()
    }

    #[cfg(test)]
    fn answer(&self, effect: BannerEffect) -> Result<(), String> {
        self.state.borrow_mut().answer(effect)
    }
}

impl TakesCommands for State {
    fn take(&mut self, command: Command) {
        self.take_command(command);
        self.hand_over();
    }

    fn take_published(&mut self) {
        self.take_feed();
        self.hand_over();
    }
}

impl State {
    fn new(link: Option<Link>) -> Self {
        let (link, own) = match link {
            Some(link) => (link, false),
            None => (Link::new(), true),
        };
        let mut surface = Self {
            stack: NotificationStack::new(),
            link,
            own,
            scenario: None,
            shown: 0,
            rect: Rect::ZERO,
            machine: Vec::new(),
            call_edges: Vec::new(),
            focus_pending: false,
            dirty: true,
        };
        if own {
            surface.stage_scenario(None);
        }
        surface
    }

    fn stage_scenario(&mut self, scenario: Option<&str>) {
        self.scenario = scenario.map(str::to_owned);
        self.link.publish(crate::fixture::feed(scenario));
        self.take_feed();
        if scenario == Some("focused") {
            // A focus ring needs the stack laid out once.
            self.link.command(Command::FocusNewest);
        }
        self.dirty = true;
    }

    /// Show the desktop's newest feed, when there is one.
    fn take_feed(&mut self) {
        if let Some((generation, feed)) = self.link.feed_after(self.shown) {
            self.shown = generation;
            self.stack.set_feed(feed);
            self.dirty = true;
        }
    }

    /// Take what the desktop routed here while the banners were busy, in
    /// order.
    fn take_commands(&mut self) {
        while let Some(command) = self.link.next_command() {
            self.take_command(command);
        }
    }

    fn take_command(&mut self, command: Command) {
        match command {
            Command::Pointer(point) => {
                self.stack.note_pointer(point);
            }
            Command::FocusNewest => {
                // Asked before the stack was first laid out, focus lands
                // once it is.
                self.focus_pending = !self.stack.focus_newest()
                    && self.stack.rect().is_zero_area()
                    && !self.stack.feed().banners.is_empty();
            }
            Command::Key { key, shift } => {
                if !self.stack.handle_key(&key, shift) {
                    self.stack.release_keyboard();
                }
            }
            Command::ReleaseKeyboard => {
                self.stack.release_keyboard();
            }
            Command::CancelPress => self.stack.cancel_pointer_press(),
            Command::Activate(target) => {
                self.stack
                    .perform_semantic(&target, &ui::a11y::Action::Activate);
            }
        }
        self.dirty = true;
    }

    /// Bring what is shown up to date with the desktop.
    fn catch_up(&mut self) {
        self.take_feed();
        self.take_commands();
        self.hand_over();
    }

    /// Put what the stack was asked on the link, and say what is shown. On
    /// its own the surface answers the fixed desktop itself.
    fn hand_over(&mut self) {
        let asks = self.stack.take_asks();
        for ask in asks {
            self.link.ask(Ask::Banner(ask));
        }
        if self.own {
            // The fixed desktop keeps what it is asked and shows no change.
            let _ = self.link.take_asks();
        }
        self.link.show(self.shown_now());
    }

    fn shown_now(&self) -> Shown {
        Shown {
            stack: self.stack.rect(),
            banners: self.stack.banner_rects(),
            machine: self.machine.clone(),
            call_edges: self.call_edges.clone(),
            keyboard: self.stack.has_keyboard_focus(),
            focusable: self.stack.can_focus(),
            captures_pointer: self.stack.wants_pointer_capture(),
            animating: self.stack.needs_animation_frame(),
            deadline: self.stack.next_wake_deadline(),
            accessibility: self.stack.accessibility(),
        }
    }

    fn placement(&self) -> Placement {
        let mut placement = if self.own {
            crate::fixture::placement(
                self.scenario.as_deref(),
                (self.rect.width(), self.rect.height()),
            )
        } else {
            self.link.placement()
        };
        if placement.viewport.is_zero_area() {
            placement.viewport = self.rect;
        }
        placement
    }

    /// The banner `named` names (its id), or the newest when `None`, among
    /// those that `fits`.
    fn banner(
        &self,
        named: Option<&str>,
        fits: impl Fn(&Banner) -> bool,
    ) -> Result<Banner, String> {
        let feed = self.stack.feed();
        if feed.locked {
            return Err("the session is locked".into());
        }
        match named.map(str::trim) {
            Some(id) => {
                let id: u64 = id
                    .parse()
                    .map_err(|_| format!("`{id}` is not a notification's id"))?;
                let banner = feed
                    .banner(id)
                    .ok_or_else(|| format!("no notification {id} is on screen"))?;
                if fits(banner) {
                    Ok(banner.clone())
                } else {
                    Err(format!("notification {id} cannot do that"))
                }
            }
            None => feed
                .banners
                .iter()
                .rev()
                .find(|banner| fits(banner))
                .cloned()
                .ok_or_else(|| "no notification on screen can do that".into()),
        }
    }

    fn answer(&mut self, effect: BannerEffect) -> Result<(), String> {
        use tilo_desktop::notifications::{Addressed, BannerAsk};
        let any = |_: &Banner| true;
        let call =
            |banner: &Banner| banner.notification.addressed() == Some(Addressed::IncomingCall);
        let ask = match effect {
            BannerEffect::Open { banner } => Ask::Banner(BannerAsk::Activate(
                self.banner(banner.as_deref(), any)?.id(),
            )),
            BannerEffect::Dismiss { banner } => Ask::Banner(BannerAsk::Dismiss(
                self.banner(banner.as_deref(), any)?.id(),
            )),
            BannerEffect::Act { banner, choice } => {
                let choice = choice.trim();
                let chosen = |banner: &Banner| choice_key(banner, choice).is_some();
                let banner = self.banner(banner.as_deref(), chosen)?;
                let key = choice_key(&banner, choice).unwrap_or_default();
                Ask::Banner(BannerAsk::Invoke(banner.id(), key))
            }
            BannerEffect::Reply { banner, text } => {
                if text.trim().is_empty() {
                    return Err("a reply needs some text".into());
                }
                let replies = |banner: &Banner| takes_reply(banner);
                let id = self.banner(banner.as_deref(), replies)?.id();
                Ask::Reply { id, text }
            }
            BannerEffect::Snooze { banner } => {
                let snoozes = |banner: &Banner| choice_key(banner, "snooze").is_some();
                let id = self.banner(banner.as_deref(), snoozes)?.id();
                Ask::Banner(BannerAsk::Invoke(id, "snooze".into()))
            }
            BannerEffect::FocusNewest => {
                if !self.stack.focus_newest() {
                    return Err("no notification on screen can take focus".into());
                }
                self.hand_over();
                return Ok(());
            }
            BannerEffect::Read { banner } => Ask::Read(self.banner(banner.as_deref(), any)?.id()),
            BannerEffect::AnswerCall { banner } => {
                Ask::AnswerCall(self.banner(banner.as_deref(), call)?.id())
            }
            BannerEffect::DeclineCall { banner } => {
                Ask::DeclineCall(self.banner(banner.as_deref(), call)?.id())
            }
        };
        self.link.ask(ask);
        self.hand_over();
        Ok(())
    }
}

trait BannerId {
    fn id(&self) -> u64;
}

impl BannerId for Banner {
    fn id(&self) -> u64 {
        self.notification.id
    }
}

/// The key of `banner`'s choice named `choice` by key or label.
fn choice_key(banner: &Banner, choice: &str) -> Option<String> {
    banner
        .actions
        .iter()
        .find(|(key, label)| key == choice || label.eq_ignore_ascii_case(choice))
        .map(|(key, _)| key.clone())
}

/// Whether `banner`'s sender takes an inline reply: it offered the
/// freedesktop `inline-reply` action.
fn takes_reply(banner: &Banner) -> bool {
    banner
        .notification
        .actions
        .iter()
        .any(|(key, _)| key == crate::INLINE_REPLY)
}

impl State {
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.take_feed();
        self.take_commands();
        let placement = self.placement();
        self.stack.set_viewport(placement.viewport);
        self.stack.set_clear_top(placement.clear_top);
        self.stack.set_window_bars(placement.window_bars.clone());
        self.stack.draw(scene, transform, text_renderer, theme);
        // Focus asked for before the stack was laid out lands now; its ring
        // shows on the next frame.
        let again = std::mem::take(&mut self.focus_pending) && self.stack.focus_newest();
        // The state of the whole machine is on every display. Banners about
        // the machine repeat, at rest, on the displays other than the one in
        // use, where the stack takes clicks and keys; a ringing call, whose
        // card is on the display in use, gives the others a quiet edge.
        self.machine.clear();
        self.call_edges.clear();
        let ringing = self.stack.call_ringing();
        for other in &placement.others {
            let banner = self.stack.draw_machine_wide(
                scene,
                transform,
                text_renderer,
                theme,
                (other.viewport, &placement.window_bars),
            );
            if !banner.is_zero_area() {
                self.machine.push(banner);
            }
            if ringing {
                self.call_edges.push(other.display);
                ui::chrome::shell::paint_display_edge(
                    scene,
                    transform,
                    other.display,
                    ui::chrome::shell::DisplayEdge::Ringing,
                    theme,
                );
            }
        }
        self.dirty = again;
        self.hand_over();
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.catch_up();
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.stack.owns_key(logical_key) => {
                let shift = ctx.modifiers.state().shift_key();
                if self.stack.handle_key(logical_key, shift) {
                    *ctx.handled = true;
                }
            }
            _ => {
                if self
                    .stack
                    .handle_pointer_event(event, ctx.scale_factor, ctx.handled)
                {
                    self.dirty = true;
                }
            }
        }
        self.hand_over();
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.link.has_commands()
            || self.link.generation() != self.shown
            || self.stack.needs_animation_frame()
            || self
                .stack
                .next_wake_deadline()
                .is_some_and(|deadline| deadline <= ui::time::now())
    }

    fn test_rects(&self) -> Vec<(String, Rect)> {
        let mut rects = Vec::new();
        if !self.stack.rect().is_zero_area() {
            rects.push(("banners-stack".to_owned(), self.stack.rect()));
        }
        for (id, rect) in self.stack.banner_rects() {
            rects.push((format!("banner:{id}"), rect));
        }
        for (at, rect) in self.machine.iter().enumerate() {
            rects.push((format!("machine-banner:{at}"), *rect));
        }
        for (at, rect) in self.call_edges.iter().enumerate() {
            rects.push((format!("call-edge:{at}"), *rect));
        }
        rects
    }
}

impl Widget for BannerSurface {
    fn name(&self) -> &'static str {
        "NotificationBanners"
    }

    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        let style = ui::taffy::Style {
            size: ui::taffy::Size::from_percent(1.0, 1.0),
            ..ui::taffy::Style::default()
        };
        let node = taffy
            .new_leaf(style)
            .unwrap_or_else(|_| ui::taffy::NodeId::from(0_u64));
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(
        &mut self,
        taffy: &ui::taffy::TaffyTree<()>,
        node: ui::taffy::NodeId,
        origin: ui::vello::kurbo::Point,
    ) {
        self.state.borrow_mut().rect = ui::widget::layout_rect(taffy, node, origin);
    }

    fn get_rect(&self) -> Rect {
        self.state.borrow().rect
    }

    fn sync(&mut self, _state: &ui::widget::AppModel, _path: &ui::widget::ModelPath) {
        self.state.borrow_mut().catch_up();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.state
            .borrow_mut()
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.state.borrow_mut().handle_event(event, ctx);
    }

    fn needs_redraw(&self) -> bool {
        self.state.borrow().needs_redraw()
    }

    fn needs_animation_frame(&self) -> bool {
        self.state.borrow().stack.needs_animation_frame()
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.state.borrow().stack.next_wake_deadline()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.state.borrow().stack.wants_pointer_capture()
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.state.borrow().stack.accessibility()
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        _state: &mut ui::widget::Edits<'_>,
        _path: &ui::widget::ModelPath,
    ) -> bool {
        let mut state = self.state.borrow_mut();
        let pressed = state.stack.perform_semantic(target, action);
        state.hand_over();
        pressed
    }

    fn test_rects(&self) -> Vec<(String, Rect)> {
        self.state.borrow().test_rects()
    }
}

impl ResourceSurface<Banners, BannerEffect> for BannerSurface {
    fn present(&mut self, _state: &Rc<Banners>) {}

    fn attach(&mut self, _cx: SurfaceCx<Banners>) {}

    fn stage(&mut self, scenario: Option<&str>) {
        let mut state = self.state.borrow_mut();
        if state.own {
            state.stage_scenario(scenario);
        }
    }

    fn effect(&mut self, effect: BannerEffect, _cx: &mut EffectCx<Banners, Self>) -> EffectAnswer {
        let mut state = self.state.borrow_mut();
        state.catch_up();
        state.answer(effect)?;
        state.dirty = true;
        Ok(Effected::done())
    }

    /// The banners on screen, each opened through [`BannerEffect::Open`]:
    /// what an agent reads to find the banner to act on.
    fn live_rows(&self) -> Vec<LiveRow> {
        let state = self.state.borrow();
        let feed: &BannerFeed = state.stack.feed();
        if feed.locked {
            return Vec::new();
        }
        feed.banners
            .iter()
            .rev()
            .map(|banner| {
                let id = banner.id();
                LiveRow::new(
                    format!("notifications.banner/{id}"),
                    "notifications.banner",
                    banner.notification.title.clone(),
                )
                .effect(&BannerEffect::Open {
                    banner: Some(id.to_string()),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop::notifications::BannerAsk;

    fn hosted(scenario: &str) -> State {
        let link = Link::new();
        link.publish(crate::fixture::feed(Some(scenario)));
        let mut surface = State::new(Some(link));
        surface.catch_up();
        surface
    }

    fn newest(surface: &State) -> u64 {
        surface.stack.feed().banners.last().map_or(0, BannerId::id)
    }

    #[test]
    fn actions_name_their_banner_or_the_newest_and_reach_the_desktop() {
        let mut surface = hosted("banners");
        let message = newest(&surface);
        surface
            .answer(BannerEffect::Reply {
                banner: None,
                text: "On it".into(),
            })
            .unwrap();
        surface
            .answer(BannerEffect::Snooze { banner: None })
            .unwrap();
        surface
            .answer(BannerEffect::Dismiss {
                banner: Some(message.to_string()),
            })
            .unwrap();
        let asks = surface.link.take_asks();
        let alarm = surface.stack.feed().banners[1].id();
        assert_eq!(
            asks,
            [
                Ask::Reply {
                    id: message,
                    text: "On it".into()
                },
                Ask::Banner(BannerAsk::Invoke(alarm, "snooze".into())),
                Ask::Banner(BannerAsk::Dismiss(message)),
            ]
        );
    }

    #[test]
    fn what_a_banner_does_not_offer_is_refused() {
        let mut surface = hosted("banners");
        let build = surface.stack.feed().banners[0].id();
        assert!(
            surface
                .answer(BannerEffect::Reply {
                    banner: Some(build.to_string()),
                    text: "thanks".into(),
                })
                .is_err(),
            "a build result takes no reply"
        );
        assert!(
            surface
                .answer(BannerEffect::AnswerCall { banner: None })
                .is_err(),
            "nothing rings"
        );
        assert!(
            surface
                .answer(BannerEffect::Act {
                    banner: None,
                    choice: "Approve".into(),
                })
                .is_err()
        );
        assert!(surface.link.take_asks().is_empty());
    }

    #[test]
    fn a_ringing_call_is_answered_or_declined() {
        let mut surface = hosted("call");
        let call = newest(&surface);
        surface
            .answer(BannerEffect::AnswerCall { banner: None })
            .unwrap();
        surface
            .answer(BannerEffect::DeclineCall { banner: None })
            .unwrap();
        assert_eq!(
            surface.link.take_asks(),
            [Ask::AnswerCall(call), Ask::DeclineCall(call)]
        );
    }

    #[test]
    fn a_locked_session_acts_on_no_banner_and_lists_none() {
        let link = Link::new();
        link.publish(crate::fixture::feed(Some("locked")));
        let surface = BannerSurface::new(Some(link));
        surface.state.borrow_mut().catch_up();
        assert!(
            surface
                .answer(BannerEffect::Dismiss { banner: None })
                .is_err()
        );
        assert!(surface.live_rows().is_empty());
    }

    #[test]
    fn a_command_routed_while_the_banners_are_idle_is_taken_at_once() {
        let link = Link::new();
        link.publish(crate::fixture::feed(Some("banners")));
        let surface = BannerSurface::new(Some(link.clone()));
        surface.state.borrow_mut().catch_up();
        link.command(Command::ReleaseKeyboard);
        assert!(!link.has_commands(), "taken, not queued");
    }
}
