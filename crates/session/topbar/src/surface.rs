//! The bar's surface: the full bar along the display in use, the compact
//! bar along every other display, and the menu or card a bar opens, over
//! the area the desktop gives them.
//!
//! The bars are the desktop's shared bar widget
//! ([`tilo_desktop::topbar::TopBarChrome`]) and the menus its workspace menu
//! and profile card; this surface feeds them what the desktop lent
//! ([`Link`]) and puts what they ask as [`Ask`]s. Without a desktop (its own
//! window, a capture, a test) it shows a fixed desktop ([`crate::fixture`])
//! and keeps what is asked of it.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use tilo_desktop::chrome::{
    AppTrayItem, AppTrayRequest, AvatarAsk, Chrome, ChromeIntent, ClockSnapshot,
    DesktopChromeSnapshot, DesktopChromeSource,
};
use tilo_desktop::profile_card::ProfileCard;
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop::workspace_menu::{WorkspaceMenu, WorkspaceMenuChoice};
use tilo_desktop_model::DesktopTree;
use ui::framework::{EffectAnswer, EffectCx, Effected, LiveRow, ResourceSurface, SurfaceCx};
use ui::presence::PresenceCtx;
use ui::taffy::{NodeId, TaffyTree};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Point, Rect};
use ui::widget::{AppModel, EventCtx, ModelPath, TaffyTreeExt as _, Widget};
use ui::winit::event::{ElementState, WindowEvent};

use crate::link::{Ask, BarFeed, BarPlace, Command, Link, MediaCommand, Placement, Shown};
use crate::resource::{TopBar, TopBarEffect};

/// A menu or card a bar asked to open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Popover {
    WorkspaceMenu,
    Profile,
}

/// The chrome source every bar reads: the desktop's last feed, with what a
/// bar asks put on the link.
struct FeedChrome {
    link: Link,
    feed: RefCell<BarFeed>,
    generation: Cell<u64>,
    /// A bar asked for a menu or card since the surface last looked.
    popover: Cell<Option<Popover>>,
    /// Where the bar that asked for the workspace menu drew the name.
    anchor: Cell<Rect>,
    /// The wall clock, read once a second.
    clock: RefCell<Option<(u64, ClockSnapshot)>>,
}

impl FeedChrome {
    fn new(link: Link) -> Self {
        Self {
            link,
            feed: RefCell::new(BarFeed::default()),
            generation: Cell::new(0),
            popover: Cell::new(None),
            anchor: Cell::new(Rect::ZERO),
            clock: RefCell::new(None),
        }
    }

    fn wall_clock(&self) -> ClockSnapshot {
        let second = ui::time::unix_now_ns() / 1_000_000_000;
        let mut clock = self.clock.borrow_mut();
        match clock.as_ref() {
            Some((at, snapshot)) if *at == second => snapshot.clone(),
            _ => {
                let snapshot = ClockSnapshot::now();
                *clock = Some((second, snapshot.clone()));
                snapshot
            }
        }
    }
}

impl DesktopChromeSource for FeedChrome {
    fn snapshot(&self) -> DesktopChromeSnapshot {
        let feed = self.feed.borrow();
        let mut snapshot = feed.snapshot.clone();
        snapshot.revision = self.generation.get();
        snapshot.clock = feed.clock.clone().unwrap_or_else(|| self.wall_clock());
        snapshot
    }

    fn tree(&self) -> DesktopTree {
        self.feed.borrow().tree.clone()
    }

    fn intent(&self, intent: ChromeIntent) -> bool {
        match intent {
            ChromeIntent::ToggleWorkspaceMenu => self.popover.set(Some(Popover::WorkspaceMenu)),
            ChromeIntent::ToggleProfile => self.popover.set(Some(Popover::Profile)),
            // The desktop's bar carries no Tilo mark: About is in Settings.
            ChromeIntent::ToggleTiloMenu => return false,
            intent => self.link.ask(Ask::Chrome(intent)),
        }
        true
    }

    fn settings_anchor(&self, rect: Rect) {
        self.anchor.set(rect);
    }

    fn continuity_offer(&self) -> Option<String> {
        self.feed.borrow().continuity.clone()
    }

    fn avatar_ask(&self) -> Option<AvatarAsk> {
        self.feed.borrow().avatar_ask.clone()
    }

    fn app_tray_revision(&self) -> u64 {
        self.feed.borrow().app_tray_revision
    }

    fn app_tray(&self) -> Vec<AppTrayItem> {
        self.feed.borrow().app_tray.clone()
    }

    fn app_notification_badge(&self, app_id: &str) -> u32 {
        self.feed
            .borrow()
            .app_badges
            .get(app_id)
            .copied()
            .unwrap_or(0)
    }
}

/// One display's bar.
struct Bar {
    place: BarPlace,
    widget: TopBarChrome,
}

/// The bars and their menus on screen.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent fact about the surface that changes on its own"
)]
pub struct TopBarSurface {
    link: Link,
    source: Rc<FeedChrome>,
    /// No desktop lent the bar: it shows the fixed desktop.
    own: bool,
    /// The scenario the fixed desktop stages.
    scenario: Option<String>,
    placement: Placement,
    bars: Vec<Bar>,
    menu: WorkspaceMenu,
    profile: ProfileCard,
    bar_nodes: Vec<NodeId>,
    menu_node: Option<NodeId>,
    profile_node: Option<NodeId>,
    rect: Rect,
    /// The desktop moved keyboard focus into the bar (its chord): that
    /// focus holds a hidden bar shown, as Tab into it does.
    focus_asked: bool,
    /// A scenario's menu or card, opened once the bar has drawn the
    /// control it hangs from.
    staged_popover: Option<Popover>,
    /// The bars changed and the layout tree holds the old ones.
    restructured: bool,
    dirty: bool,
}

impl TopBarSurface {
    /// The bar over the desktop's `link`, or over the fixed desktop when no
    /// desktop lends one.
    #[must_use]
    pub fn new(link: Option<Link>) -> Self {
        let (link, own) = match link {
            Some(link) => (link, false),
            None => (Link::new(), true),
        };
        let mut surface = Self {
            source: Rc::new(FeedChrome::new(link.clone())),
            link,
            own,
            scenario: None,
            placement: Placement::default(),
            bars: Vec::new(),
            menu: WorkspaceMenu::default(),
            profile: ProfileCard::default(),
            bar_nodes: Vec::new(),
            menu_node: None,
            profile_node: None,
            rect: Rect::ZERO,
            focus_asked: false,
            staged_popover: None,
            restructured: true,
            dirty: true,
        };
        if own {
            surface.stage_scenario(None);
        }
        surface
    }

    /// The link the bar and its desktop talk over.
    #[must_use]
    pub fn link(&self) -> Link {
        self.link.clone()
    }

    /// The full bar, when there is one.
    #[must_use]
    pub fn full_bar(&self) -> Option<&TopBarChrome> {
        self.bars
            .iter()
            .find(|bar| bar.place.full)
            .map(|bar| &bar.widget)
    }

    /// The workspace menu's rows as shown, while it is open.
    #[must_use]
    pub fn workspace_menu_labels(&self) -> Option<Vec<Option<String>>> {
        self.menu.is_open().then(|| self.menu.labels())
    }

    /// Choose the workspace menu's row labelled `label`, as a click would.
    pub fn choose_in_workspace_menu(&mut self, label: &str) -> bool {
        let chosen = self.menu.choose(label);
        self.settle_popovers();
        self.hand_over();
        chosen
    }

    /// Whether the profile card is open.
    #[must_use]
    pub fn profile_open(&self) -> bool {
        self.profile.is_open()
    }

    /// Open the workspace menu under the full bar's workspace name, or close
    /// it, as a press on the name does.
    pub fn toggle_workspace_menu(&mut self) {
        self.source.anchor.set(Rect::ZERO);
        self.open_popover(Popover::WorkspaceMenu);
        self.hand_over();
    }

    /// Open the profile card under the person's own avatar, or close it, as
    /// a press on the avatar does.
    pub fn toggle_profile(&mut self) {
        self.open_popover(Popover::Profile);
        self.hand_over();
    }

    fn stage_scenario(&mut self, scenario: Option<&str>) {
        self.scenario = scenario.map(str::to_owned);
        self.link.publish(crate::fixture::feed(scenario));
        self.take_feed();
        self.dirty = true;
    }

    /// Show the desktop's newest feed, when there is one.
    fn take_feed(&mut self) {
        let seen = self.source.generation.get();
        if let Some((generation, feed)) = self.link.feed_after(seen) {
            *self.source.feed.borrow_mut() = feed;
            self.source.generation.set(generation);
            self.dirty = true;
        }
    }

    /// Where the bars go now: the desktop's placement, or the fixed
    /// desktop's, or one full bar along the area.
    fn placement_now(&self) -> Placement {
        let mut placement = if self.own {
            crate::fixture::placement(
                self.scenario.as_deref(),
                (self.rect.width(), self.rect.height()),
            )
        } else {
            self.link.placement()
        };
        if placement.bars.is_empty() {
            placement.bars.push(BarPlace {
                display: 0,
                rect: self.rect,
                full: true,
            });
        }
        placement
    }

    /// Keep one bar per display, each where its display is.
    fn follow_placement(&mut self) {
        let placement = self.placement_now();
        if placement == self.placement {
            return;
        }
        let feed = self.source.feed.borrow().clone();
        let mut kept: Vec<Bar> = std::mem::take(&mut self.bars);
        for place in &placement.bars {
            let reuse = kept
                .iter()
                .position(|bar| bar.place.display == place.display && bar.place.full == place.full);
            let mut bar = if let Some(at) = reuse {
                kept.remove(at)
            } else {
                let chrome: Chrome = self.source.clone();
                let widget =
                    TopBarChrome::from_tree_with_local_user(&feed.tree, feed.local_user.clone());
                let widget = if place.full { widget } else { widget.compact() };
                Bar {
                    place: *place,
                    widget: widget.with_chrome(chrome),
                }
            };
            bar.place = *place;
            bar.widget.set_placement(Some(place.rect));
            self.bars.push(bar);
        }
        if !kept.is_empty() || self.bars.len() != self.placement.bars.len() {
            self.restructured = true;
        }
        self.placement = placement;
        self.dirty = true;
    }

    /// Whether the compact bar on `bar`'s display steps aside for a
    /// fullscreen window there. The full bar steps aside on its own.
    fn suppressed(&self, bar: &Bar) -> bool {
        !bar.place.full && self.placement.fullscreen.contains(&bar.place.display)
    }

    /// Bring the bar up to date with the desktop: its feed, its commands,
    /// where the bars go and what a bar asked to open.
    fn catch_up(&mut self) {
        self.take_feed();
        while let Some(command) = self.link.next_command() {
            self.take_command(&command);
        }
        self.follow_placement();
        self.open_asked_popover();
    }

    fn take_command(&mut self, command: &Command) {
        match command {
            Command::Focus => {
                if let Some(bar) = self.bars.iter_mut().find(|bar| bar.place.full) {
                    bar.widget.set_focus(true);
                    self.focus_asked = true;
                }
            }
            Command::ReleaseKeyboard => {
                self.focus_asked = false;
                for bar in &mut self.bars {
                    bar.widget.set_focus(false);
                }
            }
            Command::MoveWithWindow {
                shown,
                motion,
                source,
            } => {
                if let Some(bar) = self.bars.iter_mut().find(|bar| bar.place.full) {
                    bar.widget.move_with_window(*shown, *motion, *source);
                }
            }
            Command::ClosePopovers => {
                self.menu.close();
                self.profile.close();
                self.settle_popovers();
            }
        }
        self.dirty = true;
    }

    /// Open (or close) the menu or card a bar asked for, one at a time.
    fn open_asked_popover(&mut self) {
        let Some(popover) = self.source.popover.take() else {
            return;
        };
        self.open_popover(popover);
    }

    fn open_popover(&mut self, popover: Popover) {
        let feed = self.source.feed.borrow().clone();
        match popover {
            Popover::WorkspaceMenu => {
                self.profile.close();
                let anchor = self.source.anchor.get();
                let anchor = if anchor.is_zero_area() {
                    self.full_bar()
                        .map_or(Rect::ZERO, TopBarChrome::workspace_menu_rect)
                } else {
                    anchor
                };
                self.menu.toggle(&feed.workspace_menu, anchor, self.rect);
            }
            Popover::Profile => {
                self.menu.close();
                let anchor = self
                    .full_bar()
                    .map_or(Rect::ZERO, TopBarChrome::own_avatar_rect);
                self.profile.toggle(&feed.profile, anchor, self.rect);
            }
        }
        // Whatever else the desktop has open over the bar closes for it.
        if self.menu.is_open() || self.profile.is_open() {
            self.link.ask(Ask::Opened);
        }
        self.settle_popovers();
        self.dirty = true;
    }

    /// Carry out what the person chose in either popover: a rename is the
    /// bar's own, the rest the desktop's.
    fn settle_popovers(&mut self) {
        if let Some(choice) = self.menu.take_choice() {
            match choice {
                WorkspaceMenuChoice::Rename => {
                    if let Some(bar) = self.bars.iter_mut().find(|bar| bar.place.full) {
                        bar.widget.begin_workspace_rename();
                    }
                }
                choice => self.link.ask(Ask::WorkspaceMenu(choice)),
            }
            self.dirty = true;
        }
        if let Some(choice) = self.profile.take_choice() {
            self.link.ask(Ask::Profile(choice));
            self.dirty = true;
        }
    }

    fn popover_open(&self) -> bool {
        self.menu.is_open() || self.profile.is_open()
    }

    /// Say what is shown. On its own the surface answers the fixed desktop
    /// itself.
    fn hand_over(&mut self) {
        self.open_asked_popover();
        if self.own {
            let _ = self.link.take_asks();
        }
        self.link.show(self.shown_now());
    }

    fn shown_now(&self) -> Shown {
        let full = self.full_bar();
        let rect_of = |f: fn(&TopBarChrome) -> Rect| full.map_or(Rect::ZERO, f);
        let popover = if self.menu.is_open() {
            self.menu
                .children()
                .first()
                .map_or(Rect::ZERO, |menu| menu.get_rect())
        } else if self.profile.is_open() {
            self.profile.rect()
        } else {
            Rect::ZERO
        };
        let clock = full
            .and_then(|bar| {
                bar.test_rects()
                    .into_iter()
                    .filter(|(name, _)| name.starts_with("clock_overlay_"))
                    .map(|(_, rect)| rect)
                    .filter(|rect| !rect.is_zero_area())
                    .reduce(|a, b| a.union(b))
            })
            .unwrap_or(Rect::ZERO);
        let keyboard = self.bars.iter().any(|bar| bar.widget.is_focused());
        let asked = self.focus_asked && keyboard;
        Shown {
            bars: self
                .bars
                .iter()
                .filter(|bar| !self.suppressed(bar))
                .map(|bar| (bar.place.display, bar.widget.bar_rect(), bar.place.full))
                .collect(),
            workspace_name: rect_of(TopBarChrome::workspace_menu_rect),
            own_avatar: rect_of(TopBarChrome::own_avatar_rect),
            people: rect_of(TopBarChrome::collaborators_rect),
            tray: rect_of(TopBarChrome::tray_pill_rect),
            clock,
            popover,
            workspace_menu: self.workspace_menu_labels(),
            profile: self.profile.is_open(),
            modal: self.popover_open(),
            keyboard,
            holds_shown: self.popover_open()
                || asked
                || full.is_some_and(TopBarChrome::holds_itself_shown),
            captures_pointer: self
                .bars
                .iter()
                .any(|bar| bar.widget.wants_pointer_capture()),
            animating: self.needs_animation_frame(),
            deadline: self.next_wake_deadline(),
        }
    }

    /// The tray item named `item` (its key, id or title), and where the
    /// full bar draws it.
    fn tray_item(&self, item: &str) -> Result<(AppTrayItem, Rect), String> {
        let item = item.trim();
        let feed = self.source.feed.borrow();
        let found = feed
            .app_tray
            .iter()
            .find(|candidate| {
                candidate.key == item
                    || candidate.id == item
                    || candidate.title.eq_ignore_ascii_case(item)
            })
            .cloned()
            .ok_or_else(|| format!("no tray item `{item}` is on the bar"))?;
        let name = format!("topbar_tray_item_{}", found.id);
        let rect = self
            .full_bar()
            .and_then(|bar| {
                bar.test_rects()
                    .into_iter()
                    .find(|(target, _)| *target == name)
                    .map(|(_, rect)| rect)
            })
            .unwrap_or(Rect::ZERO);
        Ok((found, rect))
    }

    /// The workspace named `workspace` (its key, its name or its place
    /// counting from 1).
    fn workspace_key(&self, workspace: &str) -> Result<String, String> {
        let workspace = workspace.trim();
        let feed = self.source.feed.borrow();
        let workspaces = &feed.tree.workspaces;
        let by_place = workspace
            .parse::<usize>()
            .ok()
            .and_then(|place| place.checked_sub(1))
            .and_then(|index| workspaces.get(index));
        workspaces
            .iter()
            .find(|candidate| {
                candidate.id.0.to_string() == workspace
                    || candidate.name.eq_ignore_ascii_case(workspace)
            })
            .or(by_place)
            .map(|found| found.id.0.to_string())
            .ok_or_else(|| format!("no workspace `{workspace}` is on this desktop"))
    }

    fn answer(&mut self, effect: TopBarEffect) -> Result<(), String> {
        let snapshot = self.source.snapshot();
        let ask = match effect {
            TopBarEffect::SwitchWorkspace { workspace } => {
                Ask::WorkspaceMenu(WorkspaceMenuChoice::Switch(self.workspace_key(&workspace)?))
            }
            TopBarEffect::NextWorkspace => Ask::Chrome(ChromeIntent::SwitchWorkspace(1)),
            TopBarEffect::PreviousWorkspace => Ask::Chrome(ChromeIntent::SwitchWorkspace(-1)),
            TopBarEffect::ShowStrips => Ask::Chrome(ChromeIntent::ShowStrips),
            TopBarEffect::Overview => Ask::Chrome(ChromeIntent::ToggleOverview),
            TopBarEffect::QuickSettings => Ask::Chrome(ChromeIntent::ShowQuickSettings),
            TopBarEffect::Notifications => Ask::Notifications,
            TopBarEffect::People => Ask::Chrome(ChromeIntent::ToggleCollaborators),
            TopBarEffect::Activity => Ask::Chrome(ChromeIntent::ShowActivity(None)),
            TopBarEffect::Invite => {
                if snapshot.invite != tilo_desktop::chrome::Invite::Offered {
                    return Err("you may not invite anyone here".into());
                }
                Ask::Chrome(ChromeIntent::Invite)
            }
            TopBarEffect::Profile => {
                self.open_popover(Popover::Profile);
                self.hand_over();
                return Ok(());
            }
            TopBarEffect::WorkspaceMenu => {
                self.source.anchor.set(Rect::ZERO);
                self.open_popover(Popover::WorkspaceMenu);
                self.hand_over();
                return Ok(());
            }
            TopBarEffect::RenameWorkspace { name } => {
                let name = name.trim();
                if name.is_empty() {
                    return Err("a workspace needs a name".into());
                }
                Ask::Chrome(ChromeIntent::RenameWorkspace {
                    name: name.to_owned(),
                })
            }
            TopBarEffect::ActivateTrayItem { item } => {
                let (item, rect) = self.tray_item(&item)?;
                let at = rect.center();
                Ask::Chrome(ChromeIntent::AppTray {
                    key: item.key,
                    request: AppTrayRequest::Activate {
                        x: ui::num::i32_of_f64(at.x),
                        y: ui::num::i32_of_f64(at.y),
                    },
                })
            }
            TopBarEffect::PlayPause => Ask::Media(MediaCommand::PlayPause),
            TopBarEffect::NextTrack => Ask::Media(MediaCommand::Next),
            TopBarEffect::PreviousTrack => Ask::Media(MediaCommand::Previous),
            TopBarEffect::Continue => {
                if self.source.continuity_offer().is_none() {
                    return Err("no other device offers a tab to continue".into());
                }
                Ask::Chrome(ChromeIntent::AcceptContinuity)
            }
            TopBarEffect::Focus => {
                self.take_command(&Command::Focus);
                self.hand_over();
                return Ok(());
            }
        };
        self.link.ask(ask);
        self.hand_over();
        Ok(())
    }
}

impl Widget for TopBarSurface {
    fn name(&self) -> &'static str {
        "TopBar"
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.catch_up();
        self.restructured = false;
        self.bar_nodes = self
            .bars
            .iter_mut()
            .map(|bar| bar.widget.build_layout(taffy, theme, node_map))
            .collect();
        self.menu_node = self
            .menu
            .is_open()
            .then(|| self.menu.build_layout(taffy, theme, node_map));
        self.profile_node = self
            .profile
            .is_open()
            .then(|| self.profile.build_layout(taffy, theme, node_map));
        let children: Vec<NodeId> = self
            .bar_nodes
            .iter()
            .copied()
            .chain(self.menu_node)
            .chain(self.profile_node)
            .collect();
        let style = ui::taffy::Style {
            display: ui::taffy::Display::Flex,
            flex_direction: ui::taffy::FlexDirection::Column,
            size: ui::taffy::Size::from_percent(1.0, 1.0),
            ..ui::taffy::Style::default()
        };
        taffy.widget_node(style, &children, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let inner = self.rect.origin();
        for (bar, node) in self.bars.iter_mut().zip(self.bar_nodes.iter().copied()) {
            bar.widget.apply_layout(taffy, node, inner);
        }
        if let Some(node) = self.menu_node {
            self.menu.apply_layout(taffy, node, inner);
        }
        if let Some(node) = self.profile_node {
            self.profile.apply_layout(taffy, node, inner);
        }
        // An area that changed size moves the bars of a placement that
        // follows it.
        self.follow_placement();
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = self
            .bars
            .iter()
            .map(|bar| &bar.widget as &dyn Widget)
            .collect();
        if self.menu_node.is_some() && self.menu.is_open() {
            children.push(&self.menu);
        }
        if self.profile_node.is_some() && self.profile.is_open() {
            children.push(&self.profile);
        }
        children
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let menu = self.menu_node.is_some() && self.menu.is_open();
        let profile = self.profile_node.is_some() && self.profile.is_open();
        let mut children: Vec<&mut (dyn Widget + '_)> = self
            .bars
            .iter_mut()
            .map(|bar| &mut bar.widget as &mut (dyn Widget + '_))
            .collect();
        if menu {
            children.push(&mut self.menu);
        }
        if profile {
            children.push(&mut self.profile);
        }
        children
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.catch_up();
        for bar in &mut self.bars {
            bar.widget.sync(state, path);
        }
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.catch_up();
        let mut changed = std::mem::take(&mut self.dirty);
        for bar in &mut self.bars {
            changed |= bar.widget.update(state, path);
        }
        self.hand_over();
        changed
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.catch_up();
        let suppressed: Vec<bool> = self.bars.iter().map(|bar| self.suppressed(bar)).collect();
        for (bar, suppressed) in self.bars.iter_mut().zip(suppressed) {
            if !suppressed {
                bar.widget
                    .draw(scene, transform, text_renderer, theme, presence);
            }
        }
        if self.menu.is_open() {
            self.menu
                .draw_overlay(scene, transform, text_renderer, theme, presence);
        }
        if self.profile.is_open() {
            self.profile
                .draw_overlay(scene, transform, text_renderer, theme, presence);
        }
        self.dirty = false;
        if !self.rect.is_zero_area()
            && let Some(popover) = self.staged_popover.take()
        {
            self.open_popover(popover);
        }
        self.hand_over();
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.catch_up();
        // A press while a menu or card is open is its own: it chooses, or
        // it dismisses without also reaching what is under it (the control
        // that opened it would open it again).
        let pressed = self.popover_open()
            && matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                }
            );
        if self.menu.is_open()
            && let Some(node) = self.menu_node
        {
            self.menu
                .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            self.settle_popovers();
            if *ctx.handled {
                self.hand_over();
                return;
            }
        }
        if self.profile.is_open()
            && let Some(node) = self.profile_node
        {
            self.profile
                .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            let modal = self.profile.modal_capture().is_some();
            self.settle_popovers();
            if *ctx.handled || modal {
                *ctx.handled = true;
                self.hand_over();
                return;
            }
        }
        if pressed {
            self.menu.close();
            self.profile.close();
            self.settle_popovers();
            *ctx.handled = true;
            self.dirty = true;
            self.hand_over();
            return;
        }
        let hovered = |bars: &[Bar]| -> Vec<Option<String>> {
            bars.iter()
                .map(|bar| bar.widget.hovered_control())
                .collect()
        };
        let hovered_before = hovered(&self.bars);
        let suppressed: Vec<bool> = self.bars.iter().map(|bar| self.suppressed(bar)).collect();
        for ((bar, node), suppressed) in self
            .bars
            .iter_mut()
            .zip(self.bar_nodes.iter().copied())
            .zip(suppressed)
        {
            if suppressed {
                continue;
            }
            bar.widget
                .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            if *ctx.handled {
                break;
            }
        }
        self.dirty = true;
        if hovered(&self.bars) != hovered_before {
            // A control lifting or coming to rest is a repaint, not input
            // taken: the app frames only on what it took, so ask for one.
            self.link.wake();
        }
        self.hand_over();
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.restructured
            || self.link.has_commands()
            || self.link.generation() != self.source.generation.get()
            || self.source.popover.get().is_some()
            || self.staged_popover.is_some()
            || self.bars.iter().any(|bar| bar.widget.needs_redraw())
            || self.menu.needs_redraw()
            || self.profile.needs_redraw()
    }

    fn needs_animation_frame(&self) -> bool {
        self.bars
            .iter()
            .any(|bar| bar.widget.needs_animation_frame())
            || self.menu.needs_animation_frame()
            || self.profile.needs_animation_frame()
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.bars
            .iter()
            .filter_map(|bar| bar.widget.next_wake_deadline())
            .min()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.bars
            .iter()
            .any(|bar| bar.widget.wants_pointer_capture())
    }

    fn cursor_shape(&self, point: Point) -> Option<ui::winit::window::CursorIcon> {
        self.bars
            .iter()
            .find_map(|bar| bar.widget.cursor_shape(point))
    }

    fn is_layout_dirty(&self) -> bool {
        self.restructured
            || self.menu.is_open() != self.menu_node.is_some()
            || self.profile.is_open() != self.profile_node.is_some()
            || self.bars.iter().any(|bar| bar.widget.is_layout_dirty())
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.catch_up();
        let mut dirty = self.restructured
            || self.menu.is_open() != self.menu_node.is_some()
            || self.profile.is_open() != self.profile_node.is_some();
        for bar in &mut self.bars {
            dirty |= bar.widget.take_structure_dirty();
        }
        dirty
    }

    fn test_rects(&self) -> Vec<(String, Rect)> {
        let mut rects = Vec::new();
        for bar in &self.bars {
            if self.suppressed(bar) {
                continue;
            }
            let prefix = if bar.place.full {
                String::new()
            } else {
                format!("display:{}:", bar.place.display)
            };
            rects.push((format!("{prefix}topbar"), bar.widget.bar_rect()));
            if bar.place.full {
                rects.extend(bar.widget.test_rects());
            }
        }
        let shown = self.shown_now();
        if !shown.popover.is_zero_area() {
            let name = if self.menu.is_open() {
                "workspace_menu"
            } else {
                "profile_card"
            };
            rects.push((name.to_owned(), shown.popover));
        }
        rects.retain(|(_, rect)| !rect.is_zero_area());
        rects
    }
}

impl ResourceSurface<TopBar, TopBarEffect> for TopBarSurface {
    fn present(&mut self, _state: &Rc<TopBar>) {}

    fn attach(&mut self, _cx: SurfaceCx<TopBar>) {}

    fn stage(&mut self, scenario: Option<&str>) {
        if self.own {
            self.stage_scenario(scenario);
            self.catch_up();
            match scenario {
                Some("menu") => self.staged_popover = Some(Popover::WorkspaceMenu),
                Some("profile") => self.staged_popover = Some(Popover::Profile),
                Some("focused") => self.take_command(&Command::Focus),
                _ => {}
            }
        }
    }

    fn effect(&mut self, effect: TopBarEffect, _cx: &mut EffectCx<TopBar, Self>) -> EffectAnswer {
        self.catch_up();
        self.answer(effect)?;
        self.dirty = true;
        Ok(Effected::done())
    }

    /// The workspaces on this desktop, each switched to through
    /// [`TopBarEffect::SwitchWorkspace`], and the running applications'
    /// tray items, each pressed through [`TopBarEffect::ActivateTrayItem`]:
    /// what an agent reads to find what to act on.
    fn live_rows(&self) -> Vec<LiveRow> {
        let feed = self.source.feed.borrow();
        let workspaces = feed.tree.workspaces.iter().map(|workspace| {
            let key = workspace.id.0.to_string();
            LiveRow::new(
                format!("desktop.workspace/{key}"),
                "desktop.workspace",
                workspace.name.clone(),
            )
            .effect(&TopBarEffect::SwitchWorkspace { workspace: key })
        });
        let tray = feed.app_tray.iter().map(|item| {
            LiveRow::new(
                format!("desktop.tray-item/{}", item.key),
                "desktop.tray-item",
                item.title.clone(),
            )
            .effect(&TopBarEffect::ActivateTrayItem {
                item: item.key.clone(),
            })
        });
        workspaces.chain(tray).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosted(scenario: &str) -> TopBarSurface {
        let link = Link::new();
        link.publish(crate::fixture::feed(Some(scenario)));
        let mut surface = TopBarSurface::new(Some(link));
        surface.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        surface.catch_up();
        surface
    }

    #[test]
    fn actions_reach_the_desktop_as_what_the_bar_asks() {
        let mut surface = hosted("bar");
        surface
            .answer(TopBarEffect::SwitchWorkspace {
                workspace: "home".into(),
            })
            .unwrap();
        surface.answer(TopBarEffect::QuickSettings).unwrap();
        surface.answer(TopBarEffect::Notifications).unwrap();
        surface.answer(TopBarEffect::PlayPause).unwrap();
        surface.answer(TopBarEffect::NextWorkspace).unwrap();
        let home = surface.workspace_key("Home").unwrap();
        assert_eq!(
            surface.link.take_asks(),
            [
                Ask::WorkspaceMenu(WorkspaceMenuChoice::Switch(home)),
                Ask::Chrome(ChromeIntent::ShowQuickSettings),
                Ask::Notifications,
                Ask::Media(MediaCommand::PlayPause),
                Ask::Chrome(ChromeIntent::SwitchWorkspace(1)),
            ]
        );
    }

    #[test]
    fn what_the_desktop_does_not_offer_is_refused() {
        let mut surface = hosted("bar");
        assert!(
            surface
                .answer(TopBarEffect::SwitchWorkspace {
                    workspace: "nowhere".into()
                })
                .is_err()
        );
        assert!(
            surface
                .answer(TopBarEffect::ActivateTrayItem {
                    item: "missing".into()
                })
                .is_err()
        );
        assert!(surface.answer(TopBarEffect::Continue).is_err());
        assert!(
            surface
                .answer(TopBarEffect::RenameWorkspace { name: " ".into() })
                .is_err()
        );
        assert!(surface.link.take_asks().is_empty());
    }

    #[test]
    fn the_menus_open_one_at_a_time_and_say_so() {
        let mut surface = hosted("bar");
        surface.answer(TopBarEffect::WorkspaceMenu).unwrap();
        assert!(surface.menu.is_open());
        assert_eq!(surface.link.take_asks(), [Ask::Opened]);
        surface.answer(TopBarEffect::Profile).unwrap();
        assert!(surface.profile.is_open());
        assert!(!surface.menu.is_open(), "one at a time");
        surface.link.command(Command::ClosePopovers);
        surface.catch_up();
        assert!(!surface.popover_open());
    }

    #[test]
    fn a_bar_asking_for_its_menu_opens_it_here() {
        let mut surface = hosted("bar");
        assert!(surface.source.intent(ChromeIntent::ToggleWorkspaceMenu));
        surface.catch_up();
        assert!(surface.menu.is_open());
        assert!(surface.source.intent(ChromeIntent::ToggleOverview));
        assert!(
            surface
                .link
                .take_asks()
                .contains(&Ask::Chrome(ChromeIntent::ToggleOverview))
        );
    }

    #[test]
    fn every_display_has_its_bar() {
        let link = Link::new();
        link.publish(crate::fixture::feed(Some("displays")));
        link.place(crate::fixture::placement(Some("displays"), (2560.0, 800.0)));
        let mut surface = TopBarSurface::new(Some(link.clone()));
        surface.catch_up();
        assert_eq!(surface.bars.len(), 2);
        assert_eq!(surface.bars.iter().filter(|bar| bar.place.full).count(), 1);
        link.place(Placement {
            fullscreen: vec![2],
            ..crate::fixture::placement(Some("displays"), (2560.0, 800.0))
        });
        surface.catch_up();
        surface.hand_over();
        assert_eq!(
            link.shown().bars.len(),
            1,
            "the fullscreen display's bar steps aside"
        );
    }
}
