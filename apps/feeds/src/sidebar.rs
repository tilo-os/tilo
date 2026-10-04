//! The sidebar: the reader's places in the shared [`NavList`].
//!
//! Smart lists first (all, unread, starred), then the folders with their
//! feeds nested under them and the feeds outside any folder, then the tags.
//! Counts are unread articles (articles for a tag), quiet and tabular; all
//! articles carries none, since Unread counts the same articles. A
//! feed's actions open from its row's More button or a right click, in a
//! menu that marks the row it acts on.

use std::cell::RefCell;
use std::rc::Rc;

use ui::icons::Icon;
use ui::widget::{AppModel, ModelPath, NavAction, NavItem, NavList, NavSection, Widget};
use vello::kurbo::{Affine, Point};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::app::{Core, Request, Requests, Shared};
use crate::model::{FeedKind, Filter, Id, TagKey};

/// The sidebar item id of a place.
pub(crate) fn nav_id(filter: Filter) -> String {
    match filter {
        Filter::All => "smart:all".into(),
        Filter::Unread => "smart:unread".into(),
        Filter::Starred => "smart:starred".into(),
        Filter::Folder(id) => format!("folder:{id}"),
        Filter::Feed(id) => format!("feed:{id}"),
        Filter::Tag(key) => format!("tag:{key}"),
    }
}

/// The place a sidebar item id names.
pub(crate) fn filter_of(id: &str) -> Option<Filter> {
    match id {
        "smart:all" => Some(Filter::All),
        "smart:unread" => Some(Filter::Unread),
        "smart:starred" => Some(Filter::Starred),
        _ => {
            if let Some(raw) = id.strip_prefix("folder:") {
                raw.parse::<Id>().ok().map(Filter::Folder)
            } else if let Some(raw) = id.strip_prefix("feed:") {
                raw.parse::<Id>().ok().map(Filter::Feed)
            } else {
                id.strip_prefix("tag:")
                    .and_then(|raw| raw.parse::<TagKey>().ok())
                    .map(Filter::Tag)
            }
        }
    }
}

/// A row's count, shown only when there is something to count.
trait Counted {
    fn counted(self, n: usize) -> Self;
}

impl Counted for NavItem {
    fn counted(self, n: usize) -> Self {
        if n == 0 {
            self
        } else {
            self.count(u32::try_from(n).unwrap_or(u32::MAX))
        }
    }
}

fn feed_icon(kind: FeedKind) -> Icon {
    match kind {
        // A Gemini capsule is a page of gemtext.
        FeedKind::Gemini => Icon::FileText,
        _ => Icon::Globe,
    }
}

impl Core {
    /// The sidebar's sections, from the library.
    pub(crate) fn nav_sections(&self) -> Vec<NavSection> {
        let store = &self.store;
        let smart = vec![
            // Unread, below, already counts what is new everywhere.
            NavItem::new(nav_id(Filter::All), "All articles")
                .icon(Icon::Layers)
                .without_actions(),
            NavItem::new(nav_id(Filter::Unread), "Unread")
                .icon(Icon::Inbox)
                .counted(store.total_unread())
                .without_actions(),
            NavItem::new(nav_id(Filter::Starred), "Starred")
                .icon(Icon::Star)
                .counted(store.total_starred())
                .without_actions(),
        ];
        let mut sections = vec![NavSection::untitled("library", smart)];

        let feed_item = |id: Id, title: &str, kind: FeedKind| {
            NavItem::new(nav_id(Filter::Feed(id)), title)
                .icon(feed_icon(kind))
                .counted(store.unread_for_feed(id))
        };
        let mut feeds: Vec<NavItem> = store
            .folders_sorted()
            .into_iter()
            .map(|folder| {
                let children = store
                    .feeds_in_folder(Some(folder.id))
                    .into_iter()
                    .map(|f| feed_item(f.id, &f.title, f.kind))
                    .collect();
                let item = NavItem::new(nav_id(Filter::Folder(folder.id)), folder.name.clone())
                    .icon(Icon::Folder)
                    .counted(store.unread_for_folder(folder.id))
                    .children(children)
                    .without_actions();
                if folder.collapsed {
                    item
                } else {
                    item.expanded()
                }
            })
            .collect();
        feeds.extend(
            store
                .feeds_in_folder(None)
                .into_iter()
                .map(|f| feed_item(f.id, &f.title, f.kind)),
        );
        if !feeds.is_empty() {
            sections.push(NavSection::new("feeds", "Feeds", feeds));
        }

        let tags: Vec<NavItem> = store
            .tags_limited(crate::actions::MAX_CONTEXT_TAG_RESOURCES)
            .into_iter()
            .map(|tag| {
                let key = crate::model::tag_key(&tag);
                NavItem::new(nav_id(Filter::Tag(key)), tag)
                    .counted(store.count_for(Filter::Tag(key)))
                    .without_actions()
            })
            .collect();
        if !tags.is_empty() {
            sections.push(NavSection::new("tags", "Tags", tags));
        }
        sections
    }
}

/// How far a list row's lift sits inside the row in `theme`.
fn lift_inset(theme: &ui::theme::Theme) -> (f64, f64) {
    (
        f64::from(theme.tokens.spacing.s2),
        f64::from(theme.tokens.metrics.chrome_rule_px),
    )
}

/// The sidebar widget: the shared [`NavList`], kept in step with the library.
pub(crate) struct SidebarNav {
    list: NavList,
    core: Shared,
    requests: Requests,
    sections: Vec<NavSection>,
    selected: Option<String>,
    /// Feeds whose More button was pressed this event.
    more: Rc<RefCell<Vec<String>>>,
    pointer: Point,
    /// How far a row's lift sits inside its row: the list's inset across,
    /// a hairline down.
    lift_inset: (f64, f64),
    /// The list has had its first layout, so it knows its row metrics.
    laid_out: bool,
}

impl SidebarNav {
    pub(crate) fn new(core: &Shared, requests: &Requests) -> Self {
        let (sections, selected) = {
            let core = core.borrow();
            (core.nav_sections(), nav_id(core.filter))
        };
        let more: Rc<RefCell<Vec<String>>> = Rc::default();
        let list = {
            let requests = Rc::clone(requests);
            let more = Rc::clone(&more);
            NavList::new(sections.clone())
                .with_id("feeds.sidebar")
                .with_accessible_name("Subscriptions")
                .selected(&selected)
                .on_select(move |id| {
                    if let Some(filter) = filter_of(&id) {
                        requests.borrow_mut().push(Request::Show(filter));
                    }
                })
                .item_actions(
                    vec![NavAction::new("more", Icon::MoreHorizontal, "More")],
                    move |id, _| more.borrow_mut().push(id.to_string()),
                )
        };
        Self {
            list,
            core: Rc::clone(core),
            requests: Rc::clone(requests),
            sections,
            selected: Some(selected),
            more,
            pointer: Point::ZERO,
            lift_inset: (0.0, 0.0),
            laid_out: false,
        }
    }

    /// Follow the library and the chosen place. New sections wait for the
    /// list's first layout: rows it rebuilds before it has seen the theme
    /// keep the stock tree row height instead of the navigation row's.
    fn refresh(&mut self) {
        if !self.laid_out {
            return;
        }
        let Ok(core) = self.core.try_borrow() else {
            return;
        };
        let (sections, selected) = (core.nav_sections(), nav_id(core.filter));
        drop(core);
        if sections != self.sections {
            self.list.set_sections(sections.clone());
            self.sections = sections;
        }
        if self.selected.as_deref() != Some(selected.as_str())
            || self.list.selected_id().as_deref() != Some(selected.as_str())
        {
            self.list.set_selected(Some(&selected));
            self.selected = Some(selected);
        }
    }

    /// The lift of `id`'s row: where the list paints its hover and
    /// selection, so a menu marks the row with the same edges.
    fn lift_rect(&self, id: &str) -> Option<vello::kurbo::Rect> {
        let (across, down) = self.lift_inset;
        self.list
            .row_rect(id)
            .map(|row| row.inset((-across, -down)))
    }

    /// Open the menu of the feed whose row holds `id`: under the row, or at
    /// `at` for a right click.
    fn open_menu(&self, id: &str, at: Option<Point>) {
        let (Some(Filter::Feed(feed)), Some(row)) = (filter_of(id), self.lift_rect(id)) else {
            return;
        };
        self.requests
            .borrow_mut()
            .push(Request::FeedMenu(feed, row, at));
    }

    /// The feed row under the pointer.
    fn feed_at(&self, p: Point) -> Option<String> {
        let core = self.core.try_borrow().ok()?;
        core.store
            .feeds
            .iter()
            .map(|f| nav_id(Filter::Feed(f.id)))
            .find(|key| self.list.row_rect(key).is_some_and(|row| row.contains(p)))
    }

    fn record_first_feed_row(&self) {
        let Ok(mut core) = self.core.try_borrow_mut() else {
            return;
        };
        core.first_feed_row = core
            .store
            .feeds_in_folder(core.store.folders_sorted().first().map(|f| f.id))
            .first()
            .map(|f| f.id)
            .and_then(|id| {
                self.lift_rect(&nav_id(Filter::Feed(id)))
                    .map(|row| (id, row))
            });
    }
}

impl ui::widget::Decorator for SidebarNav {
    type Inner = NavList;

    fn inner(&self) -> &NavList {
        &self.list
    }

    fn inner_mut(&mut self) -> &mut NavList {
        &mut self.list
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.refresh();
        Widget::sync(&mut self.list, state, path);
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        self.refresh();
        Widget::update(&mut self.list, state, path)
    }

    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        Widget::apply_layout(&mut self.list, taffy, node, origin);
        self.laid_out = true;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.pointer = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Right,
            ..
        } = event
            && let Some(id) = self.feed_at(self.pointer)
        {
            self.open_menu(&id, Some(self.pointer));
            *ctx.handled = true;
            return;
        }
        Widget::handle_event(&mut self.list, event, ctx);
        let more = std::mem::take(&mut *self.more.borrow_mut());
        for id in more {
            self.open_menu(&id, None);
        }
    }

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: Affine,
        text: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.lift_inset = lift_inset(theme);
        Widget::draw(&mut self.list, scene, transform, text, theme, presence);
        self.record_first_feed_row();
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut vello::Scene,
        transform: Affine,
        text: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
        presence: &ui::presence::PresenceCtx,
        permissions: &ui::permissions::PermissionContext,
    ) {
        self.lift_inset = lift_inset(theme);
        Widget::draw_with_presence(
            &mut self.list,
            scene,
            transform,
            text,
            theme,
            presence,
            permissions,
        );
        self.record_first_feed_row();
    }

    fn name(&self) -> &'static str {
        "FeedsSidebar"
    }
}
