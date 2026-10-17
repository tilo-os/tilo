//! The Store's pages: the catalogue, an app's details, the Flatpak remotes
//! and trust and moderation.
//!
//! Every page is one column. A page is a scrolling canvas holding one
//! centred, bounded column, so every part of it (the summary, the sections,
//! the cards) shares one inset and the scroll bar sits at the window edge
//! instead of over the content. Pages that read (details, remotes, trust)
//! keep a narrower measure but start at the same leading edge as the
//! catalogue, so the title and the content do not jump when a person
//! navigates. Everything is composed from `ui::widget` compositions: the
//! theme owns every colour, size and space.

use ui::TextRole;
use ui::framework::TextControl;
use ui::icons::Icon;
use ui::text_render::TextKind;
use ui::widget::media_card::{MediaAspect, MediaCard, MediaGrid, MediaSize, MediaSource};
use ui::widget::{
    Actions, Badge, Banner, Button, ButtonRole, CardGrid, ChipStyle, Column, Container,
    ControlSize, EmptyState, Field, Form, FormSection, ListRow, Property, PropertyList, Row,
    ScrollAxis, Scrollable, Section, SectionHeader, Spinner, Text, TextInput, Tone, Widget,
};

use super::{
    PAGE_SIZE, StoreApp, Theme, action_code, format_size, full_publisher, remote_state_label,
    visible_operation_status,
};
use crate::bindings;
use crate::model::{StoreItem, StoreSource, StoreTrust};
use crate::resource::{ReportProjection, StoreEffect};
use crate::widgets::{
    CardAction, CardDetails, app_icon, category_label, provenance, visible_version,
};

/// How wide a page's content may grow, in lines of body text, so it widens
/// with the text scale like every other text-sized measure.
#[derive(Clone, Copy)]
enum Measure {
    /// A grid of cards uses the canvas.
    Catalogue,
    /// Prose, facts and forms stay at a readable measure: the width a
    /// property list is itself bounded to, so every part of the page ends at
    /// the same trailing edge.
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
        } else {
            self.build_detail(theme)
                .unwrap_or_else(|| self.build_list(theme))
        }
    }

    /// A page: `children` stacked one group step apart in a bounded, centred
    /// column inside a vertical scroller. Only the catalogue keeps its scroll
    /// position across rebuilds.
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
        // Every page sits in the catalogue's column. A reading page keeps
        // its content to the readable width at that column's leading edge,
        // so the inset never changes.
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

    /// Optional payloads never download just because a Browser link was opened.
    fn build_runtime_recovery(&self, theme: &Theme) -> Element {
        use crate::runtime_recovery::Phase;
        let Some(recovery) = &self.runtime_recovery else {
            return self.build_list(theme);
        };
        let app_name = self
            .items
            .iter()
            .find(|item| item.id == recovery.target.app)
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

    // ---- catalogue ----

    /// Persistent operation feedback belongs in document flow, not in a
    /// transient overlay. `Banner` is the shared native alert treatment: it
    /// honours every theme, keeps a readable measure at large type, and
    /// exposes a status role to assistive technology.
    pub(super) fn operation_notice(&self) -> Option<Element> {
        if self.worker_busy {
            let detail = if self.status_line.is_empty() {
                "Package operation in progress…".to_string()
            } else {
                visible_operation_status(&self.status_line)
            };
            // Worker operations do not publish one trustworthy unit: a
            // launch, catalogue refresh, and Flatpak update cannot honestly
            // share a made-up percentage. The spinner makes activity visible
            // without pretending otherwise.
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
                                Banner::info("Working in background").with_message(detail),
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

    /// What the catalogue holds, or how much of it the filters show.
    pub(super) fn catalogue_source_summary(&self, shown: usize) -> String {
        if self.items.is_empty() {
            return "No apps loaded yet".to_string();
        }
        if self.filters_active() {
            return format!("Showing {shown} of {} apps", self.items.len());
        }
        let sources = self
            .items
            .iter()
            .map(|item| match &item.source {
                StoreSource::Tilo { .. } => "Tilo",
                StoreSource::Flathub => "Flathub",
                StoreSource::FlatpakRemote { name, .. } => name.as_str(),
            })
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        format!(
            "{} apps from {sources} source{}",
            self.items.len(),
            if sources == 1 { "" } else { "s" }
        )
    }

    /// The catalogue's actions, for the window's header row beside History.
    /// Every action is an icon, so the row never outgrows a narrow window.
    pub(super) fn catalogue_actions() -> Vec<Element> {
        let mut remotes = Button::icon_only("store.remotes", Icon::Globe, "Flatpak remotes");
        if let Some(binding) = bindings::effect(&StoreEffect::ShowRemotes) {
            remotes = remotes.on_action(binding);
        }
        let mut trust = Button::icon_only("store.safety", Icon::Shield, "Trust and moderation");
        if let Some(binding) = bindings::effect(&StoreEffect::ShowTrust) {
            trust = trust.on_action(binding);
        }
        let mut refresh = Button::icon_only("store.refresh", Icon::Refresh, "Refresh catalogues");
        if let Some(binding) = bindings::effect(&StoreEffect::Refresh) {
            refresh = refresh.on_action(binding);
        }
        let mut import = Button::new("store.package.import", &ui::i18n!("store-package-review"));
        if let Some(binding) = bindings::effect(&StoreEffect::ImportPackage {
            path: None,
            content: None,
            reported_size: None,
        }) {
            import = import.on_action(binding);
        }
        vec![
            Box::new(import),
            Box::new(ui::search::search_button(
                ui::search::SearchRequest::this_app(),
            )),
            Box::new(remotes),
            Box::new(trust),
            Box::new(refresh),
        ]
    }

    /// The top of the catalogue: what it holds, and a way back from filters.
    fn catalogue_summary(&self, shown: usize) -> Element {
        let mut row = Row::new()
            .no_wrap()
            .with_rhythm(ui::tokens::Rhythm::Tight)
            .with_align_items(taffy::AlignItems::Center)
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_flex_grow()
                    .with_child(Box::new(
                        Text::new(&self.catalogue_source_summary(shown))
                            .with_role(TextRole::Caption)
                            .dimmed(),
                    )),
            ));
        if self.filters_active() {
            let mut clear = Button::new("store.filters.clear", "Clear filters")
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small);
            if let Some(binding) = bindings::effect(&StoreEffect::ClearFilters) {
                clear = clear.on_action(binding);
            }
            row = row.push(Box::new(clear));
        }
        Box::new(row)
    }

    /// Idle browsing groups apps by the decision they invite (update, already
    /// here, new). A search keeps one relevance-ranked sequence instead.
    pub(super) fn group_catalogue_by_availability(&self) -> bool {
        self.query.trim().is_empty()
    }

    /// The card of one catalogue entry, wired to its typed actions.
    fn catalogue_card(&self, item: &StoreItem) -> Element {
        let action = item.action_kind(self.flatpak_available);
        let operation_active = self
            .active_operation
            .as_ref()
            .is_some_and(|(id, active_action)| id == &item.id && *active_action == action);
        let click_key = format!("store.app.{}.{}", item.id, action_code(action));
        let label = self.package_label(action);
        let select_key = format!("store.detail.{}", item.id);
        crate::widgets::store_card(
            item,
            CardAction {
                kind: action,
                label: Some(&label),
                key: &click_key,
                binding: bindings::package(item, action),
                in_progress: operation_active,
            },
            CardDetails {
                key: &select_key,
                binding: bindings::show_details(item),
            },
            self.images.as_ref(),
        )
    }

    /// The catalogue has nothing to show: loading, offline, empty, or every
    /// app filtered out. One sentence and at most one recovery action.
    fn empty_catalogue(&self) -> Element {
        if !self.items.is_empty()
            || (self.filters_active() && !self.worker_busy && self.operation_error.is_none())
        {
            let mut empty = EmptyState::new(ui::i18n!("store-search-empty"))
                .with_message(ui::i18n!("store-search-empty-help"))
                .with_action(ui::i18n!("store-search-clear"))
                .with_action_id("store.empty.clear");
            if let Some(binding) = bindings::effect(&StoreEffect::ClearFilters) {
                empty = empty.on_action(binding);
            }
            return Box::new(empty);
        }
        if self.worker_busy {
            return Box::new(
                EmptyState::new("Loading apps")
                    .with_message("Checking your configured catalogues."),
            );
        }
        // An unavailable or genuinely empty catalogue is recoverable by
        // refreshing. The visible action goes through the same one-shot
        // `store.refresh` resource action as the pointer, keyboard, assistive
        // technology and agents, so each requests one serialized operation.
        let empty = if let Some(error) = self.operation_error.as_ref().or_else(|| {
            self.remotes
                .iter()
                .find_map(|remote| remote.catalogue_error.as_ref())
        }) {
            EmptyState::error("Catalogues offline", visible_operation_status(error))
        } else {
            EmptyState::new("No apps available")
                .with_message("Add or enable a catalogue you trust, then refresh.")
        }
        .with_action("Refresh catalogue")
        .with_action_id("store.empty.refresh");
        Box::new(match bindings::effect(&StoreEffect::Refresh) {
            Some(binding) => empty.on_action(binding),
            None => empty,
        })
    }

    /// The catalogue: what it holds, then the apps in the groups a person
    /// decides between (an update, something already here, something new), or
    /// one relevance-ranked list while a search is active.
    pub(super) fn build_list(&self, theme: &Theme) -> Element {
        let visible = self.visible_items();
        let mut children = vec![self.catalogue_summary(visible.len())];
        if !self.items.is_empty()
            && let Some(notice) = self.operation_notice()
        {
            children.push(notice);
        }
        if visible.is_empty() {
            children.push(self.empty_catalogue());
            return self.page(theme, Measure::Catalogue, true, children);
        }

        let page_count = visible.len().div_ceil(PAGE_SIZE).max(1);
        let page = self.catalog_page.min(page_count.saturating_sub(1));
        let start = page.saturating_mul(PAGE_SIZE);
        let grouped = self.group_catalogue_by_availability();
        let mut updates: Vec<Element> = Vec::new();
        let mut here: Vec<Element> = Vec::new();
        let mut discover: Vec<Element> = Vec::new();
        let mut matches: Vec<Element> = Vec::new();
        for item in visible.into_iter().skip(start).take(PAGE_SIZE) {
            let card = self.catalogue_card(item);
            if !grouped {
                matches.push(card);
            } else if item.installed.update_available() {
                updates.push(card);
            } else if item.installed.is_installed() {
                here.push(card);
            } else {
                discover.push(card);
            }
        }
        let section = |title: &str, cards: Vec<Element>| -> Option<Element> {
            (!cards.is_empty()).then(|| {
                let count = cards.len();
                let mut header = SectionHeader::heading(title).count(count);
                if title == "Updates" {
                    let mut update = Button::new("store.update-all", "Update all")
                        .with_icon(Icon::Refresh)
                        .with_enabled(self.active_operation.is_none());
                    if let Some(binding) = bindings::effect(&StoreEffect::UpdateAll) {
                        update = update.on_action(binding);
                    }
                    header = header.action(update);
                }
                Box::new(Section::new(vec![Box::new(CardGrid::new(cards))]).with_header(header))
                    as Element
            })
        };
        children.extend(section("Updates", updates));
        children.extend(section("On this device", here));
        children.extend(section("Discover", discover));
        children.extend(section("Search results", matches));
        if page_count > 1 {
            children.push(Self::paging(page, page_count));
        }
        self.page(theme, Measure::Catalogue, true, children)
    }

    /// Previous and next page of a catalogue longer than one page.
    fn paging(page: usize, page_count: usize) -> Element {
        let mut row = Row::new().with_rhythm(ui::tokens::Rhythm::Related);
        if page > 0 {
            let mut previous =
                Button::new("store.page.previous", "Previous").tooltip("Previous catalogue page");
            if let Some(binding) = bindings::effect(&StoreEffect::PreviousPage) {
                previous = previous.on_action(binding);
            }
            row = row.push(Box::new(previous));
        }
        row = row.push(Box::new(
            Text::new(&format!("Page {} of {page_count}", page + 1))
                .with_role(TextRole::Caption)
                .dimmed()
                .numeric(),
        ));
        if page + 1 < page_count {
            let mut next = Button::new("store.page.next", "Next").tooltip("Next catalogue page");
            if let Some(binding) = bindings::effect(&StoreEffect::NextPage) {
                next = next.on_action(binding);
            }
            row = row.push(Box::new(next));
        }
        Box::new(row)
    }

    // ---- app details ----

    /// What the app is, for the person deciding on it: the badges under its
    /// name.
    fn availability_badge(&self, item: &StoreItem) -> Badge {
        let operation_active = self.active_operation.as_ref().is_some_and(|(id, action)| {
            id == &item.id && *action == item.action_kind(self.flatpak_available)
        });
        if operation_active {
            Badge::text("Working").style(ChipStyle::Tinted(Tone::Info))
        } else if item.installed.update_available() {
            Badge::text("Update available").style(ChipStyle::Tinted(Tone::Warning))
        } else if item.installed.is_installed() {
            Badge::text("On this device").style(ChipStyle::Tinted(Tone::Success))
        } else if matches!(item.source, StoreSource::Tilo { .. }) {
            Badge::text("On demand").style(ChipStyle::Neutral)
        } else {
            Badge::text("Not installed").style(ChipStyle::Neutral)
        }
    }

    /// The standing badges under the app's name. A source that vouches for
    /// the publisher says so once here; the rest of the page does not repeat
    /// it.
    fn detail_badges(&self, item: &StoreItem, gap: f32) -> Element {
        let mut row = Row::new()
            .no_wrap()
            .with_gap(gap)
            .push(Box::new(self.availability_badge(item)));
        if matches!(
            item.trust,
            StoreTrust::PublisherSigned | StoreTrust::TrustedAttestor
        ) {
            row = row.push(Box::new(
                Badge::text("Publisher verified").style(ChipStyle::Tinted(Tone::Success)),
            ));
        }
        Box::new(row)
    }

    /// The decision actions of the details page: the app's one main action,
    /// then keeping it on the device and removing it.
    fn detail_actions(&self, item: &StoreItem) -> Element {
        let action = item.action_kind(self.flatpak_available);
        let imported = self
            .imported_review
            .as_ref()
            .is_some_and(|(id, _, _)| id == &item.id);
        let label = if imported {
            ui::i18n!("store-package-install")
        } else {
            self.package_label(action)
        };
        let mut primary = Button::new(
            &format!("store.app.{}.{}", item.id, action_code(action)),
            &label,
        )
        .role(ButtonRole::Primary)
        .with_enabled(item.unavailable_action(action).is_none())
        .tooltip(item.unavailable_action(action).map_or_else(
            || format!("{} {}", self.package_label(action), item.name),
            |(_, reason)| reason,
        ));
        let binding = if imported {
            self.imported_review.as_ref().and_then(|(_, root, _)| {
                bindings::effect(&StoreEffect::InstallImportedPackage {
                    app: item.id.clone(),
                    root: root.clone(),
                })
            })
        } else {
            bindings::package(item, action)
        };
        if let Some(binding) = binding {
            primary = primary.on_action(binding);
        }
        let mut row = Row::new()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(Box::new(primary));
        if let Some(pin_action) = item.pin_action() {
            let mut button = Button::new(
                &format!("store.app.{}.{}", item.id, action_code(pin_action)),
                pin_action.label(),
            )
            .tooltip(format!("{} {}", pin_action.label(), item.name));
            if let Some(binding) = bindings::package(item, pin_action) {
                button = button.on_action(binding);
            }
            row = row.push(Box::new(button));
        }
        if let Some(remove_action) = item.remove_action() {
            let mut button = Button::new(&format!("store.app.{}.remove", item.id), "Remove")
                .role(ButtonRole::Destructive)
                .tooltip(format!("Remove {}", item.name));
            if let Some(binding) = bindings::package(item, remove_action) {
                button = button.on_action(binding);
            }
            row = row.push(Box::new(button));
        }
        Box::new(row)
    }

    /// The top of the details page: the app's icon, name and standing, the
    /// actions that decide on it, and what it is.
    fn detail_summary(&self, item: &StoreItem, theme: &Theme) -> Element {
        let sp = &theme.tokens.spacing;
        let identity = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(Box::new(
                Text::new(&item.name)
                    .with_role(TextRole::LargeTitle)
                    .with_heading_level(1),
            ))
            .push(Box::new(
                Text::new(&provenance(item))
                    .with_role(TextRole::Caption)
                    .dimmed(),
            ))
            .push(self.detail_badges(item, sp.s2));
        let hero = Row::new()
            .no_wrap()
            .with_rhythm(ui::tokens::Rhythm::Section)
            .align_start()
            .push(Box::new(app_icon(
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
        let mut summary = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Section)
            .push(Box::new(hero));
        if !item.tagline.trim().is_empty() {
            summary = summary.push(Box::new(
                Text::new(item.tagline.trim()).with_role(TextRole::Body),
            ));
        }
        Box::new(summary.push(self.detail_actions(item)))
    }

    /// The details page of the focused app.
    pub(super) fn build_detail(&self, theme: &Theme) -> Option<Element> {
        let id = self.focused_detail.as_ref()?;
        let item = self.items.iter().find(|item| &item.id == id)?;
        let mut children: Vec<Element> = Vec::new();
        if let Some(notice) = self.operation_notice() {
            children.push(notice);
        }
        children.push(self.detail_summary(item, theme));
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
        if let Some(store) = &self.images
            && !item.screenshot_urls.is_empty()
        {
            let previews = item
                .screenshot_urls
                .iter()
                .take(3)
                .enumerate()
                .map(|(index, url)| {
                    MediaCard::new(
                        format!("store.preview.{}.{index}", item.id),
                        MediaSource::Url {
                            store: store.clone(),
                            url: url.clone(),
                        },
                    )
                    .aspect(MediaAspect::Wide)
                })
                .collect();
            children.push(Box::new(Section::titled(
                "Previews",
                vec![Box::new(
                    MediaGrid::new(previews).with_size(MediaSize::Small),
                )],
            )));
        }
        if let Some(bot) = &item.bot {
            children.push(bot_section(bot));
        }
        if !item.description.trim().is_empty() && item.description.trim() != item.tagline.trim() {
            children.push(Box::new(Section::titled(
                ui::i18n!("store-about-app"),
                vec![Box::new(
                    Text::new(item.description.trim())
                        .with_id("store.detail.description")
                        .with_role(TextRole::Body),
                )],
            )));
        }
        // One property list for the whole page, so every value starts at the
        // same edge: each list measures its own label column.
        children.push(Box::new(Section::titled(
            "Details",
            vec![Box::new(PropertyList::new(detail_properties(item)))],
        )));
        children.push(Box::new(Section::titled(
            "Trust",
            vec![Self::prose(
                &self
                    .local_only_notice(item)
                    .unwrap_or_else(|| trust_explanation(item)),
            )],
        )));
        children.push(self.technical_details(item));
        children.push(Self::report_section(item));
        Some(self.page(theme, Measure::Reading, false, children))
    }

    /// For an installed app no repository describes: whether one may yet
    /// (it has not answered) or none lists it, said once on its details page.
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
        self.technical_properties_section(technical_properties(item))
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
            "Create report draft",
        )
        .tooltip("Create a local, collaborative report draft");
        if let Some(binding) = bindings::draft_report(item) {
            report = report.on_action(binding);
        }
        let mut moderation = Button::new("store.safety.detail", "Moderation details")
            .role(ButtonRole::Plain)
            .tooltip("Review the Store trust and moderation model");
        if let Some(binding) = bindings::effect(&StoreEffect::ShowTrust) {
            moderation = moderation.on_action(binding);
        }
        Box::new(Section::titled(
            "Report a problem",
            vec![
                Self::prose(
                    "Report concerns together with your workspace, or review how verified decisions and appeals work.",
                ),
                Box::new(
                    Row::new()
                        .with_rhythm(ui::tokens::Rhythm::Related)
                        .push(Box::new(report))
                        .push(Box::new(moderation)),
                ),
            ],
        ))
    }

    // ---- trust and moderation ----

    /// Reports the person has drafted, how decisions work, and what a
    /// signature does and does not promise.
    pub(super) fn build_safety_center(&self, theme: &Theme) -> Element {
        let mut children: Vec<Element> = Vec::new();
        children.extend(self.operation_notice());
        if self.reports.is_empty() {
            children.push(Box::new(
                EmptyState::new("No report drafts")
                    .with_message("Open an app and choose Create report draft."),
            ));
        }
        for report in &self.reports {
            children.push(self.report_form(report));
        }
        let explainers: [(&str, &str); 3] = [
            (
                "How reports work",
                "Reports are signed with your workspace identity and stay drafts that your workspace shares until you submit them. The Store shows a new status only once it has checked it, so an offline or refused request stays a draft you can retry.",
            ),
            (
                "What a signature means",
                "A verified package comes from its publisher and has not been changed. That does not make an app useful, private or honest, and the Store never invents a review badge.",
            ),
            (
                "Publishing guidelines",
                "Listings must describe what the app does, name any paid or online services it depends on, ask only for the permissions it needs, keep people's data across updates, work with assistive technology, and never impersonate others or track people without saying so.",
            ),
        ];
        for (title, text) in explainers {
            children.push(Box::new(Section::titled(title, vec![Self::prose(text)])));
        }
        self.page(theme, Measure::Reading, false, children)
    }

    /// One report draft or moderation case: its status, the concern field
    /// and what can be done with it.
    fn report_form(&self, report: &ReportProjection) -> Element {
        let item = self.items.iter().find(|item| item.id == report.app_id);
        let mut export = Button::new(&format!("{}.export", report.id), "Export evidence")
            .tooltip("Save a bounded JSON evidence envelope through the system file picker");
        if let Some(binding) =
            bindings::report(|app| StoreEffect::ExportEvidence { app }, &report.app_id)
        {
            export = export.on_action(binding);
        }
        let mut buttons: Vec<Button> = Vec::new();
        if self.reported_app(report).is_ok() {
            if report.is_case() {
                let case_actions: [super::CaseControl; 2] = [
                    ("refresh-status", "Refresh status", |app| {
                        StoreEffect::RefreshStatus { app }
                    }),
                    ("appeal", "Appeal decision", |app| StoreEffect::Appeal {
                        app,
                    }),
                ];
                for (action, label, effect) in case_actions {
                    let mut button = Button::new(&format!("{}.{action}", report.id), label);
                    if let Some(binding) = bindings::report(effect, &report.app_id) {
                        button = button.on_action(binding);
                    }
                    buttons.push(button);
                }
            } else {
                let mut submit = Button::new(&format!("{}.submit", report.id), "Submit report")
                    .role(ButtonRole::Primary)
                    .tooltip("Sign and submit this report to the authoritative registry");
                if let Some(binding) =
                    bindings::report(|app| StoreEffect::SubmitReport { app }, &report.app_id)
                {
                    submit = submit.on_action(binding);
                }
                buttons.push(submit);
            }
        }
        let kind = if report.is_case() {
            "Moderation case"
        } else {
            "Report draft"
        };
        let name = item.map_or(report.app_id.as_str(), |item| item.name.as_str());
        let mut actions = Row::new()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .with_justify(taffy::JustifyContent::End)
            .push(Box::new(export));
        for button in buttons {
            actions = actions.push(Box::new(button));
        }
        let field = Field::new("What happened?", self.report_note_field(&report.app_id))
            .with_help("Describe observable behavior and evidence another reviewer can verify.");
        Box::new(Section::titled(
            name,
            vec![
                Box::new(
                    Row::new()
                        .no_wrap()
                        .with_rhythm(ui::tokens::Rhythm::Related)
                        .with_align_items(taffy::AlignItems::Center)
                        .push(Box::new(
                            Text::new(kind).with_role(TextRole::Caption).dimmed(),
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
            .with_placeholder("Describe what happened");
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
        let mut add = Button::new("store.remote.add", "Add remote").role(ButtonRole::Primary);
        if let Some(binding) = bindings::add_remote(&self.remote_name_state, &self.remote_url_state)
        {
            add = add.on_action(binding);
        }
        let mut refresh = Button::new("store.remote.refresh", "Refresh remotes");
        if let Some(binding) = bindings::effect(&StoreEffect::RefreshRemotes) {
            refresh = refresh.on_action(binding);
        }
        let form = Form::new()
            .section(
                FormSection::new("Add a remote")
                    .field(Field::new(
                        "Name",
                        TextInput::from_state(&self.remote_name_state)
                            .with_id("store.remote.name")
                            .with_kind(ui::widget::TextInputKind::Name)
                            .with_placeholder("Remote name"),
                    ))
                    .field(
                        Field::new(
                            "Repository URL",
                            TextInput::from_state(&self.remote_url_state)
                                .with_id("store.remote.url")
                                .with_kind(ui::widget::TextInputKind::Url)
                                .with_placeholder("https://"),
                        )
                        .with_help("Repositories must use HTTPS."),
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
                    row = row.action_binding(Icon::Trash, "Remove remote", binding);
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
                SectionHeader::heading("Configured remotes").count(count),
            )));
        }
        self.page(theme, Measure::Reading, false, children)
    }
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
        StoreTrust::PublisherSigned => {
            "Publisher identity and package integrity are verified. This is not an independent quality review.".into()
        }
        StoreTrust::Flathub => "Tilo has not independently reviewed this app.".into(),
        StoreTrust::ConfiguredRemote => {
            "This metadata comes from a configured repository. Tilo has not independently reviewed the app.".into()
        }
    }
}

/// Every fact the details page lists, in one reading order: what the package
/// is, where it comes from, what it is doing on this device and where the
/// project lives.
/// What a directory bot does and needs, as its signed listing says.
fn bot_section(bot: &crate::model::BotListing) -> Element {
    let mut lines: Vec<Element> = bot
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
            Box::new(
                Text::new(&text)
                    .with_id(format!("store.bot.trigger.{index}"))
                    .with_role(TextRole::Body),
            ) as Element
        })
        .collect();
    if !bot.apps.is_empty() {
        lines.push(Box::new(
            Text::new(&ui::i18n::I18nManager::format(
                "store-bot-needs-apps",
                &[("apps", ui::i18n::ArgValue::Text(bot.apps.join(", ")))],
            ))
            .with_id("store.bot.apps")
            .with_role(TextRole::Body),
        ));
    }
    if !bot.connectors.is_empty() {
        let connectors = bot
            .connectors
            .iter()
            .map(|c| format!("{} \u{2265} {}", c.package_id, c.min_version))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(Box::new(
            Text::new(&ui::i18n::I18nManager::format(
                "store-bot-needs-connectors",
                &[("connectors", ui::i18n::ArgValue::Text(connectors))],
            ))
            .with_id("store.bot.connectors")
            .with_role(TextRole::Body),
        ));
    }
    if bot.appearance {
        lines.push(Box::new(
            Text::new(&ui::i18n!("store-bot-appearance"))
                .with_id("store.bot.appearance")
                .with_role(TextRole::Body),
        ));
    }
    Box::new(Section::titled(ui::i18n!("store-bot-what-it-does"), lines))
}

fn detail_properties(item: &StoreItem) -> Vec<Property> {
    let mut rows = package_properties(item);
    rows.extend(publisher_properties(item));
    rows.extend(
        device_properties(item)
            .into_iter()
            .map(|(label, value)| Property::new(label, value)),
    );
    rows.extend(link_properties(item));
    rows
}

/// The package's facts: identity, version and what it weighs.
fn package_properties(item: &StoreItem) -> Vec<Property> {
    let mut rows = Vec::new();
    if !item.supported_languages.is_empty() {
        rows.push(Property::new(
            ui::i18n!("store-supported-languages"),
            item.supported_languages.join(", "),
        ));
    }
    if !item.categories.is_empty() {
        rows.push(Property::new(
            "Categories",
            item.categories
                .iter()
                .map(|category| category_label(category))
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    if let Some(version) = item.versions.first() {
        rows.push(
            Property::new("Latest version", visible_version(item)).with_kind(TextKind::Numeric),
        );
        if let Some(runtime) = &version.runtime {
            rows.push(Property::new(ui::i18n!("store-runtime-reference"), runtime));
        }
        if version.size_bytes > 0 {
            rows.push(
                Property::new("Download size", format_size(version.size_bytes))
                    .with_kind(TextKind::Numeric),
            );
        }
    }
    rows
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

/// Which catalogue lists the app and who published it. Whether the
/// publisher is verified is the badge's and the trust note's to say.
fn publisher_properties(item: &StoreItem) -> Vec<Property> {
    let source = match &item.source {
        StoreSource::Tilo { registry_name } => registry_name.clone(),
        StoreSource::Flathub => "Flathub".to_string(),
        StoreSource::FlatpakRemote { title, .. } => title.clone(),
    };
    let mut rows = vec![Property::new("Catalogue", source)];
    if item.publisher_pubkey_hex.is_none() {
        rows.push(Property::new("Publisher", full_publisher(item)));
    }
    rows
}

/// Exact package identity remains inspectable without dominating the app summary.
fn technical_properties(item: &StoreItem) -> Vec<Property> {
    let mut rows = vec![
        Property::new("App ID", &item.id)
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
    if let Some(hash) = item
        .versions
        .first()
        .and_then(|version| version.root_hash_hex.as_deref())
    {
        rows.push(
            Property::new("Package digest", digest_summary(hash)).with_kind(TextKind::Secret),
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
pub(super) fn device_properties(item: &StoreItem) -> Vec<(&'static str, String)> {
    use crate::model::InstalledState;
    match (&item.source, &item.installed) {
        (StoreSource::Tilo { .. }, InstalledState::Not) => vec![
            ("On this device", "Available on demand".into()),
            (
                "Storage",
                "Fetched when opened, not kept on this device".into(),
            ),
        ],
        (
            StoreSource::Tilo { .. },
            InstalledState::Installed {
                version,
                available,
                pinned,
                user_managed,
            },
        ) => vec![
            ("On this device", with_update("Cached", version, available)),
            (
                "Storage",
                if !*user_managed {
                    "Provided by the system"
                } else if *pinned {
                    "Kept on this device"
                } else {
                    "Not kept, the cache may be reclaimed"
                }
                .into(),
            ),
        ],
        (StoreSource::Flathub | StoreSource::FlatpakRemote { .. }, InstalledState::Not) => {
            vec![("On this device", "Not installed".into())]
        }
        (
            StoreSource::Flathub | StoreSource::FlatpakRemote { .. },
            InstalledState::Installed {
                version,
                user_managed: false,
                ..
            },
        ) => vec![(
            "On this device",
            ui::i18n::I18nManager::format(
                "store-device-provided-version",
                &[("version", ui::i18n::ArgValue::Text(version.clone()))],
            ),
        )],
        (
            StoreSource::Flathub | StoreSource::FlatpakRemote { .. },
            InstalledState::Installed {
                version, available, ..
            },
        ) => vec![(
            "On this device",
            with_update("Installed", version, available),
        )],
    }
}

/// The installed version, and the newer one the catalogue lists when there
/// is one. The package's own latest version is listed once, with its facts.
fn with_update(state: &str, version: &str, available: &str) -> String {
    if available.is_empty() || available == version {
        format!("{state}, version {version}")
    } else {
        format!("{state}, version {version}; {available} is available")
    }
}

/// Where the project lives, each with an action that opens it.
fn link_properties(item: &StoreItem) -> Vec<Property> {
    [
        ("Homepage", item.homepage_url.as_deref()),
        ("Source code", item.source_url.as_deref()),
    ]
    .into_iter()
    .filter_map(|(label, url)| {
        let url = url?.to_owned();
        let target = url.clone();
        Some(
            Property::new(label, url)
                .with_kind(TextKind::Identifier)
                .action(Icon::ExternalLink, format!("Open {label}"), move || {
                    ui::widget::open_external_url(&target);
                }),
        )
    })
    .collect()
}
