//! The Store's screens.
//!
//! ```text
//! [Discover | Updates | On this device | Bots]          (the window's filter field searches)
//! Discover        Updates (when there are any), Featured, Recently updated, Categories
//! a category      every app in it, Tilo first
//! Updates         one list: Update all, and Update on each app
//! On this device  the apps here, each opening from its row
//! Bots            the bots, never in an app grid
//! search          Apps, then Bots, then more from Flathub, answered on this device
//! an app's page   icon, name, developer and tag line; its one action; screenshots;
//!                 about; what's new; facts; what it may do; blocking; reporting
//! ```
//!
//! Every page is one column on the window ground, composed from the toolkit's
//! content widgets; the theme owns every colour, size and space, and every
//! string comes from the Store's fluent catalogue.

use ui::TextRole;
use ui::framework::TextControl;
use ui::icons::Icon;
use ui::text_render::TextKind;
use ui::widget::media_card::{MediaAspect, MediaCard, MediaSource};
use ui::widget::toggle_group::{Segment, ToggleGroup};
use ui::widget::{
    Actions, Badge, Banner, Button, ButtonRole, CardGrid, ChipStyle, Column, Container,
    ControlSize, EmptyState, Field, Form, FormSection, ListRow, Property, PropertyList, Row,
    ScrollAxis, Scrollable, Section, SectionHeader, Spinner, Text, TextInput, Widget,
};

use super::{
    StoreApp, Theme, View, action_code, format_size, full_publisher, remote_state_label,
    visible_operation_status,
};
use crate::bindings;
use crate::catalogue::{self, Blocker, Permission, Unblocked as _};
use crate::model::{ActionKind, StoreItem, StoreSource, StoreTrust};
use crate::resource::{ReportProjection, StoreEffect};
use crate::widgets::app::{self as app_widget, Primary};
use crate::widgets::visible_version;

/// How wide a page's content may grow, in lines of body text, so it widens
/// with the text scale like every other text-sized measure.
#[derive(Clone, Copy)]
enum Measure {
    /// Grids of cards and lists use the canvas.
    Catalogue,
    /// Prose, facts and forms stay at a readable measure.
    Reading,
}

impl Measure {
    fn width(self, theme: &Theme) -> f32 {
        let lines = match self {
            Self::Catalogue => 55.0,
            Self::Reading => 32.0,
        };
        (theme.text(TextRole::Body).line_height_px() * lines).round()
    }
}

type Element = Box<dyn Widget>;

/// How many cards or rows a long list shows before Show more.
pub(super) const PAGE: usize = 48;

/// The width of one screenshot in the gallery, in lines of body text.
const SCREENSHOT_LINES: f32 = 14.0;

impl StoreApp {
    /// Compose the whole tree. Called from `build_layout`; also used by tests
    /// to inspect the composition graph directly.
    pub(super) fn compose(&self, theme: &Theme) -> Element {
        if self.runtime_visible() {
            self.build_runtime_recovery(theme)
        } else if self.safety_open {
            self.build_safety_center(theme)
        } else if self.remotes_open {
            self.build_remotes(theme)
        } else if let Some(page) = self.build_detail(theme) {
            page
        } else if !self.query.trim().is_empty() {
            self.build_search(theme)
        } else {
            match &self.view {
                View::Home => self.build_home(theme),
                View::Category(category) => self.build_category(theme, category),
                View::Updates => self.build_updates(theme),
                View::Installed => self.build_installed(theme),
                View::Bots => self.build_bots(theme),
                View::Repository(remote) => self.build_repository(theme, remote),
            }
        }
    }

    /// A page: `children` stacked one group step apart in a bounded, centred
    /// column inside a vertical scroller. Catalogue views keep their scroll
    /// position when a refresh lands.
    fn page(
        &self,
        theme: &Theme,
        measure: Measure,
        keeps_scroll: bool,
        children: Vec<Element>,
    ) -> Element {
        let mut column = Column::new()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Group);
        for child in children {
            column = column.push(child);
        }
        let mut scroll = Scrollable::new(ScrollAxis::Vertical).fill_parent_height();
        if keeps_scroll {
            scroll = scroll.with_persisted_offset(self.scroll_offset.clone());
        }
        let content: Element = match measure {
            Measure::Catalogue => Box::new(column),
            Measure::Reading => Box::new(
                Container::new()
                    .with_no_padding()
                    .readable()
                    .with_child(Box::new(column)),
            ),
        };
        Box::new(
            scroll.with_child(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_centered_max_width(Measure::Catalogue.width(theme))
                    .with_child(content),
            )),
        )
    }

    /// A quiet paragraph of prose.
    fn prose(text: &str) -> Element {
        Box::new(Text::new(text).with_role(TextRole::Body).dimmed())
    }

    // ---- the catalogue's chrome ----

    /// The catalogue's actions in the window's header row: review a package
    /// file, the Flatpak remotes and trust. Search is the window's filter
    /// field; the catalogue refreshes itself.
    pub(super) fn catalogue_actions() -> Vec<Element> {
        let mut remotes = Button::icon_only(
            "store.remotes",
            Icon::Globe,
            &ui::i18n!("store-remotes-title"),
        );
        if let Some(binding) = bindings::effect(&StoreEffect::ShowRemotes) {
            remotes = remotes.on_action(binding);
        }
        let mut trust = Button::icon_only(
            "store.safety",
            Icon::Shield,
            &ui::i18n!("store-trust-title"),
        );
        if let Some(binding) = bindings::effect(&StoreEffect::ShowTrust) {
            trust = trust.on_action(binding);
        }
        let mut import = Button::icon_only(
            "store.package.import",
            Icon::Upload,
            &ui::i18n!("store-package-review"),
        );
        if let Some(binding) = bindings::effect(&StoreEffect::ImportPackage {
            path: None,
            content: None,
            reported_size: None,
        }) {
            import = import.on_action(binding);
        }
        vec![Box::new(import), Box::new(remotes), Box::new(trust)]
    }

    /// The four parts of the Store, as one segmented control at the top of
    /// every catalogue view. Updates carries how many there are.
    fn view_switcher(&self) -> Element {
        let updates = self.update_ids().len();
        let updates_label = if updates == 0 {
            ui::i18n!("store-view-updates")
        } else {
            ui::i18n!("store-view-updates-count", "count" => ui::format::count(updates as u64))
        };
        let selected = match &self.view {
            View::Home | View::Category(_) | View::Repository(_) => 0,
            View::Updates => 1,
            View::Installed => 2,
            View::Bots => 3,
        };
        let mut switcher = ToggleGroup::new(
            vec![
                Segment::new(ui::i18n!("store-view-home")),
                Segment::new(updates_label),
                Segment::new(ui::i18n!("store-view-installed")),
                Segment::new(ui::i18n!("store-view-bots")),
            ],
            selected,
        )
        .with_id("store.views")
        .with_accessible_name(ui::i18n!("store-views-label"));
        if let Some(binding) = bindings::view() {
            switcher = switcher.on_action(binding);
        }
        Box::new(switcher)
    }

    /// The top of every catalogue view: the switcher, then a running
    /// operation or a failure.
    fn catalogue_top(&self) -> Vec<Element> {
        let mut top = vec![self.view_switcher()];
        if !self.items.is_empty()
            && let Some(notice) = self.operation_notice()
        {
            top.push(notice);
        }
        top
    }

    /// Persistent operation feedback belongs in document flow: `Banner` is the
    /// shared alert, readable at large type and announced as a status.
    pub(super) fn operation_notice(&self) -> Option<Element> {
        if self.worker_busy && self.active_operation.is_none() && self.pending_jobs > 0 {
            let detail = if self.status_line.is_empty() {
                ui::i18n!("store-working")
            } else {
                visible_operation_status(&self.status_line)
            };
            return Some(Box::new(
                Row::new()
                    .no_wrap()
                    .with_rhythm(ui::tokens::Rhythm::Sibling)
                    .with_align_items(taffy::AlignItems::Center)
                    .push(Box::new(Spinner::new()))
                    .push(Box::new(
                        Container::new()
                            .with_no_padding()
                            .with_flex_grow()
                            .with_child(Box::new(
                                Banner::info(ui::i18n!("store-working-title")).with_message(detail),
                            )),
                    )),
            ));
        }
        let source_error = self
            .remotes
            .iter()
            .find_map(|remote| remote.catalogue_error.as_ref());
        self.operation_error.as_ref().or(source_error).map(|error| {
            let mut refresh = Button::new("store.error.refresh", &ui::i18n!("store-error-refresh"));
            if let Some(binding) = bindings::effect(&StoreEffect::Refresh) {
                refresh = refresh.on_action(binding);
            }
            Box::new(
                (if self.operation_error.is_some() {
                    Banner::error(ui::i18n!("store-operation-failed"))
                } else {
                    Banner::warning(ui::i18n!("store-source-partial"))
                })
                .with_message(visible_operation_status(error))
                .with_action(refresh),
            ) as Element
        })
    }

    // ---- an app in a list ----

    /// The one action an app offers from a list or its page.
    pub(super) fn primary(&self, item: &StoreItem) -> Primary {
        let action = item.action_kind(self.flatpak_available);
        let busy = self
            .active_operation
            .as_ref()
            .is_some_and(|(id, active)| id == &item.id && *active == action);
        let unavailable = item.unavailable_action(action).map(|(_, reason)| reason);
        let label = self.package_label(action);
        Primary {
            key: format!("store.app.{}.{}", item.id, action_code(action)),
            tooltip: unavailable.clone().unwrap_or_else(|| {
                ui::i18n!("store-action-tooltip", "action" => label.clone(), "name" => item.name.clone())
            }),
            label,
            binding: bindings::package(item, action),
            enabled: unavailable.is_none() && (self.active_operation.is_none() || busy),
            progress: busy.then(|| progress_label(action, &item.name)),
        }
    }

    /// An app as a card, opening its page.
    fn card(&self, item: &StoreItem) -> Element {
        app_widget::card(
            item,
            self.primary(item),
            bindings::show_details(item),
            self.images.as_ref(),
        )
    }

    /// An app as a row with `trailing` at its end, opening its page.
    fn row(&self, item: &StoreItem, trailing: Option<Element>, meta: Option<String>) -> Element {
        app_widget::row(
            item,
            trailing,
            meta,
            bindings::show_details(item),
            self.images.as_ref(),
        )
    }

    /// An app as a row whose end is its one action.
    fn action_row(&self, item: &StoreItem, meta: Option<String>) -> Element {
        let primary = self
            .primary(item)
            .control(ControlSize::Small, ButtonRole::Secondary);
        self.row(item, Some(primary), meta)
    }

    /// The items `ids` name, in that order, skipping any no longer listed.
    fn items_of<'a>(&'a self, ids: &'a [String]) -> impl Iterator<Item = &'a StoreItem> + 'a {
        ids.iter().filter_map(|id| self.item(id))
    }

    /// A grid of the apps `ids` names under `title`, or nothing without one.
    fn shelf(&self, title: &str, ids: &[String]) -> Option<Element> {
        let cards: Vec<Element> = self.items_of(ids).map(|item| self.card(item)).collect();
        (!cards.is_empty()).then(|| {
            Box::new(
                Section::new(vec![Box::new(CardGrid::new(cards))])
                    .with_header(SectionHeader::heading(title)),
            ) as Element
        })
    }

    /// Show more, when `total` is more than a list shows.
    fn show_more(&self, total: usize) -> Option<Element> {
        (total > self.shown).then(|| {
            let mut more =
                Button::new("store.more", &ui::i18n!("store-show-more")).role(ButtonRole::Plain);
            if let Some(binding) = bindings::effect(&StoreEffect::ShowMore) {
                more = more.on_action(binding);
            }
            Box::new(Actions::new(more)) as Element
        })
    }

    // ---- Discover ----

    /// Home: Updates when there are any, Tilo's apps and Flathub's popular
    /// ones, the recently updated, and the categories. Real content only; a
    /// shelf with nothing in it is left out.
    pub(super) fn build_home(&self, theme: &Theme) -> Element {
        let mut children = self.catalogue_top();
        let home = self
            .home
            .clone()
            .unwrap_or_else(|| catalogue::Home::compute(&self.items, &self.blocks));
        if home.is_empty() {
            children.push(self.empty_catalogue());
            return self.page(theme, Measure::Catalogue, true, children);
        }
        if !home.updates.is_empty() {
            let rows: Vec<Element> = self
                .items_of(&home.updates)
                .take(3)
                .map(|item| self.action_row(item, Some(update_meta(item))))
                .collect();
            let mut all = Button::new("store.updates.all", &ui::i18n!("store-see-all-updates"))
                .role(ButtonRole::Plain);
            if let Some(binding) = bindings::view_named("updates") {
                all = all.on_action(binding);
            }
            children.push(Box::new(
                Section::new(rows).with_header(
                    SectionHeader::heading(ui::i18n!("store-updates-title"))
                        .count(home.updates.len())
                        .action(all),
                ),
            ));
        }
        children.extend(self.shelf(&ui::i18n!("store-featured-title"), &home.featured));
        let featured: std::collections::HashSet<&str> =
            home.featured.iter().map(String::as_str).collect();
        let recent: Vec<String> = home
            .recent
            .iter()
            .filter(|id| !featured.contains(id.as_str()))
            .cloned()
            .collect();
        children.extend(self.shelf(&ui::i18n!("store-recent-title"), &recent));
        if !home.categories.is_empty() {
            let rows: Vec<Element> = home
                .categories
                .iter()
                .map(|(category, count)| {
                    let mut row = ListRow::new(
                        format!("store.category.{category}"),
                        catalogue::category_label(category),
                    )
                    .icon(ui::icons::app_glyph("", Some(category.as_str())))
                    .meta(ui::format::count(*count as u64))
                    .disclosure();
                    if let Some(binding) = bindings::category_named(category) {
                        row = row.on_action(binding);
                    }
                    Box::new(row) as Element
                })
                .collect();
            children.push(Box::new(Section::new(rows).with_header(
                SectionHeader::heading(ui::i18n!("store-categories-title")),
            )));
        }
        self.page(theme, Measure::Catalogue, true, children)
    }

    /// Every app in `category`, Tilo first.
    pub(super) fn build_category(&self, theme: &Theme, category: &str) -> Element {
        let mut children = self.catalogue_top();
        let ids = catalogue::in_category(&self.items, &self.blocks, category);
        let cards: Vec<Element> = self
            .items_of(&ids)
            .take(self.shown)
            .map(|item| self.card(item))
            .collect();
        if cards.is_empty() {
            children.push(Box::new(EmptyState::new(ui::i18n!("store-category-empty"))));
        } else {
            children.push(Box::new(
                Section::new(vec![Box::new(CardGrid::new(cards))]).with_header(
                    SectionHeader::heading(catalogue::category_label(category)).count(ids.len()),
                ),
            ));
            children.extend(self.show_more(ids.len()));
        }
        self.page(theme, Measure::Catalogue, true, children)
    }

    /// The apps of one configured Flatpak repository.
    fn build_repository(&self, theme: &Theme, remote: &str) -> Element {
        let mut children = self.catalogue_top();
        let ids = self.repository_ids(remote);
        if ids.is_empty() {
            children.push(Box::new(EmptyState::new(ui::i18n!("store-category-empty"))));
        } else {
            let cards: Vec<Element> = self
                .items_of(&ids)
                .take(self.shown)
                .map(|item| self.card(item))
                .collect();
            children.push(Box::new(
                Section::new(vec![Box::new(CardGrid::new(cards))])
                    .with_header(SectionHeader::heading(self.page_title()).count(ids.len())),
            ));
            children.extend(self.show_more(ids.len()));
        }
        self.page(theme, Measure::Catalogue, true, children)
    }

    /// The ids of the installed apps a newer version is offered for, in the
    /// order Updates first showed them.
    pub(super) fn update_ids(&self) -> Vec<String> {
        let fresh = catalogue::updates(&self.items, &self.blocks);
        match &self.home {
            Some(home) => catalogue::keep_order(&home.updates, &fresh),
            None => fresh,
        }
    }

    /// Updates: one list, Update all, and Update on each app.
    pub(super) fn build_updates(&self, theme: &Theme) -> Element {
        let mut children = self.catalogue_top();
        let ids = self.update_ids();
        if ids.is_empty() {
            children.push(Box::new(EmptyState::new(ui::i18n!("store-updates-none"))));
            return self.page(theme, Measure::Catalogue, true, children);
        }
        let rows: Vec<Element> = self
            .items_of(&ids)
            .map(|item| self.action_row(item, Some(update_meta(item))))
            .collect();
        let mut update = Button::new("store.update-all", &ui::i18n!("store-update-all"))
            .with_icon(Icon::Refresh)
            .with_enabled(self.active_operation.is_none());
        if let Some(binding) = bindings::effect(&StoreEffect::UpdateAll) {
            update = update.on_action(binding);
        }
        children.push(Box::new(
            Section::new(rows).with_header(
                SectionHeader::heading(ui::i18n!("store-updates-title"))
                    .count(ids.len())
                    .action(update),
            ),
        ));
        self.page(theme, Measure::Catalogue, true, children)
    }

    /// On this device: every app here, Tilo first, by name.
    pub(super) fn build_installed(&self, theme: &Theme) -> Element {
        let mut children = self.catalogue_top();
        let mut here: Vec<&StoreItem> = self
            .blocks
            .unblocked(&self.items)
            .filter(|item| catalogue::is_listed_app(item) && item.installed.is_installed())
            .collect();
        here.sort_by_key(|item| {
            (
                !matches!(item.source, StoreSource::Tilo { .. }),
                item.name.to_lowercase(),
            )
        });
        if here.is_empty() {
            children.push(Box::new(EmptyState::new(ui::i18n!("store-installed-none"))));
            return self.page(theme, Measure::Catalogue, true, children);
        }
        let rows: Vec<Element> = here
            .iter()
            .map(|item| self.action_row(item, Some(device_meta(item))))
            .collect();
        children.push(Box::new(Section::new(rows).with_header(
            SectionHeader::heading(ui::i18n!("store-view-installed")).count(here.len()),
        )));
        self.page(theme, Measure::Catalogue, true, children)
    }

    /// Bots: their own list, never an app grid.
    pub(super) fn build_bots(&self, theme: &Theme) -> Element {
        let mut children = self.catalogue_top();
        let ids = catalogue::bots(&self.items, &self.blocks);
        if ids.is_empty() {
            children.push(Box::new(EmptyState::new(ui::i18n!("store-bots-none"))));
            return self.page(theme, Measure::Catalogue, true, children);
        }
        let rows: Vec<Element> = self
            .items_of(&ids)
            .map(|item| self.row(item, None, Some(ui::i18n!("store-bot-kind"))))
            .collect();
        children.push(Box::new(Section::new(rows).with_header(
            SectionHeader::heading(ui::i18n!("store-view-bots")).count(ids.len()),
        )));
        self.page(theme, Measure::Catalogue, true, children)
    }

    /// Search inside the Store: apps, then bots, found on this device as the
    /// person types; then what Flathub has that the kept catalogue does not,
    /// in its own section so nothing above it moves when it arrives.
    pub(super) fn build_search(&self, theme: &Theme) -> Element {
        let mut children = Vec::new();
        if self.items.is_empty() && self.remote_results.is_empty() {
            // Nothing is loaded yet, or nothing could be: that is the
            // catalogue's state to say, not an empty search result.
            children.push(self.empty_catalogue());
            return self.page(theme, Measure::Catalogue, true, children);
        }
        if let Some(notice) = self.operation_notice() {
            children.push(notice);
        }
        let apps: Vec<Element> = self
            .items_of(&self.results.apps)
            .take(self.shown)
            .map(|item| self.action_row(item, None))
            .collect();
        let bots: Vec<Element> = self
            .items_of(&self.results.bots)
            .map(|item| self.row(item, None, Some(ui::i18n!("store-bot-kind"))))
            .collect();
        let more: Vec<Element> = self
            .remote_results
            .iter()
            .filter(|item| self.item(&item.id).is_none() && !self.blocks.is_blocked(&item.id))
            .map(|item| {
                let mut install = Button::new(
                    &format!("store.remote-app.{}", item.id),
                    &ui::i18n!("store-action-view"),
                )
                .control_size(ControlSize::Small);
                if let Some(binding) = bindings::show_details(item) {
                    install = install.on_action(binding);
                }
                self.row(item, Some(Box::new(install)), None)
            })
            .collect();
        if apps.is_empty() && bots.is_empty() && more.is_empty() {
            let mut empty = EmptyState::new(ui::i18n!("store-search-empty"))
                .with_message(ui::i18n!("store-search-empty-help"))
                .with_action(ui::i18n!("store-search-clear"))
                .with_action_id("store.empty.clear");
            if let Some(binding) = bindings::effect(&StoreEffect::ClearFilters) {
                empty = empty.on_action(binding);
            }
            children.push(Box::new(empty));
            return self.page(theme, Measure::Catalogue, true, children);
        }
        if !apps.is_empty() {
            children.push(Box::new(
                Section::new(apps).with_header(
                    SectionHeader::heading(ui::i18n!("store-search-apps"))
                        .count(self.results.apps.len()),
                ),
            ));
            children.extend(self.show_more(self.results.apps.len()));
        }
        if !bots.is_empty() {
            children.push(Box::new(Section::new(bots).with_header(
                SectionHeader::heading(ui::i18n!("store-view-bots")).count(self.results.bots.len()),
            )));
        }
        if !more.is_empty() {
            children.push(Box::new(Section::new(more).with_header(
                SectionHeader::heading(ui::i18n!("store-search-more-flathub")),
            )));
        }
        self.page(theme, Measure::Catalogue, true, children)
    }

    /// The catalogue has nothing to show: loading, offline or empty. One
    /// sentence and at most one action.
    fn empty_catalogue(&self) -> Element {
        if self.worker_busy && self.items.is_empty() {
            return Box::new(
                Row::new()
                    .with_rhythm(ui::tokens::Rhythm::Sibling)
                    .with_align_items(taffy::AlignItems::Center)
                    .push(Box::new(Spinner::new()))
                    .push(Box::new(
                        Text::new(&ui::i18n!("store-loading")).with_role(TextRole::Body),
                    )),
            );
        }
        let empty = if let Some(error) = self.operation_error.as_ref().or_else(|| {
            self.remotes
                .iter()
                .find_map(|remote| remote.catalogue_error.as_ref())
        }) {
            EmptyState::error(
                ui::i18n!("store-offline-title"),
                visible_operation_status(error),
            )
        } else {
            EmptyState::new(ui::i18n!("store-empty-title"))
                .with_message(ui::i18n!("store-empty-help"))
        }
        .with_action(ui::i18n!("store-error-refresh"))
        .with_action_id("store.empty.refresh");
        Box::new(match bindings::effect(&StoreEffect::Refresh) {
            Some(binding) => empty.on_action(binding),
            None => empty,
        })
    }

    // ---- an app's page ----

    /// The page of the focused app: who makes it and what it is, its one
    /// action, its screenshots, what it does and what changed, its facts and
    /// what it may do, and blocking and reporting it.
    pub(super) fn build_detail(&self, theme: &Theme) -> Option<Element> {
        let id = self.focused_detail.as_ref()?;
        let item = self
            .item(id)
            .or_else(|| self.remote_results.iter().find(|item| &item.id == id))?;
        let mut children: Vec<Element> = Vec::new();
        if let Some(notice) = self.operation_notice() {
            children.push(notice);
        }
        if let Some(blocked) = self.blocked_notice(item) {
            children.push(blocked);
        }
        children.push(self.detail_header(item));
        if let Some((app, root, version)) = &self.imported_review
            && app == &item.id
        {
            children.push(Box::new(Section::titled(
                ui::i18n!("store-package-reviewed-file"),
                vec![Box::new(PropertyList::new(vec![
                    Property::new(ui::i18n!("store-package-reviewed-version"), version),
                    Property::new(ui::i18n!("store-package-reviewed-root"), root),
                    Property::new(
                        ui::i18n!("store-package-reviewed-state"),
                        if item.installed.is_installed() {
                            ui::i18n!("store-package-reinstall")
                        } else {
                            ui::i18n!("store-package-new-install")
                        },
                    ),
                ]))],
            )));
        }
        let facts = self.page_facts(item);
        children.extend(self.gallery(item, &facts.screenshots, theme));
        if let Some(bot) = &item.bot {
            children.push(bot_does(bot));
            children.push(self.bot_access(bot));
        }
        if !facts.paragraphs.is_empty() {
            let paragraphs: Vec<Element> = facts
                .paragraphs
                .iter()
                .enumerate()
                .map(|(index, paragraph)| {
                    Box::new(
                        Text::new(paragraph)
                            .with_id(format!("store.detail.description.{index}"))
                            .with_role(TextRole::Body),
                    ) as Element
                })
                .collect();
            children.push(Box::new(Section::titled(
                ui::i18n!("store-about-app"),
                paragraphs,
            )));
        }
        if !facts.releases.is_empty() {
            children.push(whats_new(&facts.releases));
        }
        children.push(Box::new(Section::titled(
            ui::i18n!("store-facts-title"),
            vec![Box::new(PropertyList::new(self.fact_rows(item, &facts)))],
        )));
        if item.bot.is_none() {
            children.push(permissions_section(item, facts.permissions.as_deref()));
        }
        if let Some(notice) = self.local_only_notice(item) {
            children.push(Box::new(Section::titled(
                ui::i18n!("store-trust-section"),
                vec![Self::prose(&notice)],
            )));
        }
        children.push(self.technical_details(item));
        children.push(self.block_section(item));
        children.push(Self::report_section(item));
        Some(self.page(theme, Measure::Reading, false, children))
    }

    /// The top of the page: the app's icon, name, developer and tag line,
    /// then what can be done with it.
    fn detail_header(&self, item: &StoreItem) -> Element {
        let mut identity = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(Box::new(
                Text::new(&item.name)
                    .with_role(TextRole::LargeTitle)
                    .with_heading_level(1),
            ));
        let developer = self.developer(item);
        if !developer.is_empty() {
            identity = identity.push(Box::new(
                Text::new(&developer)
                    .with_id("store.detail.developer")
                    .with_role(TextRole::Caption)
                    .dimmed(),
            ));
        }
        let tagline = item.summary();
        if !tagline.is_empty() {
            identity = identity.push(Box::new(
                Text::new(tagline)
                    .with_id("store.detail.tagline")
                    .with_role(TextRole::Body),
            ));
        }
        let hero = Row::new()
            .no_wrap()
            .with_rhythm(ui::tokens::Rhythm::Section)
            .align_start()
            .push(Box::new(app_widget::icon(
                item,
                self.images.as_ref(),
                ui::widget::AvatarSize::Hero,
            )))
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_flex_grow()
                    .with_child(Box::new(identity)),
            ));
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_rhythm(ui::tokens::Rhythm::Section)
                .push(Box::new(hero))
                .push(self.detail_actions(item)),
        )
    }

    /// Who makes the app: its listing's developer, else its kept details'.
    pub(super) fn developer(&self, item: &StoreItem) -> String {
        if !item.listing.developer.is_empty() {
            return item.listing.developer.clone();
        }
        self.details
            .get(&item.id)
            .map(|details| details.developer.clone())
            .unwrap_or_default()
    }

    /// The page's actions: its one primary action (Open for a Tilo app, which
    /// downloads it when needed; Install for a Flathub app), with its progress
    /// inline while it runs; then keeping a Tilo app on this device, and
    /// removing an installed one.
    fn detail_actions(&self, item: &StoreItem) -> Element {
        if self.blocks.is_blocked(&item.id) {
            // A blocked app cannot be opened or installed from here; its
            // notice says who blocked it.
            return Box::new(Row::new());
        }
        let imported = self
            .imported_review
            .as_ref()
            .is_some_and(|(id, _, _)| id == &item.id);
        let mut primary = self.primary(item);
        if imported {
            primary.label = ui::i18n!("store-package-install");
            primary.binding = self.imported_review.as_ref().and_then(|(_, root, _)| {
                bindings::effect(&StoreEffect::InstallImportedPackage {
                    app: item.id.clone(),
                    root: root.clone(),
                })
            });
        }
        let mut row = Row::new()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .with_align_items(taffy::AlignItems::Center)
            .push(primary.control(ControlSize::Large, ButtonRole::Primary));
        if let Some(pin_action) = item.pin_action() {
            let label = self.package_label(pin_action);
            let mut button = Button::new(
                &format!("store.app.{}.{}", item.id, action_code(pin_action)),
                &label,
            )
            .control_size(ControlSize::Large)
            .tooltip(ui::i18n!("store-action-tooltip", "action" => label.clone(), "name" => item.name.clone()));
            if let Some(binding) = bindings::package(item, pin_action) {
                button = button.on_action(binding);
            }
            row = row.push(Box::new(button));
        }
        if let Some(remove_action) = item.remove_action() {
            let label = self.package_label(remove_action);
            let mut button = Button::new(&format!("store.app.{}.remove", item.id), &label)
                .role(ButtonRole::Destructive)
                .control_size(ControlSize::Large)
                .tooltip(ui::i18n!("store-action-tooltip", "action" => label.clone(), "name" => item.name.clone()));
            if let Some(binding) = bindings::package(item, remove_action) {
                button = button.on_action(binding);
            }
            row = row.push(Box::new(button));
        }
        Box::new(row)
    }

    /// What the page shows beyond the catalogue row, from the app's kept or
    /// fetched details and its listing.
    pub(super) fn page_facts(&self, item: &StoreItem) -> PageFacts {
        let details = self.details.get(&item.id);
        let paragraphs = details
            .map(|details| details.paragraphs.clone())
            .filter(|paragraphs| !paragraphs.is_empty())
            .unwrap_or_else(|| {
                let description = item.description.trim();
                if description.is_empty() || description == item.summary() {
                    Vec::new()
                } else {
                    description
                        .split("\n\n")
                        .map(|paragraph| paragraph.split_whitespace().collect::<Vec<_>>().join(" "))
                        .filter(|paragraph| !paragraph.is_empty())
                        .collect()
                }
            });
        let screenshots = details
            .map(|details| details.screenshots.clone())
            .filter(|shots| !shots.is_empty())
            .unwrap_or_else(|| item.screenshot_urls.clone());
        let releases = details
            .map(|details| details.releases.clone())
            .filter(|releases| !releases.is_empty())
            .unwrap_or_else(|| {
                // A Tilo listing names its versions and when they shipped.
                item.versions
                    .iter()
                    .filter(|version| version.released_at_unix > 0)
                    .take(3)
                    .map(|version| catalogue::Release {
                        version: visible_version_of(item, &version.version),
                        date_unix: version.released_at_unix,
                        notes: Vec::new(),
                    })
                    .collect()
            });
        let permissions = match &item.source {
            StoreSource::Tilo { .. } => (!item.listing.capabilities.is_empty())
                .then(|| catalogue::details::tilo_permissions(&item.listing.capabilities)),
            _ => details
                .filter(|details| details.permissions_known)
                .map(|details| details.permissions.clone()),
        };
        PageFacts {
            paragraphs,
            screenshots,
            releases,
            license: details
                .map(|details| details.license.clone())
                .filter(|license| !license.is_empty())
                .unwrap_or_else(|| item.listing.license.clone()),
            download_bytes: details
                .map(|details| details.download_bytes)
                .filter(|bytes| *bytes > 0)
                .unwrap_or_else(|| {
                    item.versions
                        .first()
                        .map_or(0, |version| version.size_bytes)
                }),
            installed_bytes: details.map_or(0, |details| details.installed_bytes),
            permissions,
            homepage: item
                .homepage_url
                .clone()
                .or_else(|| details.and_then(|details| details.homepage.clone())),
        }
    }

    /// The screenshots as one horizontal gallery, each loading from the disk
    /// cache or the network on its own.
    fn gallery(&self, item: &StoreItem, shots: &[String], theme: &Theme) -> Option<Element> {
        let store = self.images.as_ref()?;
        if shots.is_empty() {
            return None;
        }
        let width = (theme.text(TextRole::Body).line_height_px() * SCREENSHOT_LINES).round();
        let mut strip = Row::new()
            .no_wrap()
            .with_rhythm(ui::tokens::Rhythm::Related);
        for (index, url) in shots.iter().enumerate() {
            strip = strip.push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_width(width)
                    .with_child(Box::new(
                        MediaCard::new(
                            format!("store.screenshot.{}.{index}", item.id),
                            MediaSource::Url {
                                store: store.clone(),
                                url: url.clone(),
                            },
                        )
                        .aspect(MediaAspect::Wide)
                        .title(ui::i18n!(
                            "store-screenshot-label",
                            "name" => item.name.clone(),
                            "number" => (index + 1).to_string()
                        )),
                    )),
            ));
        }
        Some(Box::new(Section::titled(
            ui::i18n!("store-screenshots-title"),
            vec![Box::new(
                Scrollable::new(ScrollAxis::Horizontal)
                    .with_semantic_key(format!("store.gallery.{}", item.id))
                    .with_child(Box::new(strip)),
            )],
        )))
    }

    /// The facts: size, source, licence, developer, version and the last
    /// update, each its own row; a fact nobody published is left out.
    fn fact_rows(&self, item: &StoreItem, facts: &PageFacts) -> Vec<Property> {
        let mut rows = Vec::new();
        if facts.download_bytes > 0 {
            rows.push(
                Property::new(
                    ui::i18n!("store-fact-download"),
                    format_size(facts.download_bytes),
                )
                .with_kind(TextKind::Numeric),
            );
        }
        if facts.installed_bytes > 0 {
            rows.push(
                Property::new(
                    ui::i18n!("store-fact-installed-size"),
                    format_size(facts.installed_bytes),
                )
                .with_kind(TextKind::Numeric),
            );
        }
        rows.push(Property::new(
            ui::i18n!("store-fact-source"),
            match &item.source {
                StoreSource::Tilo { registry_name } => ui::i18n!(
                    "store-fact-source-tilo",
                    "registry" => registry_name.clone()
                ),
                StoreSource::Flathub => ui::i18n!("store-source-flathub"),
                StoreSource::FlatpakRemote { title, .. } => title.clone(),
            },
        ));
        if !facts.license.is_empty() {
            rows.push(
                Property::new(ui::i18n!("store-fact-license"), &facts.license)
                    .with_kind(TextKind::Identifier),
            );
        }
        let developer = self.developer(item);
        if !developer.is_empty() {
            rows.push(Property::new(ui::i18n!("store-fact-developer"), developer));
        } else if item.publisher_pubkey_hex.is_none() {
            rows.push(Property::new(
                ui::i18n!("store-fact-publisher"),
                full_publisher(item),
            ));
        }
        let version = visible_version(item);
        if !version.is_empty() {
            rows.push(
                Property::new(ui::i18n!("store-fact-version"), version)
                    .with_kind(TextKind::Numeric),
            );
        }
        if item.listing.updated_at_unix > 0 {
            rows.push(Property::new(
                ui::i18n!("store-fact-updated"),
                unix_date(item.listing.updated_at_unix),
            ));
        }
        if !item.categories.is_empty() {
            rows.push(Property::new(
                ui::i18n!("store-fact-categories"),
                item.categories
                    .iter()
                    .map(|category| catalogue::category_label(category))
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }
        if !item.supported_languages.is_empty() {
            rows.push(Property::new(
                ui::i18n!("store-supported-languages"),
                item.supported_languages.join(", "),
            ));
        }
        for (label, value) in device_properties(item) {
            rows.push(Property::new(label, value));
        }
        if let Some(url) = facts.homepage.clone() {
            let target = url.clone();
            rows.push(
                Property::new(ui::i18n!("store-fact-homepage"), url)
                    .with_kind(TextKind::Identifier)
                    .action(
                        Icon::ExternalLink,
                        ui::i18n!("store-open-homepage"),
                        move || {
                            ui::widget::open_external_url(&target);
                        },
                    ),
            );
        }
        if let Some(url) = item.source_url.clone() {
            let target = url.clone();
            rows.push(
                Property::new(ui::i18n!("store-fact-source-code"), url)
                    .with_kind(TextKind::Identifier)
                    .action(
                        Icon::ExternalLink,
                        ui::i18n!("store-open-source-code"),
                        move || {
                            ui::widget::open_external_url(&target);
                        },
                    ),
            );
        }
        rows
    }

    /// Who blocked the app, and for the person who did, the way back.
    fn blocked_notice(&self, item: &StoreItem) -> Option<Element> {
        let blockers = self.blocks.blockers(&item.id);
        // A policy's block is the one that matters: lifting the person's own
        // would not bring the app back.
        let blocker = blockers
            .iter()
            .find(|by| ***by != Blocker::Person)
            .or_else(|| blockers.first())?;
        let message = match blocker {
            Blocker::Person => ui::i18n!("store-blocked-by-you"),
            Blocker::Workspace { workspace } => {
                ui::i18n!("store-blocked-by-workspace", "workspace" => workspace.clone())
            }
            Blocker::Device { admin } if admin.is_empty() => ui::i18n!("store-blocked-by-policy"),
            Blocker::Device { admin } => {
                ui::i18n!("store-blocked-by-admin", "name" => admin.clone())
            }
        };
        let mut banner = Banner::warning(ui::i18n!("store-blocked-title")).with_message(message);
        if catalogue::person_can_unblock(&self.blocks, &item.id) {
            let mut unblock = Button::new(
                &format!("store.unblock.{}", item.id),
                &ui::i18n!("store-unblock"),
            );
            if let Some(binding) = bindings::unblock(item) {
                unblock = unblock.on_action(binding);
            }
            banner = banner.with_action(unblock);
        }
        Some(Box::new(banner))
    }

    /// Blocking the app: it then appears in no list and no search, here or in
    /// the desktop's search.
    fn block_section(&self, item: &StoreItem) -> Element {
        if self.blocks.is_blocked(&item.id) {
            return Box::new(Section::titled(
                ui::i18n!("store-block-title"),
                vec![Self::prose(&ui::i18n!("store-blocked-explanation"))],
            ));
        }
        let mut block = Button::new(
            &format!("store.block.{}", item.id),
            &ui::i18n!("store-block-button"),
        )
        .tooltip(ui::i18n!("store-block-tooltip", "name" => item.name.clone()));
        if let Some(binding) = bindings::block(item) {
            block = block.on_action(binding);
        }
        Box::new(Section::titled(
            ui::i18n!("store-block-title"),
            vec![
                Self::prose(&ui::i18n!("store-block-explanation")),
                Box::new(Actions::new(block)),
            ],
        ))
    }

    /// What a bot may reach: the apps whose actions it uses or whose changes
    /// it watches, the connectors it needs, and whether it posts under other
    /// names.
    fn bot_access(&self, bot: &crate::model::BotListing) -> Element {
        let mut rows: Vec<Element> = Vec::new();
        for app in &bot.apps {
            let name = self
                .item(app)
                .map_or_else(|| app.clone(), |item| item.name.clone());
            rows.push(Box::new(
                ListRow::new(format!("store.bot.app.{app}"), name)
                    .icon(ui::icons::app_glyph(app, None))
                    .secondary(ui::i18n!("store-bot-uses-app")),
            ));
        }
        for connector in &bot.connectors {
            let name = self
                .item(&connector.package_id)
                .map_or_else(|| connector.package_id.clone(), |item| item.name.clone());
            rows.push(Box::new(
                ListRow::new(
                    format!("store.bot.connector.{}", connector.package_id),
                    name,
                )
                .icon(Icon::Link)
                .secondary(ui::i18n!(
                    "store-bot-needs-connector",
                    "version" => connector.min_version.clone()
                )),
            ));
        }
        if bot.appearance {
            rows.push(Box::new(
                ListRow::new("store.bot.appearance", ui::i18n!("store-bot-appearance"))
                    .icon(Icon::Users),
            ));
        }
        if rows.is_empty() {
            rows.push(Self::prose(&ui::i18n!("store-bot-access-none")));
        }
        Box::new(Section::titled(ui::i18n!("store-bot-access-title"), rows))
    }

    /// For an installed app no repository describes: whether one may yet
    /// (it has not answered) or none lists it, said once on its page.
    pub(super) fn local_only_notice(&self, item: &StoreItem) -> Option<String> {
        let StoreSource::Tilo { registry_name } = &item.source else {
            return None;
        };
        if registry_name != crate::sources::INSTALLED_ON_DEVICE {
            return None;
        }
        Some(if self.unanswered_repositories.is_empty() {
            ui::i18n!("store-installed-unlisted")
        } else {
            ui::i18n::I18nManager::format(
                "store-installed-unanswered",
                &[(
                    "repositories",
                    ui::i18n::ArgValue::Text(self.unanswered_repositories.join(", ")),
                )],
            )
        })
    }

    fn technical_details(&self, item: &StoreItem) -> Element {
        let mut properties = technical_properties(item);
        properties.push(Property::new(
            ui::i18n!("store-trust-section"),
            trust_explanation(item),
        ));
        self.technical_properties_section(properties)
    }

    fn technical_properties_section(&self, properties: Vec<Property>) -> Element {
        let label = ui::i18n::I18nManager::lookup_plain(if self.technical_details_open {
            "store-hide-technical-details"
        } else {
            "store-show-technical-details"
        });
        let mut toggle = Button::new("store.detail.technical", &label).role(ButtonRole::Plain);
        if let Some(binding) = bindings::effect(&StoreEffect::SetTechnicalDetails {
            expanded: !self.technical_details_open,
        }) {
            toggle = toggle.on_action(binding);
        }
        let items = if self.technical_details_open {
            vec![Box::new(PropertyList::new(properties)) as Element]
        } else {
            Vec::new()
        };
        Box::new(
            Section::new(items).with_header(
                SectionHeader::heading(ui::i18n::I18nManager::lookup_plain(
                    "store-technical-details",
                ))
                .action(toggle),
            ),
        )
    }

    /// Reporting a problem with the app, or reading how decisions are made.
    fn report_section(item: &StoreItem) -> Element {
        let mut report = Button::new(
            &format!("store.report.create.{}", item.id),
            &ui::i18n!("store-report-create"),
        )
        .tooltip(ui::i18n!("store-report-create-tooltip"));
        if let Some(binding) = bindings::draft_report(item) {
            report = report.on_action(binding);
        }
        let mut moderation = Button::new("store.safety.detail", &ui::i18n!("store-report-how"))
            .role(ButtonRole::Plain);
        if let Some(binding) = bindings::effect(&StoreEffect::ShowTrust) {
            moderation = moderation.on_action(binding);
        }
        Box::new(Section::titled(
            ui::i18n!("store-report-title"),
            vec![
                Self::prose(&ui::i18n!("store-report-explanation")),
                Box::new(
                    Row::new()
                        .with_rhythm(ui::tokens::Rhythm::Related)
                        .push(Box::new(report))
                        .push(Box::new(moderation)),
                ),
            ],
        ))
    }

    // ---- component repair ----

    /// Optional payloads never download just because a Browser link was opened.
    fn build_runtime_recovery(&self, theme: &Theme) -> Element {
        use crate::runtime_recovery::Phase;
        let Some(recovery) = &self.runtime_recovery else {
            return self.build_home(theme);
        };
        let app_name = self
            .item(&recovery.target.app)
            .map_or(recovery.target.app.as_str(), |item| item.name.as_str());
        let mut children = vec![
            Self::prose(&ui::i18n!("store-runtime-explanation")),
            Box::new(PropertyList::new(vec![Property::new(
                ui::i18n!("store-runtime-consumer"),
                app_name,
            )])) as Element,
        ];
        match &recovery.phase {
            Phase::Inspecting | Phase::Acquiring(_) => {
                let key = if matches!(recovery.phase, Phase::Inspecting) {
                    "store-runtime-inspecting"
                } else {
                    "store-runtime-acquiring"
                };
                children.push(Box::new(
                    Row::new().push(Box::new(Spinner::new())).push(Box::new(
                        Banner::info(ui::i18n!(key))
                            .with_message(ui::i18n!("store-runtime-background")),
                    )),
                ));
                if matches!(recovery.phase, Phase::Acquiring(_)) {
                    children.push(Self::prose(&self.status_line));
                }
            }
            Phase::Review(review) => {
                if let Some(offer) = &review.offer {
                    children.push(Box::new(PropertyList::new(vec![
                        Property::new(ui::i18n!("store-runtime-source"), &offer.registry),
                        Property::new(
                            ui::i18n!("store-runtime-size"),
                            format_size(offer.size_bytes),
                        ),
                    ])));
                    children.push(Self::prose(&ui::i18n!("store-runtime-consent")));
                    if let Ok(review) = serde_json::to_string(review) {
                        let mut acquire = Button::new(
                            "store.runtime.acquire",
                            &ui::i18n!("store-runtime-download"),
                        )
                        .role(ButtonRole::Primary);
                        if let Some(binding) =
                            bindings::effect(&StoreEffect::AcquireRuntime { review })
                        {
                            acquire = acquire.on_action(binding);
                        }
                        children.push(Box::new(Actions::new(acquire)));
                    }
                    children.push(self.technical_properties_section(vec![
                        Property::new(ui::i18n!("store-runtime-repair-reference"), &review.runtime),
                        Property::new(ui::i18n!("store-runtime-package"), &offer.package_id),
                        Property::new(ui::i18n!("store-runtime-version"), &offer.version),
                        Property::new(ui::i18n!("store-runtime-publisher"), &offer.publisher),
                        Property::new(ui::i18n!("store-runtime-root"), &offer.root),
                        Property::new(ui::i18n!("store-runtime-hash"), &offer.archive_hash),
                    ]));
                }
            }
            Phase::Failed(error) => {
                let mut retry =
                    Button::new("store.runtime.retry", &ui::i18n!("store-runtime-retry"));
                if let Some(binding) = bindings::effect(&StoreEffect::RetryRuntime) {
                    retry = retry.on_action(binding);
                }
                children.push(Box::new(
                    Banner::error(ui::i18n!("store-runtime-failed"))
                        .with_message(error)
                        .with_action(retry),
                ));
            }
            Phase::Complete(_) => {
                children.push(Box::new(
                    Banner::info(ui::i18n!("store-runtime-complete"))
                        .with_message(ui::i18n!("store-runtime-reopen")),
                ));
            }
        }
        self.page(theme, Measure::Reading, false, children)
    }

    // ---- trust and moderation ----

    /// Reports the person has drafted, how decisions work, and what a
    /// signature does and does not promise.
    pub(super) fn build_safety_center(&self, theme: &Theme) -> Element {
        let mut children: Vec<Element> = Vec::new();
        children.extend(self.operation_notice());
        if self.reports.is_empty() {
            children.push(Box::new(
                EmptyState::new(ui::i18n!("store-reports-none"))
                    .with_message(ui::i18n!("store-reports-none-help")),
            ));
        }
        for report in &self.reports {
            children.push(self.report_form(report));
        }
        for (title, text) in [
            ("store-trust-reports-title", "store-trust-reports"),
            ("store-trust-signature-title", "store-trust-signature"),
            ("store-trust-guidelines-title", "store-trust-guidelines"),
        ] {
            children.push(Box::new(Section::titled(
                ui::i18n!(title),
                vec![Self::prose(&ui::i18n!(text))],
            )));
        }
        self.page(theme, Measure::Reading, false, children)
    }

    /// One report draft or moderation case: its status, the concern field
    /// and what can be done with it.
    fn report_form(&self, report: &ReportProjection) -> Element {
        let item = self.item(&report.app_id);
        let mut export = Button::new(
            &format!("{}.export", report.id),
            &ui::i18n!("store-report-export"),
        )
        .tooltip(ui::i18n!("store-report-export-tooltip"));
        if let Some(binding) =
            bindings::report(|app| StoreEffect::ExportEvidence { app }, &report.app_id)
        {
            export = export.on_action(binding);
        }
        let mut buttons: Vec<Button> = Vec::new();
        if self.reported_app(report).is_ok() {
            if report.is_case() {
                let case_actions = [
                    (
                        "refresh-status",
                        ui::i18n!("store-report-refresh"),
                        Some(StoreEffect::RefreshStatus {
                            app: report.app_id.clone(),
                        }),
                    ),
                    (
                        "appeal",
                        ui::i18n!("store-report-appeal"),
                        self.moderation_effect(&report.app_id, true),
                    ),
                ];
                for (action, label, effect) in case_actions {
                    let mut button = Button::new(&format!("{}.{action}", report.id), &label);
                    if let Some(binding) = effect.as_ref().and_then(bindings::effect) {
                        button = button.on_action(binding);
                    }
                    buttons.push(button);
                }
            } else {
                let mut submit = Button::new(
                    &format!("{}.submit", report.id),
                    &ui::i18n!("store-report-submit"),
                )
                .role(ButtonRole::Primary)
                .tooltip(ui::i18n!("store-report-submit-tooltip"));
                if let Some(binding) = self
                    .moderation_effect(&report.app_id, false)
                    .as_ref()
                    .and_then(bindings::effect)
                {
                    submit = submit.on_action(binding);
                }
                buttons.push(submit);
            }
        }
        let kind = if report.is_case() {
            ui::i18n!("store-report-case")
        } else {
            ui::i18n!("store-report-draft")
        };
        let name = item.map_or(report.app_id.as_str(), |item| item.name.as_str());
        let mut actions = Row::new()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .with_justify(taffy::JustifyContent::End)
            .push(Box::new(export));
        for button in buttons {
            actions = actions.push(Box::new(button));
        }
        let field = Field::new(
            ui::i18n!("store-report-note-label"),
            self.report_note_field(&report.app_id),
        )
        .with_help(ui::i18n!("store-report-note-help"));
        Box::new(Section::titled(
            name,
            vec![
                Box::new(
                    Row::new()
                        .no_wrap()
                        .with_rhythm(ui::tokens::Rhythm::Related)
                        .with_align_items(taffy::AlignItems::Center)
                        .push(Box::new(
                            Text::new(&kind).with_role(TextRole::Caption).dimmed(),
                        ))
                        .push(Box::new(
                            Badge::text(&report.status).style(ChipStyle::Neutral),
                        )),
                ),
                Box::new(field),
                Box::new(actions),
            ],
        ))
    }

    /// The note of `app_id`'s report: every keystroke one edit of the
    /// document's text, merging with a collaborator typing in it.
    fn report_note_field(&self, app_id: &str) -> NoteField {
        let input = TextInput::new(vec![])
            .with_id(&format!("store.report.note.{app_id}"))
            .multiline(true)
            .with_placeholder(&ui::i18n!("store-report-note-placeholder"));
        let path = crate::reports::Reports::note(app_id);
        NoteField(match &self.dispatcher {
            Some(dispatcher) => TextControl::at(input, dispatcher.clone(), path),
            None => TextControl::view_only_at(input, path),
        })
    }

    // ---- flatpak remotes ----

    /// Flatpak repositories: the form that adds one and the list of those
    /// configured on this machine.
    pub(super) fn build_remotes(&self, theme: &Theme) -> Element {
        let mut add = Button::new("store.remote.add", &ui::i18n!("store-remote-add"))
            .role(ButtonRole::Primary);
        if let Some(binding) = bindings::add_remote(&self.remote_name_state, &self.remote_url_state)
        {
            add = add.on_action(binding);
        }
        let mut refresh = Button::new("store.remote.refresh", &ui::i18n!("store-remote-refresh"));
        if let Some(binding) = bindings::effect(&StoreEffect::RefreshRemotes) {
            refresh = refresh.on_action(binding);
        }
        let form = Form::new()
            .section(
                FormSection::new(ui::i18n!("store-remote-add-title"))
                    .field(Field::new(
                        ui::i18n!("store-remote-name"),
                        TextInput::from_state(&self.remote_name_state)
                            .with_id("store.remote.name")
                            .with_kind(ui::widget::TextInputKind::Name)
                            .with_placeholder(&ui::i18n!("store-remote-name-placeholder")),
                    ))
                    .field(
                        Field::new(
                            ui::i18n!("store-remote-url"),
                            TextInput::from_state(&self.remote_url_state)
                                .with_id("store.remote.url")
                                .with_kind(ui::widget::TextInputKind::Url)
                                .with_placeholder("https://"),
                        )
                        .with_help(ui::i18n!("store-remote-url-help")),
                    ),
            )
            .with_actions(Actions::new(add).secondary(refresh));

        let rows: Vec<Element> = self
            .remotes
            .iter()
            .map(|remote| {
                let mut row =
                    ListRow::new(format!("store.{}", remote.resource_id()), &remote.title)
                        .icon(Icon::Globe)
                        .secondary(remote.catalogue_error.as_deref().unwrap_or(&remote.url))
                        .meta(remote_state_label(remote));
                if !remote.disabled
                    && let Some(binding) = bindings::effect(&StoreEffect::BrowseRemote {
                        remote: remote.resource_id(),
                    })
                {
                    row = row.disclosure().on_action(binding.clone()).action_binding(
                        Icon::Globe,
                        ui::i18n!("store-remote-browse"),
                        binding,
                    );
                }
                if remote.user_managed
                    && let Some(binding) = bindings::effect(&super::remote_enabled_effect(remote))
                {
                    row = row.trailing(Box::new(
                        Button::new(
                            &format!("store.{}.enabled", remote.resource_id()),
                            &ui::i18n!(if remote.disabled {
                                "store-remote-enable"
                            } else {
                                "store-remote-disable"
                            }),
                        )
                        .on_action(binding),
                    ));
                }
                if remote.user_managed
                    && let Some(binding) = bindings::remove_remote(remote)
                {
                    row =
                        row.action_binding(Icon::Trash, ui::i18n!("store-remote-remove"), binding);
                }
                Box::new(row) as Element
            })
            .collect();
        let mut children: Vec<Element> = Vec::new();
        children.extend(self.operation_notice());
        children.push(Self::prose(&ui::i18n!("store-remotes-description")));
        children.push(Box::new(form));
        if !rows.is_empty() {
            let count = rows.len();
            children.push(Box::new(Section::new(rows).with_header(
                SectionHeader::heading(ui::i18n!("store-remotes-configured")).count(count),
            )));
        }
        self.page(theme, Measure::Reading, false, children)
    }
}

/// What an app's page shows beyond its catalogue row.
#[derive(Clone, Debug, Default)]
pub(super) struct PageFacts {
    pub paragraphs: Vec<String>,
    pub screenshots: Vec<String>,
    pub releases: Vec<catalogue::Release>,
    pub license: String,
    pub download_bytes: u64,
    pub installed_bytes: u64,
    /// `None` when nobody said what the app may do.
    pub permissions: Option<Vec<Permission>>,
    pub homepage: Option<String>,
}

/// What a running action says in place of its button.
pub(super) fn progress_label(action: ActionKind, name: &str) -> String {
    let key = match action {
        ActionKind::Open | ActionKind::FlatpakOpen => "store-progress-opening",
        ActionKind::Remove | ActionKind::FlatpakRemove => "store-progress-removing",
        ActionKind::Update | ActionKind::FlatpakUpdate => "store-progress-updating",
        ActionKind::MarkInstalled => "store-progress-keeping",
        ActionKind::Unmark => "store-progress-releasing",
        ActionKind::FlatpakInstall | ActionKind::RuntimeInstall => "store-progress-installing",
        ActionKind::NeedsFlatpakHost => "store-progress-preparing",
        ActionKind::AddBot => "store-progress-adding",
    };
    ui::i18n::I18nManager::format(key, &[("name", ui::i18n::ArgValue::Text(name.to_owned()))])
}

/// An installed app's row meta in Updates: the version it moves to.
fn update_meta(item: &StoreItem) -> String {
    match &item.installed {
        crate::model::InstalledState::Installed { available, .. } if !available.is_empty() => {
            ui::i18n!("store-update-to", "version" => visible_version_of(item, available))
        }
        _ => catalogue::source_label(item),
    }
}

/// An installed app's row meta in On this device: whether it is kept.
fn device_meta(item: &StoreItem) -> String {
    match (&item.source, &item.installed) {
        (
            StoreSource::Tilo { .. },
            crate::model::InstalledState::Installed {
                pinned,
                user_managed,
                ..
            },
        ) => ui::i18n!(if !*user_managed {
            "store-device-system"
        } else if *pinned {
            "store-device-kept"
        } else {
            "store-device-cached"
        }),
        _ => catalogue::source_label(item),
    }
}

/// `version` as people read it: a native build's commit is left out.
fn visible_version_of(item: &StoreItem, version: &str) -> String {
    if matches!(item.source, StoreSource::Tilo { .. })
        && let Some((release, commit)) = version.split_once("+g")
        && commit.len() == 40
        && commit.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return release.to_owned();
    }
    version.to_owned()
}

/// A Unix-seconds day in the person's date format.
fn unix_date(unix: u64) -> String {
    ui::format::date_of(&ui::format::UnixSecs(i64::try_from(unix).unwrap_or(0)))
}

/// What changed in the newest releases: each its version and date, then its
/// notes.
fn whats_new(releases: &[catalogue::Release]) -> Element {
    let mut children: Vec<Element> = Vec::new();
    for (index, release) in releases.iter().enumerate() {
        let mut heading = Row::new()
            .no_wrap()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .with_align_items(taffy::AlignItems::Baseline)
            .push(Box::new(
                Text::new(
                    &ui::i18n!("store-release-version", "version" => release.version.clone()),
                )
                .with_id(format!("store.release.{index}"))
                .with_role(TextRole::Subheading),
            ));
        if release.date_unix > 0 {
            heading = heading.push(Box::new(
                Text::new(&unix_date(release.date_unix))
                    .with_role(TextRole::Caption)
                    .dimmed(),
            ));
        }
        let mut block = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Tight)
            .push(Box::new(heading));
        for note in &release.notes {
            block = block.push(Box::new(Text::new(note).with_role(TextRole::Body)));
        }
        children.push(Box::new(block));
    }
    Box::new(Section::titled(ui::i18n!("store-whats-new"), children))
}

/// What the app may do on this device, in plain words, the broad ones first.
fn permissions_section(item: &StoreItem, permissions: Option<&[Permission]>) -> Element {
    let rows: Vec<Element> = match permissions {
        Some([]) => vec![StoreApp::prose(&ui::i18n!("store-permissions-none"))],
        Some(permissions) => {
            let mut ordered: Vec<&Permission> = permissions.iter().collect();
            ordered.sort_by_key(|permission| !permission.is_broad());
            ordered
                .into_iter()
                .enumerate()
                .map(|(index, permission)| {
                    let mut row =
                        ListRow::new(format!("store.permission.{index}"), permission.label())
                            .icon(permission_icon(permission));
                    if permission.is_broad() {
                        row = row.warning();
                    }
                    Box::new(row) as Element
                })
                .collect()
        }
        None if matches!(item.source, StoreSource::Tilo { .. }) => {
            vec![StoreApp::prose(&ui::i18n!(
                "store-permissions-tilo-sandbox"
            ))]
        }
        None => vec![StoreApp::prose(&ui::i18n!("store-permissions-unknown"))],
    };
    Box::new(Section::titled(ui::i18n!("store-permissions-title"), rows))
}

fn permission_icon(permission: &Permission) -> Icon {
    match permission {
        Permission::LeavesSandbox | Permission::SystemServices => Icon::TriangleAlert,
        Permission::AllFiles | Permission::HomeFolder | Permission::Folder(_) => Icon::Folder,
        Permission::ChosenFiles => Icon::File,
        Permission::Internet => Icon::Globe,
        Permission::Camera => Icon::Camera,
        Permission::Microphone => Icon::Mic,
        Permission::Sound => Icon::Music,
        Permission::ScreenCapture => Icon::ScreenShare,
        Permission::Location => Icon::MapPin,
        Permission::AllDevices => Icon::HardDrive,
        Permission::Bluetooth => Icon::Bluetooth,
        Permission::LegacyDisplay => Icon::AppWindow,
        Permission::Secrets | Permission::SshKeys => Icon::Lock,
        Permission::Printing => Icon::Printer,
        Permission::Clipboard => Icon::ClipboardPaste,
        Permission::Notifications => Icon::Bell,
        Permission::RunsPrograms => Icon::SquareTerminal,
        Permission::OpensLinks => Icon::ExternalLink,
    }
}

/// What a directory bot does, as its signed listing says: when it runs.
fn bot_does(bot: &crate::model::BotListing) -> Element {
    let lines: Vec<Element> = bot
        .triggers
        .iter()
        .enumerate()
        .map(|(index, trigger)| {
            let key = match trigger.as_str() {
                "connector" => Some("store-bot-trigger-connector"),
                "record_changed" => Some("store-bot-trigger-record-changed"),
                "schedule" => Some("store-bot-trigger-schedule"),
                "manual" => Some("store-bot-trigger-manual"),
                "incoming_message" => Some("store-bot-trigger-incoming-message"),
                _ => None,
            };
            let text = key.map_or_else(|| trigger.clone(), ui::i18n::I18nManager::lookup_plain);
            Box::new(ListRow::new(format!("store.bot.trigger.{index}"), text).icon(Icon::Clock))
                as Element
        })
        .collect();
    Box::new(Section::titled(ui::i18n!("store-bot-what-it-does"), lines))
}

/// A report's note in a form field: the document-bound text control, named
/// by its field's visible label.
struct NoteField(TextControl<TextInput, crate::reports::Reports>);

impl ui::widget::Decorator for NoteField {
    type Inner = TextControl<TextInput, crate::reports::Reports>;

    fn inner(&self) -> &TextControl<TextInput, crate::reports::Reports> {
        &self.0
    }

    fn inner_mut(&mut self) -> &mut TextControl<TextInput, crate::reports::Reports> {
        &mut self.0
    }
}

impl ui::widget::FormControl for NoteField {
    fn with_form_accessible_name(mut self, name: &str) -> Self {
        let input = std::mem::replace(self.0.control_mut(), TextInput::new(vec![]));
        *self.0.control_mut() = input.with_accessible_name(name);
        self
    }
}

/// What a trust claim does and does not promise.
fn trust_explanation(item: &StoreItem) -> String {
    match item.trust {
        StoreTrust::TrustedAttestor => ui::i18n!("store-package-local-trust"),
        StoreTrust::PublisherSigned => ui::i18n!("store-trust-publisher-signed"),
        StoreTrust::Flathub => ui::i18n!("store-trust-flathub"),
        StoreTrust::ConfiguredRemote => ui::i18n!("store-remote-trust-explanation"),
    }
}

/// A digest as far as a person compares it: its start and its end.
fn digest_summary(hash: &str) -> String {
    let head: String = hash.chars().take(12).collect();
    let tail: String = hash
        .chars()
        .rev()
        .take(8)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("BLAKE3 {head}…{tail}")
}

/// Exact package identity remains inspectable without dominating the page.
fn technical_properties(item: &StoreItem) -> Vec<Property> {
    let mut rows = vec![
        Property::new(ui::i18n!("store-app-id"), &item.id)
            .with_kind(TextKind::Identifier)
            .copyable(),
    ];
    if !item.latest_version().is_empty() {
        rows.push(
            Property::new(ui::i18n!("store-build-version"), item.latest_version())
                .with_kind(TextKind::Identifier)
                .copyable(),
        );
    }
    if let Some(version) = item.versions.first()
        && let Some(runtime) = &version.runtime
    {
        rows.push(Property::new(ui::i18n!("store-runtime-reference"), runtime));
    }
    if let Some(hash) = item
        .versions
        .first()
        .and_then(|version| version.root_hash_hex.as_deref())
    {
        rows.push(
            Property::new(ui::i18n!("store-package-digest"), digest_summary(hash))
                .with_kind(TextKind::Secret),
        );
    }
    if let Some(key) = item.publisher_pubkey_hex.as_deref() {
        rows.push(
            Property::new(
                ui::i18n::I18nManager::lookup_plain("store-publisher-key"),
                format!("ed25519:{key}"),
            )
            .with_kind(TextKind::Identifier)
            .copyable(),
        );
    }
    rows
}

/// What the app is doing on this device.
pub(super) fn device_properties(item: &StoreItem) -> Vec<(String, String)> {
    use crate::model::InstalledState;
    let on_device = ui::i18n!("store-fact-on-device");
    match (&item.source, &item.installed) {
        (StoreSource::Tilo { .. }, InstalledState::Not) => {
            vec![(on_device, ui::i18n!("store-device-on-demand"))]
        }
        (
            StoreSource::Tilo { .. },
            InstalledState::Installed {
                version,
                pinned,
                user_managed,
                ..
            },
        ) => vec![(
            on_device,
            ui::i18n!(
                if !*user_managed {
                    "store-device-system-version"
                } else if *pinned {
                    "store-device-kept-version"
                } else {
                    "store-device-cached-version"
                },
                "version" => visible_version_of(item, version)
            ),
        )],
        (StoreSource::Flathub | StoreSource::FlatpakRemote { .. }, InstalledState::Not) => {
            vec![(on_device, ui::i18n!("store-device-not-installed"))]
        }
        (
            StoreSource::Flathub | StoreSource::FlatpakRemote { .. },
            InstalledState::Installed {
                version,
                user_managed: false,
                ..
            },
        ) => vec![(
            on_device,
            ui::i18n::I18nManager::format(
                "store-device-provided-version",
                &[("version", ui::i18n::ArgValue::Text(version.clone()))],
            ),
        )],
        (
            StoreSource::Flathub | StoreSource::FlatpakRemote { .. },
            InstalledState::Installed { version, .. },
        ) => vec![(
            on_device,
            ui::i18n!("store-device-installed-version", "version" => version.clone()),
        )],
    }
}
