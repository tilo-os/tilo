//! The sidebar: the reader's places in the shared [`NavList`].
//!
//! Smart lists first (all, unread, starred), then the folders with their
//! feeds nested under them and the feeds outside any folder, then the tags.
//! Counts are unread articles (articles for a tag), quiet and tabular; all
//! articles carries none, since Unread counts the same articles. A
//! feed's actions open from its row's More button or a right click, in a
//! menu that marks the row it acts on.

use std::rc::Rc;

use ui::icons::Icon;
use ui::widget::{AppModel, ModelPath, NavAction, NavItem, NavList, NavSection, Widget};
use vello::kurbo::Point;

use crate::app::{Core, Request, Requests, Shared};
use crate::model::{FeedKind, Filter, Id, TagKey};

/// The sidebar item id of a place.
pub(crate) fn nav_id(filter: &Filter) -> String {
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
        _ => Icon::Rss,
    }
}

impl Core {
    /// The sidebar's sections, from the library.
    pub(crate) fn nav_sections(&self) -> Vec<NavSection> {
        let store = &self.store;
        let smart = vec![
            // Unread, below, already counts what is new everywhere.
            NavItem::new(nav_id(&Filter::All), "All articles")
                .icon(Icon::Layers)
                .without_actions(),
            NavItem::new(nav_id(&Filter::Unread), "Unread")
                .icon(Icon::Inbox)
                .counted(store.total_unread())
                .without_actions(),
            NavItem::new(nav_id(&Filter::Starred), "Starred")
                .icon(Icon::Star)
                .counted(store.total_starred())
                .without_actions(),
        ];
        let mut sections = vec![NavSection::untitled("library", smart)];

        let feed_item = |id: &Id, title: &str, kind: FeedKind| {
            NavItem::new(nav_id(&Filter::Feed(id.clone())), title)
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
                let item = NavItem::new(
                    nav_id(&Filter::Folder(folder.id.clone())),
                    folder.name.clone(),
                )
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
            .tags_limited(crate::resource::MAX_TAG_PLACES)
            .into_iter()
            .map(|tag| {
                let key = crate::model::tag_key(&tag);
                NavItem::new(nav_id(&Filter::Tag(key)), tag)
                    .icon(Icon::Tag)
                    .counted(store.count_for(&Filter::Tag(key)))
                    .without_actions()
            })
            .collect();
        if !tags.is_empty() {
            sections.push(NavSection::new("tags", "Tags", tags));
        }
        sections
    }
}

/// The sidebar widget: the shared [`NavList`], kept in step with the library.
pub(crate) struct SidebarNav {
    list: NavList,
    core: Shared,
    sections: Vec<NavSection>,
    selected: Option<String>,
}

impl SidebarNav {
    pub(crate) fn new(core: &Shared, requests: &Requests) -> Self {
        let (sections, selected) = {
            let core = core.borrow();
            (core.nav_sections(), nav_id(&core.filter))
        };
        let list = {
            let show = Rc::clone(requests);
            let menu = Rc::clone(requests);
            let fold = Rc::clone(requests);
            NavList::new(sections.clone())
                .with_id("feeds.sidebar")
                .with_accessible_name("Subscriptions")
                .selected(&selected)
                .on_select(move |id| {
                    if let Some(filter) = filter_of(&id) {
                        show.borrow_mut().push(Request::Show(filter));
                    }
                })
                .item_actions(
                    vec![NavAction::new("more", Icon::MoreHorizontal, "More").opens_context_menu()],
                    |_, _| {},
                )
                // A feed's actions open from its More button, a right click,
                // a long press or the Menu key, in a menu that marks its row.
                .on_context_menu(move |id, lift, at| {
                    if let Some(Filter::Feed(feed)) = filter_of(id) {
                        menu.borrow_mut()
                            .push(Request::FeedMenu(feed, lift, Some(at)));
                    }
                })
                // What the person folds stays folded, on every device.
                .on_expand(move |id, open| {
                    if let Some(Filter::Folder(folder)) = filter_of(id) {
                        fold.borrow_mut()
                            .push(Request::SetFolderCollapsed(folder, !open));
                    }
                })
        };
        Self {
            list,
            core: Rc::clone(core),
            sections,
            selected: Some(selected),
        }
    }

    /// Follow the library and the chosen place.
    fn refresh(&mut self) {
        let Ok(core) = self.core.try_borrow() else {
            return;
        };
        let (sections, selected) = (core.nav_sections(), nav_id(&core.filter));
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

    /// Note where the first feed's row is, for the context-menu capture.
    fn record_first_feed_row(&self) {
        let Ok(mut core) = self.core.try_borrow_mut() else {
            return;
        };
        core.first_feed_row = core
            .store
            .feeds_in_folder(core.store.folders_sorted().first().map(|f| f.id))
            .first()
            .map(|f| f.id.clone())
            .and_then(|id| {
                self.list
                    .row_lift_rect(&nav_id(&Filter::Feed(id.clone())))
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

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.refresh();
        Widget::sync(&mut self.list, state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.refresh();
        Widget::update(&mut self.list, state, path)
    }

    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        Widget::apply_layout(&mut self.list, taffy, node, origin);
        self.record_first_feed_row();
    }

    fn name(&self) -> &'static str {
        "FeedsSidebar"
    }
}
