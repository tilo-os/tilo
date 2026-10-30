//! The notification banners, hosted: the banners app
//! ([`tilo_notification_banners`]) mounted in the shell's own process over
//! the desktop, with the feed of the notifications the chrome keeps lent to
//! it.
//!
//! The shell hands the banners a copy of its live notifications and where
//! they go on the displays, routes them the pointer and the keys while they
//! hold the keyboard, and carries out what they ask through the chrome's
//! notification operations. What they show (their rects, keyboard focus,
//! their live region) comes back as plain data the shell's hit testing,
//! thing menu and assistive technology read.

use std::collections::HashMap;

use tilo_desktop::notifications::BannerFeed;
use tilo_notification_banners::{
    Ask, BannerService, Command, Link, OtherDisplay, Placement, Shown,
};
use ui::a11y::AccessNode;
use ui::taffy::prelude::TaffyMaxContent as _;
use ui::widget::AppWidget;
use vello::kurbo::Vec2;

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

/// The instance the banners are mounted as: no tab of any workspace.
const BANNERS_INSTANCE: u64 = u64::MAX - 7;

/// What the shell keeps of the banners it hosts.
#[derive(Default)]
pub(super) struct BannerHost {
    link: Link,
    app: Option<Box<dyn AppWidget>>,
    /// Mounting failed; the session goes on without banners.
    failed: bool,
    /// Where the banners' area starts in the desktop's coordinates.
    origin: Point,
    /// What the feed last published held, to publish only a change.
    published: Option<FeedKey>,
    /// What the banners showed at their last frame or input, in the
    /// desktop's coordinates.
    shown: Shown,
    /// The Notifications chord moved focus into the banners since their
    /// last frame said where it is.
    focus_sent: bool,
}

/// What of the feed the banners show: a change of any of it publishes the
/// feed again.
#[derive(Clone, Debug, PartialEq)]
struct FeedKey {
    locked: bool,
    held: bool,
    banners: Vec<BannerKey>,
}

#[derive(Clone, Debug, PartialEq)]
struct BannerKey {
    id: u64,
    title: String,
    body: String,
    dwell: ui::widget::toast::Dwell,
    actions: Vec<(String, String)>,
    show_preview: bool,
}

impl FeedKey {
    fn of(feed: &BannerFeed) -> Self {
        Self {
            locked: feed.locked,
            held: feed.held,
            banners: feed
                .banners
                .iter()
                .map(|banner| BannerKey {
                    id: banner.notification.id,
                    title: banner.notification.title.clone(),
                    body: banner.notification.body.clone(),
                    dwell: banner.notification.dwell,
                    actions: banner.actions.clone(),
                    show_preview: banner.notification.show_preview,
                })
                .collect(),
        }
    }
}

fn moved(rect: Rect, by: Vec2) -> Rect {
    if rect.is_zero_area() {
        rect
    } else {
        Rect::new(
            rect.x0 + by.x,
            rect.y0 + by.y,
            rect.x1 + by.x,
            rect.y1 + by.y,
        )
    }
}

/// What `shown` paints, shadows and antialiasing included, in the
/// desktop's coordinates.
fn painted_bounds(shown: &Shown, theme: &Theme) -> Vec<Rect> {
    let shadow = &theme.tokens.elevation.floating;
    let reach = f64::from(shadow.blur) * 2.0 + f64::from(shadow.offset_y).abs();
    let mut bounds = Vec::new();
    if !shown.stack.is_zero_area() {
        // A card slides in from up to its own width to the right.
        let rest = shown.stack;
        let travel = Rect::new(rest.x0, rest.y0, rest.x1 + rest.width(), rest.y1);
        bounds.push(travel.inflate(reach, reach));
    }
    bounds.extend(shown.machine.iter().map(|rect| rect.inflate(reach, reach)));
    bounds.extend(shown.call_edges.iter().map(|rect| rect.inflate(1.0, 1.0)));
    bounds
}

/// `node` and everything in it moved by `by`.
fn moved_node(mut node: AccessNode, by: Vec2) -> AccessNode {
    if let Some(bounds) = node.bounds.as_mut() {
        bounds.x0 += by.x;
        bounds.x1 += by.x;
        bounds.y0 += by.y;
        bounds.y1 += by.y;
    }
    node.children = node
        .children
        .into_iter()
        .map(|child| moved_node(child, by))
        .collect();
    node
}

impl BannerHost {
    /// Take what the banners show now, moved into the desktop's
    /// coordinates.
    fn refresh_shown(&mut self) {
        let by = self.origin.to_vec2();
        let mut shown = self.link.shown();
        shown.stack = moved(shown.stack, by);
        for (_, rect) in &mut shown.banners {
            *rect = moved(*rect, by);
        }
        for rect in shown.machine.iter_mut().chain(shown.call_edges.iter_mut()) {
            *rect = moved(*rect, by);
        }
        shown.accessibility = shown.accessibility.map(|node| moved_node(node, by));
        self.shown = shown;
    }

    /// Whether keyboard focus is in the banners.
    fn has_keyboard(&self) -> bool {
        self.focus_sent || self.shown.keyboard
    }

    /// Whether `target` is one of the banners' controls as assistive
    /// technology names them.
    fn publishes_target(&self, target: &str) -> bool {
        fn has(node: &AccessNode, target: &str) -> bool {
            node.action_target.as_deref() == Some(target)
                || node.children.iter().any(|child| has(child, target))
        }
        self.shown
            .accessibility
            .as_ref()
            .is_some_and(|node| has(node, target))
    }
}

impl ShellOverlayHost {
    /// Mount the banners the first time the chrome is there to feed them:
    /// among the session's in-process apps on the session's broker, so
    /// their actions publish, or on their own where the session has none.
    fn mount_banners(&mut self) {
        if self.banners.app.is_some() || self.banners.failed {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let link = self.banners.link.clone();
        let apps = chrome.borrow().hosted_inputs.in_process().cloned();
        let mounted = match apps {
            Some(apps) => {
                let room = ::launcher::appdir::PERSONAL_ROOM;
                let component = apps.borrow_mut().mount(
                    tilo_notification_banners::definition(),
                    room,
                    BANNERS_INSTANCE,
                    ui::record::RecordId::room_default(),
                    |context| context.lend(BannerService { link }),
                );
                component.and_then(|component| {
                    let collaboration = apps.borrow().collaboration(&component, room);
                    tilo_desktop::resolver::app_tile(component, collaboration, |_| {
                        crate::shell::wake_compositor();
                    })
                })
            }
            None => tilo_notification_banners::prepare(link, "desktop", "person").and_then(
                |component| {
                    tilo_desktop::resolver::app_tile(component, None, |_| {
                        crate::shell::wake_compositor();
                    })
                },
            ),
        };
        match mounted {
            Ok(app) => self.banners.app = Some(app),
            // The loop never waits on the disk for the record: the store's
            // thread opens it, and a later frame mounts again.
            Err(error) if error.is_opening() => {}
            Err(error) => {
                tracing::warn!(%error, "notification banners could not be mounted");
                self.banners.failed = true;
            }
        }
    }

    /// Hand the banners the chrome's live notifications, when they changed.
    /// Whether they had.
    fn publish_banner_feed(&mut self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let feed = BannerFeed::of(&ChromeNotifications::new(&chrome));
        let key = FeedKey::of(&feed);
        if self.banners.published.as_ref() == Some(&key) {
            return false;
        }
        self.banners.published = Some(key);
        self.banners.link.publish(feed);
        true
    }

    /// Hand the banners the chrome's live notifications, when they changed,
    /// and where they go.
    fn feed_banners(&mut self, theme: &Theme) {
        self.publish_banner_feed();
        let origin = self.rect.origin();
        self.banners.origin = origin;
        let local = |rect: Rect| {
            Rect::new(
                rect.x0 - origin.x,
                rect.y0 - origin.y,
                rect.x1 - origin.x,
                rect.y1 - origin.y,
            )
        };
        let area = self.overlay_area();
        let top = f64::from(self.chrome_top_y);
        // Banners share the transients' zone with the follow bar: below it
        // while it is up, and below any window bar they would cover.
        let follow = self.follow_banner.chip_rect();
        let others = if self.is_partitioned() {
            self.layer_outputs()
                .into_iter()
                .filter(|display| *display != area)
                .map(|display| OtherDisplay {
                    display: local(display),
                    viewport: local(Rect::new(
                        display.x0,
                        display.y0 + top,
                        display.x1,
                        display.y1,
                    )),
                })
                .collect()
        } else {
            Vec::new()
        };
        self.banners.link.place(Placement {
            viewport: local(Rect::new(area.x0, area.y0 + top, area.x1, area.y1)),
            clear_top: if follow.is_zero_area() {
                0.0
            } else {
                follow.y1 - origin.y
            },
            window_bars: self
                .chrome_bar_rects(theme)
                .into_iter()
                .map(local)
                .collect(),
            others,
        });
    }

    /// Paint the banners over the desktop: above its content, below modal
    /// overlays and the permission prompt, so a prompt always wins focus.
    pub(super) fn draw_banners(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.chrome.is_none() {
            return;
        }
        self.mount_banners();
        self.feed_banners(theme);
        let region = self.rect;
        let banners = &mut self.banners;
        let Some(mut app) = banners.app.take() else {
            return;
        };
        // One damage-tracked part bounded by what the banners say they
        // cover, so a banner arriving repaints its card, not the desktop.
        ui::scene_parts::part_drawn(
            scene,
            ui::scene_parts::PartKey::new("shell.notifications", ()),
            |scene| {
                place(app.as_mut(), region, theme);
                // Drawn apart and appended whole: the app's own part spans its
                // whole area, the desktop; this part claims only what the
                // banners cover.
                // The banners stand on the desktop, not on a ground of their
                // own: they hand out no scroll copies, which would need one.
                let mut drawn = Scene::new();
                {
                    let _no_copies = ui::scroll_copy::HostCopies::begin(false);
                    app.draw(&mut drawn, transform, text_renderer, theme, presence);
                }
                banners.refresh_shown();
                let bounds: Vec<Rect> = painted_bounds(&banners.shown, theme)
                    .into_iter()
                    .map(|rect| transform.transform_rect_bbox(rect))
                    .collect();
                // With nothing on screen the app paints nothing to see.
                if !bounds.is_empty() {
                    scene.append(&drawn, None);
                }
                bounds
            },
        );
        self.banners.app = Some(app);
        self.banners.focus_sent = false;
        self.answer_banners();
    }

    /// Hand the banners what arrived since they were last painted, run the
    /// work their wake or a published action brought, and carry out what
    /// it asked. Whether it changed what they show. The frame's update
    /// comes before its accessibility tree, so an arrival is announced in
    /// the frame it arrives in, not once it has been painted.
    pub(super) fn update_banners(&mut self, doc: &AppModel, path: &ModelPath) -> bool {
        if self.banners.app.is_none() {
            return false;
        }
        let published = self.publish_banner_feed();
        let Some(app) = self.banners.app.as_mut() else {
            return false;
        };
        let visible = app.update(doc, path);
        self.answer_banners();
        visible || published
    }

    /// Carry out what the banners asked, through the chrome's notification
    /// operations, and take what they show now.
    fn answer_banners(&mut self) {
        self.banners.refresh_shown();
        if self.banners.shown.keyboard {
            // Focus landed: from here what the banners show says where it is.
            self.banners.focus_sent = false;
        }
        let asks = self.banners.link.take_asks();
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        for ask in asks {
            match ask {
                Ask::Banner(ask) => ask.answer(&ChromeNotifications::new(&chrome)),
                Ask::Reply { id, text } => {
                    if !chrome.borrow_mut().reply_to_notification(id, &text) {
                        tracing::debug!(id, "inline reply refused");
                    }
                }
                Ask::Read(id) => {
                    if chrome.borrow_mut().read_notification(id) {
                        self.quick_settings.read(id);
                    }
                }
                Ask::AnswerCall(id) => {
                    chrome.borrow_mut().answer_call(id, true);
                }
                Ask::DeclineCall(id) => {
                    chrome.borrow_mut().answer_call(id, false);
                }
            }
        }
    }

    /// Route a pointer event to the banners, as a window under them never
    /// gets it. Whether they took it.
    pub(super) fn route_banner_pointer(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        // The pointer, and what ends a press on a banner (the window losing
        // focus, Escape while a press is held). Other keys reach the banners
        // as commands while they hold the keyboard, and never otherwise.
        let pointer = matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::CursorLeft { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::Touch(_)
                | WindowEvent::Focused(_)
        );
        let ends_press = self.banners.shown.captures_pointer
            && matches!(
                event,
                WindowEvent::KeyboardInput { event, .. }
                    if event.logical_key == Key::Named(NamedKey::Escape)
            );
        if !pointer && !ends_press {
            return false;
        }
        // A wheel turned elsewhere is not the banners' (Ctrl and the wheel
        // would zoom them from anywhere): only over what they draw, or while
        // a press begun on them holds the pointer.
        if matches!(event, WindowEvent::MouseWheel { .. })
            && !self.banners.shown.captures_pointer
            && !self.last_cursor.is_some_and(|p| self.banners_drawn_at(p))
        {
            return false;
        }
        let Some(app) = self.banners.app.as_mut() else {
            return false;
        };
        let before = std::mem::replace(ctx.handled, false);
        app.handle_event(event, ctx);
        let taken = *ctx.handled;
        *ctx.handled = before || taken;
        self.answer_banners();
        taken
    }

    /// Route `command` to the banners, and carry out what it asked.
    fn command_banners(&mut self, command: Command) {
        self.banners.link.command(command);
        self.answer_banners();
    }

    /// Something in front of the banners took a press begun on them.
    pub(super) fn cancel_banner_press(&mut self) {
        self.command_banners(Command::CancelPress);
    }

    /// Whether a press begun on a banner owns the next release.
    pub(super) fn banner_wants_pointer_capture(&self) -> bool {
        self.banners.shown.captures_pointer
    }

    /// Where the banners' stack was last drawn.
    pub(super) fn banner_stack_rect(&self) -> Rect {
        self.banners.shown.stack
    }

    /// Whether the banners draw anything at `p`: the stack, a machine-wide
    /// banner or a call's edge.
    fn banners_drawn_at(&self, p: Point) -> bool {
        let shown = &self.banners.shown;
        shown.stack.contains(p)
            || shown
                .machine
                .iter()
                .chain(shown.call_edges.iter())
                .any(|rect| rect.contains(p))
    }

    /// The notification whose banner is drawn at `p`.
    pub(super) fn banner_at(&self, p: Point) -> Option<u64> {
        self.banners
            .shown
            .banners
            .iter()
            .find(|(_, rect)| rect.contains(p))
            .map(|(id, _)| *id)
    }

    /// The pointer is at `p` (`None`: it left), also over a window under
    /// the banners: reaching for the stack holds them.
    pub(super) fn note_banner_pointer(&mut self, p: Option<Point>) {
        if self.banners.shown.stack.is_zero_area() && !self.banners.shown.animating {
            return;
        }
        let origin = self.banners.origin;
        let local = p.map(|p| Point::new(p.x - origin.x, p.y - origin.y));
        self.command_banners(Command::Pointer(local));
    }

    /// The banners' live region, announcing the newest banner.
    pub(super) fn banner_accessibility(&self) -> Option<AccessNode> {
        self.banners.shown.accessibility.clone()
    }

    /// Press the banners' control assistive technology names by `target`.
    /// Whether it is one of theirs.
    pub(super) fn perform_banner_target(&mut self, target: &str) -> bool {
        if !self.banners.publishes_target(target) {
            return false;
        }
        self.command_banners(Command::Activate(target.to_owned()));
        true
    }

    /// Whether a banner on screen is arriving, leaving or moving, or the
    /// banners have something new to show.
    pub(super) fn banners_need_frame(&self) -> bool {
        self.banners.shown.animating
            || self
                .banners
                .app
                .as_ref()
                .is_some_and(|app| app.needs_redraw())
    }

    /// When the banners next need to run with nothing waking them.
    pub(super) fn banners_deadline(&self) -> Option<Instant> {
        self.banners
            .app
            .as_ref()
            .and_then(|app| app.next_wake_deadline())
            .into_iter()
            .chain(self.banners.shown.deadline)
            .min()
    }

    /// The machine-wide banners the last paint repeated on the other
    /// displays.
    #[cfg(test)]
    pub(super) fn machine_banners(&self) -> &[Rect] {
        &self.banners.shown.machine
    }

    /// The displays the last paint edged for a ringing call.
    #[cfg(test)]
    pub(super) fn call_edges(&self) -> &[Rect] {
        &self.banners.shown.call_edges
    }

    /// The Notifications chord, or a key while the banners hold the
    /// keyboard: the chord moves focus into the newest banner; there the
    /// arrows, Tab, Return, Space and Escape act on the banners and any
    /// other key gives focus back to the window. Whether the key was the
    /// banners'.
    pub(super) fn handle_banner_keyboard(
        &mut self,
        key: &Key,
        action: Option<settings_store::hardware::ShortcutAction>,
        shift: bool,
        repeat: bool,
        handled: &mut bool,
    ) -> bool {
        use settings_store::hardware::ShortcutAction;
        let chord = action == Some(ShortcutAction::Notifications);
        if !self.banners.has_keyboard() {
            if chord && !repeat && self.banners.shown.focusable {
                self.banners.focus_sent = true;
                self.command_banners(Command::FocusNewest);
                *handled = true;
                return true;
            }
            return false;
        }
        if !chord && tilo_notification_banners::banner_key(key) {
            self.command_banners(Command::Key {
                key: key.clone(),
                shift,
            });
            *handled = true;
            return true;
        }
        self.release_banner_keyboard();
        false
    }

    /// Whether the banners hold keyboard focus and act on `key`, so the
    /// window behind them must not get it.
    pub fn banner_owns_key(&self, key: &Key) -> bool {
        self.banners.has_keyboard() && tilo_notification_banners::banner_key(key)
    }

    /// Whether keyboard focus is in the banners.
    pub fn banner_has_keyboard(&self) -> bool {
        self.banners.has_keyboard()
    }

    /// Give keyboard focus back from the banners to the window: a key they
    /// do not act on went there, or a press landed elsewhere.
    pub fn release_banner_keyboard(&mut self) {
        if self.banners.has_keyboard() {
            self.banners.focus_sent = false;
            self.command_banners(Command::ReleaseKeyboard);
        }
    }
}

/// Lay `app` out over `region`.
pub(super) fn place(app: &mut dyn AppWidget, region: Rect, theme: &Theme) {
    let mut taffy = ui::taffy::TaffyTree::<()>::new();
    let mut nodes = HashMap::new();
    let node = app.build_layout(&mut taffy, theme, &mut nodes);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a desktop is at most some thousand logical pixels across"
    )]
    let size = ui::taffy::Size {
        width: ui::taffy::Dimension::length(region.width() as f32),
        height: ui::taffy::Dimension::length(region.height() as f32),
    };
    let Ok(root) = taffy.new_with_children(
        ui::taffy::Style {
            size,
            ..ui::taffy::Style::default()
        },
        &[node],
    ) else {
        return;
    };
    if taffy
        .compute_layout(root, ui::taffy::Size::MAX_CONTENT)
        .is_ok()
    {
        app.apply_layout(&taffy, node, region.origin());
    }
}
