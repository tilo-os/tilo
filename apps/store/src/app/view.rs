//! The Store's pages: the catalogue, an app's details, the Flatpak remotes
//! and trust and moderation.
//!
//! Every page is one column. A page is a scrolling canvas holding one
//! centred, bounded column, so every part of it (the toolbar, the sections,
//! the cards) shares one inset and the scroll bar sits at the window edge
//! instead of over the content. Pages that read (details, remotes, trust)
//! keep a narrower measure but start at the same leading edge as the
//! catalogue, so the title and the content do not jump when a person
//! navigates. Everything is composed from `ui::widget` compositions: the
//! theme owns every colour, size and space.

use ui::TextRole;
use ui::icons::Icon;
use ui::text_render::TextKind;
use ui::widget::media_card::{MediaAspect, MediaCard, MediaGrid, MediaSize, MediaSource};
use ui::widget::{
    Actions, Badge, Banner, Button, ButtonRole, CardGrid, ChipStyle, Column, Container,
    ControlSize, EmptyState, Field, Form, FormSection, ListRow, Property, PropertyList, Row,
    ScrollAxis, Scrollable, Section, SectionHeader, Spinner, Text, TextInput, Tone, Widget,
};

use super::{
    PAGE_SIZE, StoreApp, Theme, action_code, clear_filters_binding, detail_binding, format_size,
    full_publisher, item_action_binding, moderation_case_spec, navigation_binding,
    open_surface_binding, refresh_binding, remote_form_binding, remote_refresh_binding,
    remote_remove_binding, report_binding, report_export_binding, report_note_key,
    report_submit_binding, short_publisher, visible_operation_status,
};
use crate::model::{StoreItem, StoreSource, StoreTrust};
use crate::widgets::{CardAction, CardDetails, app_icon, category_label, provenance};

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
        if self.safety_open {
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
            .with_gap(theme.tokens.spacing.rhythm.group);
        for child in children {
            column = column.push(child);
        }
        let mut scroll = Scrollable::new(ScrollAxis::Vertical).fill_parent_height();
        if keeps_scroll {
            scroll = scroll.with_persisted_offset(self.scroll_offset.clone());
        }
        // Every page sits in the catalogue's column. A reading page bounds
        // its content inside that column from the leading edge, a spacer
        // taking what the bound leaves, so the inset never changes.
        let content: Element = match measure {
            Measure::Catalogue => Box::new(column),
            Measure::Reading => Box::new(
                Row::new()
                    .no_wrap()
                    .push(Box::new(
                        Container::new()
                            .with_no_padding()
                            .with_centered_max_width(measure.width(theme))
                            .with_child(Box::new(column)),
                    ))
                    .push(Box::new(
                        Container::new().with_no_padding().with_flex_grow(),
                    )),
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

    /// The page title: the top of a content page.
    fn page_title(title: &str) -> Element {
        Box::new(
            Text::new(title)
                .with_role(TextRole::LargeTitle)
                .with_heading_level(1),
        )
    }

    /// The top of every page that is not the catalogue: the way back to it.
    /// The window's own header carries History.
    fn back_row(id: &str) -> Element {
        let mut back = Button::new(id, "Back to apps", None)
            .with_icon(Icon::ArrowLeft)
            .role(ButtonRole::Plain)
            .tooltip("Return to the app catalogue");
        if let Some(binding) = navigation_binding("show-catalogue", "Back to apps", "catalogue") {
            back = back.on_action(binding);
        }
        Box::new(Row::new().no_wrap().push(Box::new(back)))
    }

    /// A quiet paragraph of prose.
    fn prose(text: &str) -> Element {
        Box::new(Text::new(text).with_role(TextRole::Body).dimmed())
    }

    // ---- catalogue ----

    /// Persistent operation feedback belongs in document flow, not in a
    /// transient overlay. `Banner` is the shared native alert treatment: it
    /// honours every theme, keeps a readable measure at large type, and
    /// exposes a status role to assistive technology.
    pub(super) fn operation_notice(&self, theme: &Theme) -> Option<Element> {
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
                    .with_gap(theme.tokens.spacing.s3)
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
        self.operation_error.as_ref().map(|_| {
            Box::new(
                Banner::error("The last operation failed")
                    .with_message(visible_operation_status(&self.status_line)),
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
            })
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        format!(
            "{} apps from {sources} source{}",
            self.items.len(),
            if sources == 1 { "" } else { "s" }
        )
    }

    /// The top of the catalogue: what it holds on the leading edge, and the
    /// window's actions on the trailing edge. Every action is an icon, so
    /// the row never outgrows a narrow window.
    fn catalogue_toolbar(&self, theme: &Theme, shown: usize) -> Element {
        let mut remotes = Button::icon_only("store.remotes", Icon::Globe, "Flatpak remotes");
        if let Some(binding) = open_surface_binding(
            "store:remotes",
            "store.remote-list",
            "Flatpak remotes",
            "show",
            "Show Flatpak remotes",
            "store.view-remotes",
        ) {
            remotes = remotes.on_action(binding);
        }
        let mut trust = Button::icon_only("store.safety", Icon::Shield, "Trust and moderation");
        if let Some(binding) = open_surface_binding(
            "store:moderation",
            "store.moderation",
            "Trust and moderation",
            "show",
            "Show trust and moderation",
            "store.view-trust",
        ) {
            trust = trust.on_action(binding);
        }
        let mut refresh = Button::icon_only("store.refresh", Icon::Refresh, "Refresh catalogues");
        if let Some(binding) = refresh_binding() {
            refresh = refresh.on_action(binding);
        }
        // One row: what the catalogue holds takes the leading space, every
        // action is a trailing icon (a row nested here would claim the whole
        // width and push the summary onto its own line).
        let mut row = Row::new()
            .no_wrap()
            .with_gap(theme.tokens.spacing.s1)
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
            let mut clear = Button::new("store.filters.clear", "Clear filters", None)
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small);
            if let Some(binding) = clear_filters_binding() {
                clear = clear.on_action(binding);
            }
            row = row.push(Box::new(clear));
        }
        Box::new(
            row.push(Box::new(ui::search::search_button(
                ui::search::SearchRequest::this_app(),
            )))
            .push(Box::new(remotes))
            .push(Box::new(trust))
            .push(Box::new(refresh)),
        )
    }

    /// Idle browsing groups apps by the decision they invite (update, already
    /// here, new). A search keeps one relevance-ranked sequence instead.
    pub(super) fn group_catalogue_by_availability(&self) -> bool {
        self.query.trim().is_empty()
    }

    /// The card of one catalogue entry, wired to its typed actions.
    fn catalogue_card(&self, item: &StoreItem, theme: &Theme) -> Element {
        let action = item.action_kind(self.flatpak_available);
        let operation_active = self
            .active_operation
            .as_ref()
            .is_some_and(|(id, active_action)| id == &item.id && *active_action == action);
        let click_key = format!("store.app.{}.{}", item.id, action_code(action));
        let select_key = format!("store.detail.{}", item.id);
        crate::widgets::store_card(
            item,
            CardAction {
                kind: action,
                key: &click_key,
                binding: item_action_binding(item, action),
                in_progress: operation_active,
            },
            CardDetails {
                key: &select_key,
                binding: detail_binding(item),
            },
            self.images.as_ref(),
            theme,
        )
    }

    /// The catalogue has nothing to show: loading, offline, empty, or every
    /// app filtered out. One sentence and at most one recovery action.
    fn empty_catalogue(&self) -> Element {
        if !self.items.is_empty() {
            let mut empty = EmptyState::new("No apps found")
                .with_message("Try a different search or category.")
                .with_action("Clear filters")
                .with_action_id("store.empty.clear");
            if let Some(binding) = clear_filters_binding() {
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
        let empty = if self.operation_error.is_some() {
            EmptyState::error(
                "Catalogues offline",
                visible_operation_status(&self.status_line),
            )
        } else {
            EmptyState::new("No apps available")
                .with_message("Add or enable a catalogue you trust, then refresh.")
        }
        .with_action("Refresh catalogue")
        .with_action_id("store.empty.refresh");
        Box::new(match refresh_binding() {
            Some(binding) => empty.on_action(binding),
            None => empty,
        })
    }

    /// The catalogue: the toolbar, then the apps in the groups a person
    /// decides between (an update, something already here, something new), or
    /// one relevance-ranked list while a search is active.
    pub(super) fn build_list(&self, theme: &Theme) -> Element {
        let visible = self.visible_items();
        let mut children = vec![self.catalogue_toolbar(theme, visible.len())];
        if !self.items.is_empty()
            && let Some(notice) = self.operation_notice(theme)
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
            let card = self.catalogue_card(item, theme);
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
                Box::new(
                    Section::new(vec![Box::new(CardGrid::new(cards))])
                        .with_header(SectionHeader::heading(title).count(count)),
                ) as Element
            })
        };
        children.extend(section("Updates", updates));
        children.extend(section("On this device", here));
        children.extend(section("Discover", discover));
        children.extend(section("Search results", matches));
        if page_count > 1 {
            children.push(Self::paging(theme, page, page_count));
        }
        self.page(theme, Measure::Catalogue, true, children)
    }

    /// Previous and next page of a catalogue longer than one page.
    fn paging(theme: &Theme, page: usize, page_count: usize) -> Element {
        let mut row = Row::new().with_gap(theme.tokens.spacing.s2);
        if page > 0 {
            let mut previous = Button::new("store.page.previous", "Previous", None)
                .tooltip("Previous catalogue page");
            if let Some(binding) =
                navigation_binding("previous-page", "Previous catalogue page", "previous-page")
            {
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
            let mut next =
                Button::new("store.page.next", "Next", None).tooltip("Next catalogue page");
            if let Some(binding) =
                navigation_binding("next-page", "Next catalogue page", "next-page")
            {
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
        if item.trust == StoreTrust::PublisherSigned {
            row = row.push(Box::new(
                Badge::text("Publisher verified").style(ChipStyle::Tinted(Tone::Success)),
            ));
        }
        Box::new(row)
    }

    /// The decision actions of the details page: the app's one main action,
    /// then keeping it on the device and removing it.
    fn detail_actions(&self, item: &StoreItem, theme: &Theme) -> Element {
        let action = item.action_kind(self.flatpak_available);
        let mut primary = Button::new(
            &format!("store.app.{}.{}", item.id, action_code(action)),
            action.label(),
            None,
        )
        .role(ButtonRole::Primary)
        .tooltip(format!("{} {}", action.label(), item.name));
        if let Some(binding) = item_action_binding(item, action) {
            primary = primary.on_action(binding);
        }
        let mut row = Row::new()
            .with_gap(theme.tokens.spacing.s2)
            .push(Box::new(primary));
        if let Some(pin_action) = item.pin_action() {
            let mut button = Button::new(
                &format!("store.app.{}.{}", item.id, action_code(pin_action)),
                pin_action.label(),
                None,
            )
            .tooltip(format!("{} {}", pin_action.label(), item.name));
            if let Some(binding) = item_action_binding(item, pin_action) {
                button = button.on_action(binding);
            }
            row = row.push(Box::new(button));
        }
        if let Some(remove_action) = item.remove_action() {
            let mut button = Button::new(&format!("store.app.{}.remove", item.id), "Remove", None)
                .role(ButtonRole::Destructive)
                .tooltip(format!("Remove {}", item.name));
            if let Some(binding) = item_action_binding(item, remove_action) {
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
            .with_gap(sp.s2)
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
            .with_gap(sp.s4)
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
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(sp.s4)
                .push(Box::new(hero))
                .push(self.detail_actions(item, theme))
                .push(Box::new(
                    Text::new(&item.description).with_role(TextRole::Body),
                )),
        )
    }

    /// The details page of the focused app.
    pub(super) fn build_detail(&self, theme: &Theme) -> Option<Element> {
        let id = self.focused_detail.as_ref()?;
        let item = self.items.iter().find(|item| &item.id == id)?;
        let mut children: Vec<Element> = vec![Self::back_row("store.detail.back")];
        if let Some(notice) = self.operation_notice(theme) {
            children.push(notice);
        }
        children.push(self.detail_summary(item, theme));
        // One property list for the whole page, so every value starts at the
        // same edge: each list measures its own label column.
        children.push(Box::new(Section::titled(
            "Details",
            vec![Box::new(PropertyList::new(detail_properties(item)))],
        )));
        let locally_retained = matches!(
            &item.source,
            StoreSource::Tilo { registry_name } if registry_name == "Installed on this device"
        );
        children.push(Box::new(Section::titled(
            "Trust",
            vec![Self::prose(trust_explanation(item, locally_retained))],
        )));
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
                    MediaGrid::new(previews).with_size(MediaSize::Regular),
                )],
            )));
        }
        children.push(Self::report_section(item, theme));
        Some(self.page(theme, Measure::Reading, false, children))
    }

    /// Reporting a problem with the app, or reading how decisions are made.
    fn report_section(item: &StoreItem, theme: &Theme) -> Element {
        let mut report = Button::new(
            &format!("store.report.create.{}", item.id),
            "Create report draft",
            None,
        )
        .tooltip("Create a local, collaborative report draft");
        if let Some(binding) = report_binding(item) {
            report = report.on_action(binding);
        }
        let mut moderation = Button::new("store.safety.detail", "Moderation details", None)
            .role(ButtonRole::Plain)
            .tooltip("Review the Store trust and moderation model");
        if let Some(binding) = open_surface_binding(
            "store:moderation",
            "store.moderation",
            "Trust and moderation",
            "show",
            "Show trust and moderation",
            "store.view-trust",
        ) {
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
                        .with_gap(theme.tokens.spacing.s2)
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
        let mut children: Vec<Element> = vec![
            Self::back_row("store.safety.back"),
            Self::page_title("Trust and moderation"),
        ];
        children.extend(self.operation_notice(theme));
        if self.reports.is_empty() {
            children.push(Box::new(
                EmptyState::new("No report drafts")
                    .with_message("Open an app and choose Create report draft."),
            ));
        }
        for report in &self.reports {
            children.push(self.report_form(report, theme));
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
    fn report_form(&self, report: &super::ReportRecord, theme: &Theme) -> Element {
        let item = self.items.iter().find(|item| item.id == report.app_id);
        let note = self
            .report_notes
            .get(&report.app_id)
            .map_or("", String::as_str);
        let is_case = report.id.starts_with("case-");
        let mut export = Button::new(&format!("{}.export", report.id), "Export evidence", None)
            .tooltip("Save a bounded JSON evidence envelope through the system file picker");
        if let Some(binding) = report_export_binding(report, item, note) {
            export = export.on_action(binding);
        }
        let mut buttons: Vec<Button> = Vec::new();
        if let Some(item) = item
            && let StoreSource::Tilo { registry_name } = &item.source
        {
            if is_case {
                for (action, label, type_id, reason) in [
                    (
                        "refresh-status",
                        "Refresh status",
                        "store.moderation-status",
                        "",
                    ),
                    ("appeal", "Appeal decision", "store.moderation-appeal", note),
                ] {
                    let payload = (registry_name.clone(), report.id.clone(), reason.to_string());
                    let mut button = Button::new(&format!("{}.{action}", report.id), label, None);
                    if let Some(spec) = moderation_case_spec(action, label, type_id, &payload) {
                        let resource = ui::tilo_actions::Resource::new(
                            report.id.clone(),
                            "store.moderation-case",
                            report.app_id.clone(),
                        )
                        .with_action(spec);
                        if let Ok(binding) =
                            ui::tilo_actions::ActionBinding::unit_for(&resource, action)
                        {
                            button = button.on_action(binding);
                        }
                    }
                    buttons.push(button);
                }
            } else {
                let mut submit =
                    Button::new(&format!("{}.submit", report.id), "Submit report", None)
                        .role(ButtonRole::Primary)
                        .tooltip("Sign and submit this report to the authoritative registry");
                if let Some(binding) = report_submit_binding(report, item, note, registry_name) {
                    submit = submit.on_action(binding);
                }
                buttons.push(submit);
            }
        }
        let kind = if is_case {
            "Moderation case"
        } else {
            "Report draft"
        };
        let name = item.map_or(report.app_id.as_str(), |item| item.name.as_str());
        let mut actions = Row::new()
            .with_gap(theme.tokens.spacing.s2)
            .with_justify(taffy::JustifyContent::End)
            .push(Box::new(export));
        for button in buttons {
            actions = actions.push(Box::new(button));
        }
        let field = Field::new(
            "What happened?",
            TextInput::new(vec![])
                .with_id(&format!("store.report.note.{}", report.app_id))
                .with_on_change_path(&report_note_key(&report.app_id))
                .multiline(true)
                .with_placeholder("Describe what happened"),
        )
        .with_help("Describe observable behavior and evidence another reviewer can verify.");
        Box::new(Section::titled(
            name,
            vec![
                Box::new(
                    Row::new()
                        .no_wrap()
                        .with_gap(theme.tokens.spacing.s2)
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

    // ---- flatpak remotes ----

    /// Flatpak repositories: the form that adds one and the list of those
    /// configured on this machine.
    pub(super) fn build_remotes(&self, theme: &Theme) -> Element {
        let mut add = Button::new("store.remote.add", "Add remote", None).role(ButtonRole::Primary);
        if let Some(binding) = remote_form_binding() {
            add = add.on_action(binding);
        }
        let mut refresh = Button::new("store.remote.refresh", "Refresh remotes", None);
        if let Some(binding) = remote_refresh_binding() {
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
                let queue = std::rc::Rc::clone(&self.queued_invocations);
                let binding = remote_remove_binding(remote);
                Box::new(
                    ListRow::new(format!("store.remote.{}", remote.name), &remote.title)
                        .icon(Icon::Globe)
                        .secondary(&remote.url)
                        .meta(if remote.disabled {
                            "Disabled"
                        } else {
                            "Enabled"
                        })
                        .action(Icon::Trash, "Remove remote", move || {
                            if let Some(invocation) =
                                binding.as_ref().and_then(|binding| binding.invoke(()).ok())
                            {
                                queue.borrow_mut().push(invocation);
                            }
                        }),
                ) as Element
            })
            .collect();
        let mut children: Vec<Element> = vec![
            Self::back_row("store.remotes.back"),
            Self::page_title("Flatpak remotes"),
        ];
        children.extend(self.operation_notice(theme));
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

/// What a trust claim does and does not promise.
fn trust_explanation(item: &StoreItem, locally_retained: bool) -> &'static str {
    if locally_retained {
        return "Verified when installed. Current registry metadata is unavailable, so update availability is unknown.";
    }
    match item.trust {
        StoreTrust::PublisherSigned => {
            "Publisher identity and package integrity are verified. This is not an independent quality review."
        }
        StoreTrust::Flathub => "Tilo has not independently reviewed this app.",
    }
}

/// Every fact the details page lists, in one reading order: what the package
/// is, where it comes from, what it is doing on this device and where the
/// project lives.
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
    let mut rows = vec![
        Property::new("App ID", &item.id)
            .with_kind(TextKind::Identifier)
            .copyable(),
    ];
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
        rows.push(Property::new("Latest version", &version.version).with_kind(TextKind::Numeric));
        if version.size_bytes > 0 {
            rows.push(
                Property::new("Download size", format_size(version.size_bytes))
                    .with_kind(TextKind::Numeric),
            );
        }
        if let Some(hash) = version.root_hash_hex.as_deref() {
            rows.push(
                Property::new("Package digest", digest_summary(hash)).with_kind(TextKind::Secret),
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
    };
    let mut rows = vec![Property::new("Catalogue", source)];
    match item.publisher_pubkey_hex.as_deref() {
        Some(key) => {
            let full = format!("ed25519:{key}");
            rows.push(
                Property::new("Publisher", short_publisher(item))
                    .with_kind(TextKind::Identifier)
                    .action(Icon::Copy, "Copy publisher key", move || {
                        ui::text_selection::clipboard_copy(&full);
                    }),
            );
        }
        None => rows.push(Property::new("Publisher", full_publisher(item))),
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
        (StoreSource::Flathub, InstalledState::Not) => {
            vec![("On this device", "Not installed".into())]
        }
        (
            StoreSource::Flathub,
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
