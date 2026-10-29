//! The power menu: lock the session, log out, switch user, suspend, restart
//! or shut down the computer.
//!
//! Each of them is one of the app's effects, so the palette, an agent, a
//! script and the menu's own rows reach the same door. The menu hangs from
//! the trailing end of the top bar, over the desktop. What it offers and
//! where it hangs come from the shell ([`PowerSeat`]); what is chosen goes
//! back to it, which alone powers the machine down.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use ui::a11y::{AccessNode, AccessState, Role};
use ui::advanced::{Affine, NodeId, Scene, TaffyTree, WindowEvent};
use ui::framework::{
    Dispatcher, EffectAnswer, EffectCx, EffectStatus, Effected, ResourceApp, ViewCx,
};
use ui::icons::Icon;
use ui::prelude::*;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::widget::{ContextMenu, Element, MenuItem, PanelAlign, TaffyTreeExt, Widget};

use crate::Seat as _;
use crate::presentation::{Phase, Presentation};
use crate::t;

/// The app the shell mounts the menu as.
pub const APP_ID: &str = "so.tilo.power";

/// The power menu's resource: the door its effects are published on. It
/// holds nothing; what the menu does reaches the machine, not a document.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Power {}

#[resource(kind = "power.session", label = "Power")]
impl Power {}

/// What the person can do to their session and their computer. Each acts on
/// the device it is invoked on and on nobody else's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Effect)]
#[effect(operation = "so.tilo.power.session")]
pub enum PowerEffect {
    /// Lock
    #[effect(
        own_device,
        idempotent,
        keywords = "lock screen, lock computer, secure, screensaver, away"
    )]
    Lock,
    /// Log out
    #[effect(
        own_device,
        destructive,
        keywords = "logout, sign out, sign off, end session, leave"
    )]
    LogOut,
    /// Switch user
    #[effect(
        own_device,
        keywords = "switch account, change user, other user, fast user switching"
    )]
    SwitchUser,
    /// Suspend
    #[effect(own_device, keywords = "sleep, standby, hibernate, nap")]
    Suspend,
    /// Restart
    #[effect(
        own_device,
        destructive,
        keywords = "reboot, reset computer, power cycle"
    )]
    Restart,
    /// Shut down
    #[effect(
        own_device,
        destructive,
        keywords = "shutdown, power off, turn off, switch off, halt"
    )]
    ShutDown,
}

impl PowerEffect {
    /// Every effect, in the order the menu shows them.
    pub const ALL: [Self; 6] = [
        Self::Lock,
        Self::LogOut,
        Self::SwitchUser,
        Self::Suspend,
        Self::Restart,
        Self::ShutDown,
    ];

    /// The row's stable name, which scripts and tests press it by.
    pub const fn target(self) -> &'static str {
        match self {
            Self::Lock => "power_lock",
            Self::LogOut => "power_logout",
            Self::SwitchUser => "power_switch_user",
            Self::Suspend => "power_suspend",
            Self::Restart => "power_restart",
            Self::ShutDown => "power_shut_down",
        }
    }

    /// The row's name.
    pub fn label(self) -> String {
        t(match self {
            Self::Lock => "power-lock",
            Self::LogOut => "power-logout",
            Self::SwitchUser => "power-switch-user",
            Self::Suspend => "power-suspend",
            Self::Restart => "power-restart",
            Self::ShutDown => "power-shut-down",
        })
    }

    /// What the row does beyond its name, which assistive technology reads.
    pub fn detail(self) -> String {
        t(match self {
            Self::Lock => "power-lock-detail",
            Self::LogOut => "power-logout-detail",
            Self::SwitchUser => "power-switch-user-detail",
            Self::Suspend => "power-suspend-detail",
            Self::Restart => "power-restart-detail",
            Self::ShutDown => "power-shut-down-detail",
        })
    }

    const fn icon(self) -> Icon {
        match self {
            Self::Lock => Icon::Lock,
            Self::LogOut => Icon::ArrowRight,
            Self::SwitchUser => Icon::Users,
            Self::Suspend => Icon::Moon,
            Self::Restart => Icon::RotateCw,
            Self::ShutDown => Icon::Power,
        }
    }
}

/// What this session may offer and where the menu hangs: plain data the
/// shell sets before it opens the menu.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Offers {
    /// The display manager can open a greeter beside this session. Without
    /// it, Switch user is not offered at all rather than failing.
    pub user_switching: bool,
    /// This session owns the machine's power. A headless or nested session
    /// shares the host with another desktop, so it offers no switching
    /// user, suspend, restart or shut down.
    pub host_power: bool,
    /// How far down the sheet the menu hangs from: the bottom of the top
    /// bar it was opened from.
    pub hangs_from: f64,
}

impl Default for Offers {
    fn default() -> Self {
        Self {
            user_switching: true,
            host_power: true,
            hangs_from: 0.0,
        }
    }
}

impl Offers {
    /// Whether `effect` is offered here.
    pub fn offers(self, effect: PowerEffect) -> bool {
        match effect {
            PowerEffect::SwitchUser => self.user_switching && self.host_power,
            PowerEffect::Suspend | PowerEffect::Restart | PowerEffect::ShutDown => self.host_power,
            PowerEffect::Lock | PowerEffect::LogOut => true,
        }
    }

    /// The offered effects, in the order the menu shows them.
    pub fn offered(self) -> Vec<PowerEffect> {
        PowerEffect::ALL
            .into_iter()
            .filter(|effect| self.offers(*effect))
            .collect()
    }
}

/// What the shell lends the menu: whether it is up, what it offers, and
/// where what the person chose goes.
#[derive(Clone, Debug, Default)]
pub struct PowerSeat {
    presentation: Presentation,
    offers: Rc<Cell<Offers>>,
    asked: Rc<RefCell<Vec<PowerEffect>>>,
    rows: Rc<RefCell<Vec<(PowerEffect, Rect)>>>,
    panel: Rc<Cell<Rect>>,
}

impl crate::Seat for PowerSeat {
    fn presentation(&self) -> &Presentation {
        &self.presentation
    }
}

impl PowerSeat {
    /// What the menu offers from its next opening on.
    pub fn set_offers(&self, offers: Offers) {
        self.offers.set(offers);
    }

    pub fn offers(&self) -> Offers {
        self.offers.get()
    }

    /// What the person chose since the shell last asked, oldest first.
    pub fn take_asked(&self) -> Vec<PowerEffect> {
        std::mem::take(&mut *self.asked.borrow_mut())
    }

    /// Where each row of the open menu is, in the sheet's own coordinates.
    pub fn rows(&self) -> Vec<(PowerEffect, Rect)> {
        self.rows.borrow().clone()
    }

    /// The open menu's panel, its title and rows inside it, in the sheet's
    /// own coordinates; empty while nothing is open.
    pub fn panel(&self) -> Rect {
        self.panel.get()
    }

    fn ask(&self, effect: PowerEffect) {
        self.asked.borrow_mut().push(effect);
    }
}

/// A screenshot scenario to stage, `None` for the plain capture.
struct Staged(Option<String>);

/// The menu over the sheet: a toolkit menu anchored under the top bar's
/// trailing end, opened when the shell puts the sheet up.
struct PowerMenu {
    rect: Rect,
    menu: ContextMenu,
    seat: Option<PowerSeat>,
    /// What the open menu offers, in row order after its title.
    shown: Vec<PowerEffect>,
    offers: Offers,
    /// The shell's opening the menu shows; `None` before the first.
    opening: Option<u64>,
    /// How far the menu stays from the sheet's edges, as the theme last
    /// drawn says.
    margin: f64,
    /// The row a press or Enter chose, taken once the menu has handled it.
    chosen: Rc<Cell<Option<PowerEffect>>>,
    dispatch: Option<Dispatcher<Power>>,
    /// Each effect's address on the published resource, so a row names the
    /// action it runs.
    addresses: HashMap<PowerEffect, ui::tilo_actions::ActionAddress>,
    /// A screenshot scenario to stage on the next sync.
    staged: Rc<RefCell<Option<Staged>>>,
}

impl PowerMenu {
    fn new(seat: Option<PowerSeat>, offers: Offers) -> Self {
        Self {
            rect: Rect::ZERO,
            menu: ContextMenu::new(),
            offers: seat.as_ref().map_or(offers, PowerSeat::offers),
            seat,
            shown: Vec::new(),
            opening: None,
            margin: f64::from(ui::tokens::Spacing::BASE.s2),
            chosen: Rc::default(),
            dispatch: None,
            addresses: HashMap::new(),
            staged: Rc::default(),
        }
    }

    /// Run through the view's door: rows name the actions they run, and
    /// a press that is not the person's own goes through `dispatch`.
    fn attach(&mut self, cx: &ViewCx<Power>) {
        let actions = cx.actions();
        self.addresses = PowerEffect::ALL
            .into_iter()
            .filter_map(|effect| {
                actions
                    .effect(&effect)
                    .map(|binding| (effect, binding.address().clone()))
            })
            .collect();
        self.dispatch = Some(cx.dispatcher());
        if let Some(seat) = &self.seat {
            seat.presentation().wake_with(cx.waker());
        }
    }

    /// What the menu offers now: the shell's say, else its own.
    fn offered_now(&self) -> Offers {
        self.seat.as_ref().map_or(self.offers, PowerSeat::offers)
    }

    fn effect(&mut self, effect: PowerEffect, _cx: &mut EffectCx<Power>) -> EffectAnswer {
        if !self.offered_now().offers(effect) {
            return Err(format!("{} is not offered here", effect.label()));
        }
        if self.seat.is_none() {
            return Err("no session to act on here".to_owned());
        }
        self.choose(effect);
        Ok(Effected::done())
    }

    fn stage(&mut self, scenario: Option<&str>) {
        if scenario == Some("keyboard") {
            // On the last row, as a person driving the menu from the
            // keyboard is after arrowing down to Shut down.
            let _ = self.menu.focus_item(self.shown.len());
        }
    }

    fn items(&self) -> Vec<MenuItem> {
        let mut items = vec![MenuItem::caption(t("power-menu-title"))];
        items.extend(self.shown.iter().map(|&effect| {
            let chosen = Rc::clone(&self.chosen);
            let item = MenuItem::icon_action(effect.icon(), effect.label(), move || {
                chosen.set(Some(effect));
            })
            .with_description(effect.detail());
            match self.addresses.get(&effect) {
                Some(address) => item.for_action(address.clone()),
                None => item,
            }
        }));
        items
    }

    /// Where the menu hangs: under the top bar, against the sheet's
    /// trailing edge.
    fn anchor(&self) -> (Rect, Rect) {
        let margin = self.margin;
        let area = Rect::new(0.0, 0.0, self.rect.width(), self.rect.height());
        let edge = (area.x1 - margin).max(area.x0);
        let top = self.offers.hangs_from.clamp(0.0, area.y1);
        (
            Rect::new(edge, top, edge, top),
            Rect::new(area.x0 + margin, top, edge, area.y1 - margin),
        )
    }

    fn open(&mut self, by_keyboard: bool) {
        if let Some(seat) = &self.seat {
            self.offers = seat.offers();
        }
        self.shown = self.offers.offered();
        self.menu.set_items(self.items());
        self.menu.set_align(PanelAlign::Trailing);
        let (anchor, viewport) = self.anchor();
        self.menu.open_anchored(anchor, viewport);
        if by_keyboard {
            self.menu.focus_first_action();
        }
    }

    /// Follow the shell: open on a new opening, close when it took the
    /// sheet down. With no shell (a capture, a test) the menu is up from
    /// the first frame.
    fn follow(&mut self) {
        let Some(seat) = self.seat.clone() else {
            if self.opening.is_none() {
                self.opening = Some(0);
                self.open(false);
            }
            return;
        };
        let presentation = seat.presentation();
        match presentation.phase() {
            Phase::Open if self.opening != Some(presentation.openings()) => {
                self.opening = Some(presentation.openings());
                self.open(presentation.by_keyboard());
            }
            Phase::Leaving | Phase::Closed if self.menu.is_open() => self.menu.close(),
            _ => {}
        }
    }

    /// Carry out what a row chose, and tell the shell once the menu closed.
    fn settle(&mut self) {
        if let Some(effect) = self.chosen.take() {
            match &self.dispatch {
                // The person's own press is their decision, as a key is.
                Some(dispatch) if dispatch.is_own_gesture(&effect) => self.choose(effect),
                // Anyone acting for them goes through the door, which asks
                // first where the effect says so.
                Some(dispatch) => dispatch.effect(&effect),
                None => self.choose(effect),
            }
        }
        if !self.menu.is_open()
            && let Some(seat) = &self.seat
        {
            seat.presentation().dismissed();
        }
    }

    fn choose(&mut self, effect: PowerEffect) {
        self.menu.close();
        if let Some(seat) = &self.seat {
            seat.ask(effect);
            seat.presentation().dismissed();
        }
    }

    /// Tell the shell where the rows are, and when nothing is drawn.
    fn report(&self) {
        let Some(seat) = &self.seat else {
            return;
        };
        let rows = if self.menu.is_open() {
            self.shown
                .iter()
                .enumerate()
                .filter_map(|(index, &effect)| {
                    self.menu
                        .item_rect(index.saturating_add(1))
                        .map(|rect| (effect, rect))
                })
                .collect()
        } else {
            Vec::new()
        };
        *seat.rows.borrow_mut() = rows;
        seat.panel.set(if self.menu.is_open() {
            self.menu.get_rect()
        } else {
            Rect::ZERO
        });
        if !self.menu.is_drawn() && seat.presentation().phase() == Phase::Leaving {
            seat.presentation().gone();
        }
    }

    fn out_of_step(&self) -> bool {
        self.seat.as_ref().is_some_and(|seat| {
            let presentation = seat.presentation();
            match presentation.phase() {
                Phase::Open => self.opening != Some(presentation.openings()),
                Phase::Leaving => self.menu.is_open() || !self.menu.is_drawn(),
                Phase::Closed => self.menu.is_drawn(),
            }
        })
    }
}

impl Widget for PowerMenu {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = ui::taffy::Style {
            size: ui::taffy::Size::from_percent(1.0, 1.0),
            ..ui::taffy::Style::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    ui::rect_geometry!();

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn update(&mut self, _state: &ui::widget::AppModel, _path: &ui::widget::ModelPath) -> bool {
        self.staged.borrow().is_some()
    }

    fn sync(&mut self, _state: &ui::widget::AppModel, _path: &ui::widget::ModelPath) {
        let staged = self.staged.borrow_mut().take();
        if let Some(Staged(scenario)) = staged {
            self.stage(scenario.as_deref());
        }
    }

    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.margin = ui::widget::floating_surface::viewport_margin(theme);
        self.follow();
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.margin = ui::widget::floating_surface::viewport_margin(theme);
        self.follow();
        if self.menu.is_open() {
            let (anchor, viewport) = self.anchor();
            self.menu.reanchor(anchor, viewport);
        }
        self.menu
            .draw_overlay(scene, transform, text_renderer, theme, presence);
        self.report();
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // A key that follows the opening at once is the open menu's, drawn
        // or not yet.
        self.follow();
        if !self.menu.is_open() {
            return;
        }
        self.menu.handle_event(event, ctx);
        // The menu is modal: nothing it leaves reaches what is under it.
        *ctx.handled = true;
        self.settle();
        self.report();
    }

    fn needs_redraw(&self) -> bool {
        self.menu.is_drawn() || self.out_of_step()
    }

    fn needs_animation_frame(&self) -> bool {
        self.menu.needs_animation_frame()
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.menu.is_open().then_some(self.rect)
    }

    fn traps_focus(&self) -> bool {
        self.menu.is_open()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.menu.is_open()
    }

    fn name(&self) -> &'static str {
        "PowerMenu"
    }

    fn test_rects(&self) -> Vec<(String, Rect)> {
        if !self.menu.is_open() {
            return Vec::new();
        }
        self.shown
            .iter()
            .enumerate()
            .filter_map(|(index, effect)| {
                self.menu
                    .item_rect(index.saturating_add(1))
                    .map(|rect| (effect.target().to_owned(), rect))
            })
            .collect()
    }

    fn accessibility(&self) -> Option<AccessNode> {
        if !self.menu.is_open() {
            return None;
        }
        let description = if self.shown.contains(&PowerEffect::SwitchUser) {
            t("power-menu-description-switch-user")
        } else {
            t("power-menu-description")
        };
        let mut menu = Widget::accessibility(&self.menu)?.with_name(t("power-menu-title"));
        // A row answers to the name of what it does, as scripts press it.
        for (index, effect) in self.shown.iter().enumerate() {
            let row = format!("context-menu:item:{}", index.saturating_add(1));
            if let Some(node) = menu
                .children
                .iter_mut()
                .find(|node| node.action_target.as_deref() == Some(row.as_str()))
            {
                *node = node.clone().with_action_target(effect.target());
            }
        }
        Some(
            AccessNode::new(Role::Dialog)
                .with_name(t("power-menu-title"))
                .with_description(description)
                .with_bounds(Widget::get_rect(&self.menu))
                .with_state(AccessState::new().modal(true))
                .with_children(vec![menu]),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ui::widget::ModelPath,
    ) -> bool {
        let row = self
            .shown
            .iter()
            .position(|effect| effect.target() == target)
            .map(|index| format!("context-menu:item:{}", index.saturating_add(1)));
        let target = row.as_deref().unwrap_or(target);
        let done = self
            .menu
            .perform_semantic_action(target, action, state, path);
        if done {
            self.settle();
            self.report();
        }
        done
    }
}

/// What is not offered under `offers`, and why.
fn unavailable(offers: Offers) -> Vec<EffectStatus> {
    let unavailable = |effect: PowerEffect| match effect {
        PowerEffect::Lock => EffectStatus::unavailable(PowerEffect::LOCK, "not-offered", ""),
        PowerEffect::LogOut => EffectStatus::unavailable(PowerEffect::LOG_OUT, "not-offered", ""),
        PowerEffect::SwitchUser => EffectStatus::unavailable(
            PowerEffect::SWITCH_USER,
            "not-offered",
            "This session cannot switch user",
        ),
        PowerEffect::Suspend => EffectStatus::unavailable(
            PowerEffect::SUSPEND,
            "not-offered",
            "This session does not own the computer's power",
        ),
        PowerEffect::Restart => EffectStatus::unavailable(
            PowerEffect::RESTART,
            "not-offered",
            "This session does not own the computer's power",
        ),
        PowerEffect::ShutDown => EffectStatus::unavailable(
            PowerEffect::SHUT_DOWN,
            "not-offered",
            "This session does not own the computer's power",
        ),
    };
    PowerEffect::ALL
        .into_iter()
        .filter(|effect| !offers.offers(*effect))
        .map(unavailable)
        .collect()
}

/// The power menu as an app.
pub struct PowerApp;

impl ResourceApp for PowerApp {
    type Resource = Power;
    const ID: &'static str = APP_ID;
    const TITLE: &'static str = "Power";
    const ICON: Option<&'static str> = Some("power");
    // Over the desktop: what is not the menu shows the desktop under it.
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        window: ui::application::WindowRole::Sheet,
        ..ui::application::ComponentBehaviour::DEFAULT
    };
    // Nothing is kept: there is no past to look back on.
    const DOCUMENT_HISTORY: bool = false;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::CATALOG)
    }

    fn view(cx: &mut ViewCx<Power>) -> Element {
        let seat = cx.take_lent::<PowerSeat>();
        let offers = cx
            .fixture::<Offers>()
            .map(|offers| *offers)
            .unwrap_or_default();
        let mut menu = PowerMenu::new(seat.clone(), offers);
        menu.attach(cx);
        cx.window_effects(PowerMenu::effect)
            .status(move || unavailable(seat.as_ref().map_or(offers, PowerSeat::offers)));
        let staged = Rc::clone(&menu.staged);
        cx.on_stage(move |scenario| {
            *staged.borrow_mut() = Some(Staged(scenario.map(str::to_owned)));
        });
        Box::new(menu)
    }

    fn scenarios() -> &'static [&'static str] {
        &["open", "keyboard", "detached", "no-switching"]
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
    }

    fn fixture(fixture: &mut ui::framework::Fixture<Power>) {
        let offers = match fixture.scenario() {
            Some("detached") => Offers {
                user_switching: false,
                host_power: false,
                ..Offers::default()
            },
            Some("no-switching") => Offers {
                user_switching: false,
                ..Offers::default()
            },
            _ => Offers::default(),
        };
        fixture.provide(Offers {
            hangs_from: 40.0,
            ..offers
        });
    }
}

/// The app's definition, which the shell mounts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(PowerApp)
}
