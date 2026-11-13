//! Your data: the discovered folders, apps and sign-ins, as the tree of choices a
//! person makes about them.

use std::collections::BTreeSet;
use std::rc::Rc;

use super::browser::Domain;

use ui::icons::Icon;
use ui::tokens::{Rhythm, TextRole};
use ui::widget::{Checkbox, Column, Element, ListRow, Text};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Toggle {
    All,
    None,
    Mixed,
}

impl Toggle {
    pub(super) fn of(selected: usize, total: usize) -> Self {
        if total == 0 || selected == 0 {
            Self::None
        } else if selected == total {
            Self::All
        } else {
            Self::Mixed
        }
    }

    fn checked(self) -> bool {
        self == Self::All
    }

    fn mixed(self) -> bool {
        self == Self::Mixed
    }
}

pub(super) struct Item {
    pub(super) id: String,
    pub(super) test_id: String,
    pub(super) label: String,
    pub(super) detail: Option<String>,
    pub(super) note: Option<String>,
    pub(super) eligible: bool,
    pub(super) included: bool,
    pub(super) private_label: Option<String>,
}

pub(super) struct Group {
    pub(super) id: String,
    pub(super) test_id: String,
    pub(super) label: String,
    pub(super) summary: String,
    pub(super) detail: Option<String>,
    pub(super) note: Option<String>,
    pub(super) items: Vec<Item>,
    pub(super) groups: Vec<Group>,
    pub(super) state: Option<Toggle>,
    pub(super) selectable: bool,
    pub(super) private_label: Option<String>,
}

impl Group {
    fn has_children(&self) -> bool {
        !self.items.is_empty() || !self.groups.is_empty()
    }

    fn counts(&self) -> (usize, usize) {
        let mut selected = 0;
        let mut total = 0;
        for item in self.items.iter().filter(|item| item.eligible) {
            total += 1;
            selected += usize::from(item.included);
        }
        for group in &self.groups {
            let (group_selected, group_total) = group.counts();
            selected += group_selected;
            total += group_total;
        }
        (selected, total)
    }

    fn toggle(&self) -> Toggle {
        if let Some(state) = self.state {
            return state;
        }
        let (selected, total) = self.counts();
        Toggle::of(selected, total)
    }
}

pub(super) struct Source {
    pub(super) id: String,
    pub(super) test_id: String,
    pub(super) label: String,
    pub(super) icon: Icon,
    pub(super) summary: String,
    pub(super) detail: Option<String>,
    pub(super) note: Option<String>,
    pub(super) items: Vec<Item>,
    pub(super) groups: Vec<Group>,
    pub(super) state: Option<Toggle>,
    pub(super) selectable: bool,
    pub(super) included: bool,
}

impl Source {
    fn counts(&self) -> (usize, usize) {
        let mut selected = 0;
        let mut total = 0;
        for item in self.items.iter().filter(|item| item.eligible) {
            total += 1;
            selected += usize::from(item.included);
        }
        for group in &self.groups {
            let (group_selected, group_total) = group.counts();
            selected += group_selected;
            total += group_total;
        }
        (selected, total)
    }

    fn toggle(&self) -> Toggle {
        if let Some(state) = self.state {
            return state;
        }
        let (selected, total) = self.counts();
        if total == 0 {
            return if self.included {
                Toggle::All
            } else {
                Toggle::None
            };
        }
        Toggle::of(selected, total)
    }
}

pub(super) struct Inventory {
    pub(super) sources: Vec<Source>,
}

impl Inventory {
    pub(super) fn counts(&self) -> (usize, usize) {
        let mut selected = 0;
        let mut total = 0;
        for source in self.sources.iter().filter(|source| source.selectable) {
            let (source_selected, source_total) = source.counts();
            selected += source_selected;
            total += source_total;
        }
        (selected, total)
    }

    pub(super) fn toggle(&self) -> Toggle {
        let (selected, total) = self.counts();
        Toggle::of(selected, total)
    }

    pub(super) fn source(&self, id: &str) -> Option<&Source> {
        self.sources.iter().find(|source| source.id == id)
    }

    pub(super) fn group(&self, id: &str) -> Option<&Group> {
        fn find<'a>(groups: &'a [Group], id: &str) -> Option<&'a Group> {
            for group in groups {
                if group.id == id {
                    return Some(group);
                }
                if let Some(found) = find(&group.groups, id) {
                    return Some(found);
                }
            }
            None
        }
        self.sources
            .iter()
            .find_map(|source| find(&source.groups, id))
    }

    pub(super) fn item(&self, id: &str) -> Option<&Item> {
        fn find<'a>(groups: &'a [Group], id: &str) -> Option<&'a Item> {
            for group in groups {
                if let Some(item) = group.items.iter().find(|item| item.id == id) {
                    return Some(item);
                }
                if let Some(found) = find(&group.groups, id) {
                    return Some(found);
                }
            }
            None
        }
        self.sources.iter().find_map(|source| {
            source
                .items
                .iter()
                .find(|item| item.id == id)
                .or_else(|| find(&source.groups, id))
        })
    }

    pub(super) fn report(&self) -> serde_json::Value {
        let source_report = |source: &Source| {
            let (selected, total) = source.counts();
            serde_json::json!({
                "id": source.id,
                "label": source.label,
                "selectable": source.selectable,
                "included": source.toggle().checked(),
                "mixed": source.toggle().mixed(),
                "selected": selected,
                "available": total,
                "detail": source.detail,
                "note": source.note,
                "items": source.items.iter().map(item_report).collect::<Vec<_>>(),
                "groups": source.groups.iter().map(group_report).collect::<Vec<_>>(),
            })
        };
        let (selected, total) = self.counts();
        serde_json::json!({
            "state": match self.toggle() {
                Toggle::All => "all",
                Toggle::None => "none",
                Toggle::Mixed => "mixed",
            },
            "selected": selected,
            "available": total,
            "sources": self.sources.iter().map(source_report).collect::<Vec<_>>(),
        })
    }

    pub(super) fn view(&self, expanded: &BTreeSet<String>, bind: &Bindings) -> Element {
        let (selected, total) = self.counts();
        let state = self.toggle();
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(Rhythm::Group)
            .push(Box::new(
                Text::new(&super::t("installer-your-data-heading"))
                    .with_role(TextRole::Subheading)
                    .with_heading_level(2),
            ))
            .push(Box::new(
                bind.bound(
                    Checkbox::from_state(&ui::LocalState::new(state.checked()))
                        .indeterminate(state.mixed())
                        .with_id("installer.your-data.all")
                        .with_label(super::t_args(
                            "installer-your-data-all",
                            &[
                                ("selected", selected.to_string().into()),
                                ("total", total.to_string().into()),
                            ],
                        )),
                    super::InstallerPreviewEffect::SET_YOUR_DATA_ALL,
                ),
            ));
        for source in &self.sources {
            body = body.push(source_view(source, expanded, bind));
        }
        Box::new(body)
    }
}

fn group_report(group: &Group) -> serde_json::Value {
    let (selected, total) = group.counts();
    serde_json::json!({
        "id": group.id,
        "label": group.label,
        "detail": group.detail,
        "note": group.note,
        "selectable": group.selectable,
        "selected": selected,
        "available": total,
        "items": group.items.iter().map(item_report).collect::<Vec<_>>(),
        "groups": group.groups.iter().map(group_report).collect::<Vec<_>>(),
    })
}

fn item_report(item: &Item) -> serde_json::Value {
    serde_json::json!({
        "id": item.id,
        "label": item.label,
        "detail": item.detail,
        "note": item.note,
        "eligible": item.eligible,
        "included": item.included,
    })
}

fn source_view(source: &Source, expanded: &BTreeSet<String>, bind: &Bindings) -> Element {
    let state = source.toggle();
    let (selected, total) = source.counts();
    let id = source.id.clone();
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Related);
    let summary = if source.state.is_some() {
        String::new()
    } else if total > 0 {
        super::t_args(
            "installer-your-data-selected-count",
            &[
                ("selected", selected.to_string().into()),
                ("total", total.to_string().into()),
            ],
        )
    } else {
        source.summary.clone()
    };
    let name = source.label.as_str();
    let label = if summary.is_empty() {
        source.label.clone()
    } else {
        format!("{name} ({summary})")
    };
    column = column.push(Box::new(
        bind.bound(
            Checkbox::from_state(&ui::LocalState::new(state.checked()))
                .indeterminate(state.mixed())
                .with_enabled(source.selectable)
                .with_id(&source.test_id)
                .with_label(label),
            super::InstallerPreviewEffect::SET_YOUR_DATA_SOURCE
                .map(move |included: bool| (id.clone(), included)),
        ),
    ));
    if let Some(detail) = &source.detail {
        column = column.push(Box::new(Text::new(detail).with_role(TextRole::Caption)));
    }
    if let Some(note) = &source.note {
        column = column.push(Box::new(Text::new(note).with_role(TextRole::Caption)));
    }
    let open = expanded.contains(&source.id);
    let disclosure = bind.expanded.clone();
    let disclosure_id = source.id.clone();
    column = column.push(Box::new(
        ListRow::new(open_id(source), source.label.clone())
            .secondary(summary)
            .two_line_secondary()
            .icon(source.icon)
            .disclosure()
            .on_view(move || disclosure(disclosure_id.clone(), !open)),
    ));
    if !open {
        return Box::new(column);
    }
    for item in &source.items {
        column = column.push(item_view(item, bind));
    }
    for group in &source.groups {
        column = column.push(group_view(group, expanded, bind));
    }
    Box::new(column)
}

fn open_id(source: &Source) -> String {
    format!("installer.your-data.open.{}", slug(&source.id))
}

fn group_view(group: &Group, expanded: &BTreeSet<String>, bind: &Bindings) -> Element {
    let state = group.toggle();
    let (selected, total) = group.counts();
    let id = group.id.clone();
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Related);
    let summary = if group.state.is_some() {
        String::new()
    } else if total > 0 {
        super::t_args(
            "installer-your-data-selected-count",
            &[
                ("selected", selected.to_string().into()),
                ("total", total.to_string().into()),
            ],
        )
    } else {
        group.summary.clone()
    };
    let name = group.private_label.as_deref().unwrap_or(&group.label);
    let label = if summary.is_empty() {
        name.to_string()
    } else {
        format!("{name} ({summary})")
    };
    column = column.push(Box::new(
        bind.bound(
            Checkbox::from_state(&ui::LocalState::new(state.checked()))
                .indeterminate(state.mixed())
                .with_enabled(group.selectable)
                .with_id(&group.test_id)
                .with_label(label),
            super::InstallerPreviewEffect::SET_YOUR_DATA_GROUP
                .map(move |included: bool| (id.clone(), included)),
        ),
    ));
    if let Some(detail) = &group.detail {
        column = column.push(Box::new(Text::new(detail).with_role(TextRole::Caption)));
    }
    if let Some(note) = &group.note {
        column = column.push(Box::new(Text::new(note).with_role(TextRole::Caption)));
    }
    let open = expanded.contains(&group.id);
    if group.has_children() {
        let disclosure = bind.expanded.clone();
        let disclosure_id = group.id.clone();
        column = column.push(Box::new(
            ListRow::new(
                format!("installer.your-data.open.{}", slug(&group.id)),
                name.to_string(),
            )
            .secondary(summary)
            .two_line_secondary()
            .disclosure()
            .on_view(move || disclosure(disclosure_id.clone(), !open)),
        ));
    }
    if !open || !group.has_children() {
        return Box::new(column);
    }
    for item in &group.items {
        column = column.push(item_view(item, bind));
    }
    for nested in &group.groups {
        column = column.push(group_view(nested, expanded, bind));
    }
    Box::new(column)
}

fn item_view(item: &Item, bind: &Bindings) -> Element {
    let name = item.private_label.as_deref().unwrap_or(&item.label);
    if !item.eligible {
        let mut row = ListRow::new(item.test_id.clone(), name.to_string());
        if let Some(detail) = &item.detail {
            row = row.secondary(detail.clone()).two_line_secondary();
        } else if let Some(note) = &item.note {
            row = row.secondary(note.clone()).two_line_secondary();
        }
        return Box::new(row);
    }
    let id = item.id.clone();
    let label = match &item.detail {
        Some(detail) => format!("{name} ({detail})"),
        None => name.to_string(),
    };
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Tight)
        .push(Box::new(
            bind.bound(
                Checkbox::from_state(&ui::LocalState::new(item.included))
                    .with_id(&item.test_id)
                    .with_label(label),
                super::InstallerPreviewEffect::SET_YOUR_DATA_ITEM
                    .map(move |included: bool| (id.clone(), included)),
            ),
        ));
    if let Some(note) = &item.note {
        column = column.push(Box::new(Text::new(note).with_role(TextRole::Caption)));
    }
    Box::new(column)
}

pub(super) fn slug(value: &str) -> String {
    let mut slug = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

#[derive(Clone)]
pub(super) struct Bindings {
    pub(super) actions: Option<ui::framework::Actions<crate::resource::Imager>>,
    pub(super) expanded: Rc<dyn Fn(String, bool)>,
}

impl Bindings {
    fn bound(
        &self,
        checkbox: Checkbox,
        effect: impl ui::tilo_resource::Binds<super::InstallerPreviewEffect, bool>,
    ) -> Checkbox {
        super::bound(self.actions.as_ref(), checkbox, effect)
    }
}

pub(super) fn size_text(bytes: u64) -> Option<String> {
    if bytes == 0 {
        return None;
    }
    const UNITS: [(&str, u64); 4] = [
        ("GB", 1_000_000_000),
        ("MB", 1_000_000),
        ("kB", 1_000),
        ("B", 1),
    ];
    let (unit, scale) = UNITS
        .into_iter()
        .find(|(_, scale)| bytes >= *scale)
        .unwrap_or(("B", 1));
    let value = if scale == 1 {
        bytes.to_string()
    } else {
        let tenths = bytes.saturating_mul(10) / scale;
        let whole = tenths / 10;
        if tenths.is_multiple_of(10) {
            format!("{whole}")
        } else {
            let fraction = tenths % 10;
            format!("{whole}.{fraction}")
        }
    };
    Some(format!("{value} {unit}"))
}

pub(super) fn node_label(
    label: imager_backend::setup::source_tree::Label,
    title: Option<&str>,
) -> String {
    use imager_backend::setup::source_tree::Label;
    if let Some(title) = title.filter(|title| !title.is_empty()) {
        return title.to_string();
    }
    let key = match label {
        Label::KnownFolder(folder) => folder_key(folder),
        Label::Cloud(cloud) => cloud_key(cloud),
        Label::Steam => "installer-your-data-folder-steam",
        Label::SteamLibrary => "installer-your-data-steam-library",
        Label::SteamUserdata => "installer-your-data-steam-userdata",
        Label::ProtonPrefix => "installer-your-data-proton-prefix",
        Label::Game => "installer-your-data-game",
        Label::GameAssets => "installer-your-data-game-assets",
        Label::GameSaves => "installer-your-data-game-saves",
        Label::Outlook => "installer-your-data-folder-outlook-files",
        Label::Folder | Label::Other => "installer-your-data-folder-other",
        Label::Files => "installer-your-data-files-leaf",
    };
    super::t(key)
}

fn folder_key(folder: imager_backend::setup::source_tree::KnownFolder) -> &'static str {
    use imager_backend::setup::source_tree::KnownFolder;
    match folder {
        KnownFolder::Documents => "installer-your-data-folder-documents",
        KnownFolder::Desktop => "installer-your-data-folder-desktop",
        KnownFolder::Downloads => "installer-your-data-folder-downloads",
        KnownFolder::Pictures => "installer-your-data-folder-pictures",
        KnownFolder::Music => "installer-your-data-folder-music",
        KnownFolder::Videos => "installer-your-data-folder-videos",
        KnownFolder::SavedGames => "installer-your-data-folder-saved-games",
        KnownFolder::AppData => "installer-your-data-folder-appdata",
        KnownFolder::OutlookFiles => "installer-your-data-folder-outlook-files",
        KnownFolder::Steam => "installer-your-data-folder-steam",
        KnownFolder::OneDrive => "installer-your-data-folder-onedrive",
        KnownFolder::Nextcloud => "installer-your-data-folder-nextcloud",
        KnownFolder::Dropbox => "installer-your-data-folder-dropbox",
        KnownFolder::GoogleDrive => "installer-your-data-folder-google-drive",
        KnownFolder::OwnCloud => "installer-your-data-folder-owncloud",
    }
}

fn cloud_key(cloud: imager_backend::setup::source_tree::CloudProvider) -> &'static str {
    use imager_backend::setup::source_tree::CloudProvider;
    match cloud {
        CloudProvider::OneDrive => "installer-your-data-folder-onedrive",
        CloudProvider::Nextcloud => "installer-your-data-folder-nextcloud",
        CloudProvider::Dropbox => "installer-your-data-folder-dropbox",
        CloudProvider::GoogleDrive => "installer-your-data-folder-google-drive",
        CloudProvider::OwnCloud => "installer-your-data-folder-owncloud",
    }
}

pub(super) fn node_detail(counts: &imager_backend::setup::source_tree::Counts) -> Option<String> {
    size_text(counts.local_bytes)
}

pub(super) fn fact_notes(facts: &[imager_backend::setup::source_tree::Fact]) -> Option<String> {
    use imager_backend::setup::source_tree::Fact;
    let mut notes = Vec::new();
    for fact in facts {
        let key = match fact {
            Fact::OnlineOnlyNotCopied => "installer-your-data-online-only-note",
            Fact::UnknownSize => "installer-your-data-unknown-size-note",
            Fact::CoverageIncomplete => "installer-your-data-incomplete-note",
            Fact::NeedsIndependentGrant => "installer-your-data-needs-grant-note",
            Fact::NotCopyableAtSource => "installer-your-data-not-copyable-note",
            Fact::DownloadBeforeCopy => "installer-your-data-download-note",
            Fact::ProviderConnectionUnavailable => "installer-your-data-provider-unavailable-note",
            Fact::GameAssets => "installer-your-data-game-assets-note",
            Fact::IrreplaceableSaves => "installer-your-data-saves-note",
            Fact::SaveLocationUnknown => "installer-your-data-save-location-unknown-note",
            Fact::SteamCloudUnverified => "installer-your-data-steam-cloud-note",
            Fact::ExternalSaveLocation(_) => "installer-your-data-external-save-note",
            Fact::RemoteCoverageUnknown => "installer-your-data-remote-coverage-note",
            Fact::ProtonPrefix => "installer-your-data-proton-prefix-note",
            Fact::CloudProvider(_) | Fact::AlreadyIncluded { .. } => continue,
        };
        if !notes.contains(&key) {
            notes.push(key);
        }
    }
    if notes.is_empty() {
        return None;
    }
    Some(
        notes
            .into_iter()
            .take(3)
            .map(super::t)
            .collect::<Vec<_>>()
            .join(" "),
    )
}

pub(super) fn count_phrase(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        super::t(one)
    } else {
        super::t_args(many, &[("count", count.into())])
    }
}

pub(super) struct TreeAvailability {
    pub(super) selectable: bool,
    pub(super) included: bool,
    pub(super) summary: String,
    pub(super) note: Option<String>,
}

pub(super) fn tree_source(
    selection: &super::source_selection::SourceSelection,
    availability: TreeAvailability,
) -> Source {
    let mut items = Vec::new();
    let mut groups = Vec::new();
    if let Some(tree) = selection.tree() {
        project_children(&tree.sources, selection, &mut items, &mut groups);
    }
    let state = if availability.included && selection.ready() {
        None
    } else if availability.included {
        Some(Toggle::All)
    } else {
        Some(Toggle::None)
    };
    Source {
        id: "files".into(),
        test_id: "installer.your-data.source.files".into(),
        label: super::t("installer-your-data-files"),
        icon: Icon::Folder,
        summary: availability.summary,
        detail: selection
            .counts()
            .and_then(|counts| size_text(counts.local_bytes)),
        note: availability.note,
        items,
        groups,
        state,
        selectable: availability.selectable,
        included: availability.included,
    }
}

fn project_children(
    nodes: &[imager_backend::setup::source_tree::Node],
    selection: &super::source_selection::SourceSelection,
    items: &mut Vec<Item>,
    groups: &mut Vec<Group>,
) {
    for (index, node) in nodes.iter().enumerate() {
        let label = display_label(node, index.saturating_add(1));
        if node.children.is_empty() {
            items.push(tree_item(node, selection, label));
        } else {
            groups.push(tree_group(node, selection, label));
        }
    }
}

fn display_label(node: &imager_backend::setup::source_tree::Node, ordinal: usize) -> String {
    use imager_backend::setup::source_tree::Label;
    let label = node_label(node.label, node.title.as_deref());
    if node.title.is_none() && matches!(node.label, Label::Folder | Label::Other) {
        format!("{label} {ordinal}")
    } else {
        label
    }
}

fn private_label(node: &imager_backend::setup::source_tree::Node) -> Option<String> {
    use imager_backend::setup::source_tree::Label;
    if node.title.is_some()
        || !matches!(
            node.label,
            Label::Folder | Label::Other | Label::Game | Label::Steam | Label::SteamLibrary
        )
    {
        return None;
    }
    node.private_label().map(str::to_owned)
}

fn tree_item(
    node: &imager_backend::setup::source_tree::Node,
    selection: &super::source_selection::SourceSelection,
    label: String,
) -> Item {
    use imager_backend::setup::source_tree::SelectionState;
    let state = selection.node_state(&node.id);
    Item {
        private_label: private_label(node),
        id: format!("tree|{}", node.id),
        test_id: format!("installer.your-data.tree.{}", slug(&node.id)),
        label,
        detail: node_detail(&node.counts),
        note: fact_notes(&node.facts),
        eligible: node.selectable && state != Some(SelectionState::Unavailable),
        included: node.selectable && state != Some(SelectionState::Excluded),
    }
}

fn tree_group(
    node: &imager_backend::setup::source_tree::Node,
    selection: &super::source_selection::SourceSelection,
    label: String,
) -> Group {
    let mut items = Vec::new();
    let mut groups = Vec::new();
    project_children(&node.children, selection, &mut items, &mut groups);
    Group {
        private_label: private_label(node),
        id: format!("tree|{}", node.id),
        test_id: format!("installer.your-data.tree.{}", slug(&node.id)),
        label,
        summary: String::new(),
        detail: node_detail(&node.counts),
        note: fact_notes(&node.facts),
        items,
        groups,
        state: None,
        selectable: node.selectable,
    }
}

pub(super) fn browser_sources(belongings: &super::belongings::Belongings) -> Vec<Source> {
    use imager_backend::browser_inventory::ProfileInventory;
    let browser = &belongings.browser;
    if belongings.browser_unavailable {
        return Vec::new();
    }
    let detected = belongings.sessions.detected_sources().collect::<Vec<_>>();
    if !browser.available() && detected.is_empty() {
        return Vec::new();
    }
    let mut applications = std::collections::BTreeMap::<String, Vec<&ProfileInventory>>::new();
    for profile in browser.inventory() {
        applications
            .entry(profile.application.clone())
            .or_default()
            .push(profile);
    }
    let mut by_profile =
        std::collections::BTreeMap::<&str, Vec<&super::service_sessions::DetectedSource<'_>>>::new(
        );
    let mut unmatched = Vec::new();
    for entry in &detected {
        match entry.source.browser_profile_id() {
            Some(profile) if browser.profile_state(profile).is_some() => {
                by_profile.entry(profile).or_default().push(entry);
            }
            _ => unmatched.push(entry),
        }
    }
    let mut sources = Vec::new();
    for (application, profiles) in applications {
        let mut groups = Vec::new();
        for profile in &profiles {
            let state = browser.profile_state(&profile.id).unwrap_or_default();
            let mut items = Vec::new();
            for domain in [Domain::Bookmarks, Domain::History] {
                if let Some(item) = profile_domain_item(profile, domain, state) {
                    items.push(item);
                }
            }
            if let Some(entries) = by_profile.get(profile.id.as_str()) {
                items.extend(
                    entries
                        .iter()
                        .map(|entry| sign_in_item(entry, Some(state.included))),
                );
            }
            if let Some(accounts) = profile
                .websites
                .accounts
                .exact()
                .filter(|accounts| *accounts > 0)
            {
                items.push(credentials_item(profile, accounts));
            }
            groups.push(profile_group(profile, items));
        }
        let mut source = Source {
            id: format!("browser|{application}"),
            test_id: format!("installer.your-data.source.{}", slug(&application)),
            label: application,
            icon: Icon::Globe,
            summary: String::new(),
            detail: Some(count_phrase(
                profiles.len(),
                "installer-your-data-browser-one-profile",
                "installer-your-data-browser-profiles",
            )),
            note: Some(super::t("installer-your-data-browser-note")),
            items: Vec::new(),
            groups,
            state: None,
            selectable: false,
            included: false,
        };
        source.selectable = source.groups.iter().any(|group| group.selectable);
        source.included = source.toggle() != Toggle::None;
        sources.push(source);
    }
    let mut leftovers = std::collections::BTreeMap::<
        String,
        std::collections::BTreeMap<String, Vec<&super::service_sessions::DetectedSource<'_>>>,
    >::new();
    for entry in unmatched {
        leftovers
            .entry(entry.source.application.clone())
            .or_default()
            .entry(entry.source.id.clone())
            .or_default()
            .push(entry);
    }
    for (application, grouped) in leftovers {
        let mut groups = Vec::new();
        for (id, entries) in grouped {
            let items = entries
                .iter()
                .map(|entry| sign_in_item(entry, None))
                .collect::<Vec<_>>();
            groups.push(Group {
                private_label: None,
                id: format!("sessions|{id}"),
                test_id: format!("installer.your-data.group.{}", slug(&id)),
                label: entries[0].source.label.clone(),
                summary: String::new(),
                detail: None,
                note: None,
                items,
                groups: Vec::new(),
                state: None,
                selectable: entries.iter().any(|entry| entry.selectable),
            });
        }
        let existing = sources
            .iter()
            .position(|source| source.id == format!("browser|{application}"));
        if let Some(index) = existing {
            sources[index].groups.extend(groups);
            sources[index].selectable = true;
            let selected = sources[index].toggle() != Toggle::None;
            sources[index].included |= selected;
        } else {
            let mut source = Source {
                id: format!("browser|{application}"),
                test_id: format!("installer.your-data.source.{}", slug(&application)),
                label: application,
                icon: Icon::Globe,
                summary: String::new(),
                detail: None,
                note: None,
                items: Vec::new(),
                groups,
                state: None,
                selectable: false,
                included: false,
            };
            source.selectable = source.groups.iter().any(|group| group.selectable);
            source.included = source.toggle() != Toggle::None;
            sources.push(source);
        }
    }
    sources
}

pub(super) fn account_sources(
    inventory: &imager_backend::account_sources::Inventory,
    selection: &super::accounts::SourceAccountSelection,
    included: bool,
) -> Vec<Source> {
    use super::accounts::{SourceAccountScope, can_connect_candidate};
    use imager_backend::account_sources::Origin;

    let recognized = |candidate: &&imager_backend::account_sources::Candidate| {
        candidate.origin != Origin::Browser
            && !super::accounts::source_account_services(candidate).is_empty()
            && (candidate.origin == Origin::Gnome
                || candidate.can_import_credentials
                || can_connect_candidate(candidate))
    };
    let mut sources = Vec::new();

    let gnome = inventory
        .candidates
        .iter()
        .filter(|candidate| candidate.origin == Origin::Gnome)
        .filter(recognized)
        .collect::<Vec<_>>();
    if gnome.is_empty() {
        sources.push(Source {
            id: "accounts".into(),
            test_id: "installer.your-data.source.accounts".into(),
            label: super::t("installer-your-data-online-accounts"),
            icon: Icon::Users,
            summary: super::t("installer-your-data-no-accounts"),
            detail: None,
            note: None,
            items: Vec::new(),
            groups: Vec::new(),
            state: Some(if included { Toggle::All } else { Toggle::None }),
            selectable: true,
            included,
        });
    } else {
        sources.push(account_source(
            &SourceAccountScope::Gnome,
            super::t("installer-your-data-online-accounts"),
            Icon::Users,
            &gnome,
            selection,
            included,
        ));
    }

    let nextcloud = inventory
        .candidates
        .iter()
        .filter(|candidate| candidate.origin == Origin::Nextcloud)
        .filter(recognized)
        .collect::<Vec<_>>();
    if !nextcloud.is_empty() {
        sources.push(account_source(
            &SourceAccountScope::Nextcloud,
            super::t("installer-your-data-provider-nextcloud"),
            Icon::Cloud,
            &nextcloud,
            selection,
            included,
        ));
    }

    let rclone = inventory
        .candidates
        .iter()
        .filter(|candidate| candidate.origin == Origin::Rclone)
        .filter(recognized)
        .collect::<Vec<_>>();
    if !rclone.is_empty() {
        sources.push(account_source(
            &SourceAccountScope::Rclone,
            super::t("installer-your-data-source-rclone"),
            Icon::Cloud,
            &rclone,
            selection,
            included,
        ));
    }

    let mut applications = std::collections::BTreeMap::<
        String,
        Vec<&imager_backend::account_sources::Candidate>,
    >::new();
    for candidate in inventory
        .candidates
        .iter()
        .filter(|candidate| candidate.origin == Origin::Application)
        .filter(recognized)
    {
        applications
            .entry(candidate.label.clone())
            .or_default()
            .push(candidate);
    }
    for (label, candidates) in applications {
        sources.push(account_source(
            &SourceAccountScope::Application(label.clone()),
            label,
            Icon::Cloud,
            &candidates,
            selection,
            included,
        ));
    }

    if !inventory.tilo_accounts.is_empty() {
        sources.push(Source {
            id: SourceAccountScope::Tilo.id(),
            test_id: format!(
                "installer.your-data.source.{}",
                slug(&SourceAccountScope::Tilo.id())
            ),
            label: super::t("installer-your-data-provider-tilo"),
            icon: Icon::Users,
            summary: String::new(),
            detail: None,
            note: None,
            items: inventory
                .tilo_accounts
                .iter()
                .map(|account| Item {
                    private_label: None,
                    id: format!("tilo|{}", account.source_id),
                    test_id: format!("installer.your-data.account.{}", slug(&account.source_id)),
                    label: super::accounts::tilo_candidate_label(account),
                    detail: Some(super::t("installer-your-data-account-tilo")),
                    note: None,
                    eligible: false,
                    included: false,
                })
                .collect(),
            groups: Vec::new(),
            state: Some(if included { Toggle::All } else { Toggle::None }),
            selectable: false,
            included: false,
        });
    }

    sources
}

fn account_source(
    scope: &super::accounts::SourceAccountScope,
    label: String,
    icon: Icon,
    candidates: &[&imager_backend::account_sources::Candidate],
    selection: &super::accounts::SourceAccountSelection,
    included: bool,
) -> Source {
    let groups = candidates
        .iter()
        .map(|candidate| account_group(candidate, selection, included))
        .collect::<Vec<_>>();
    let selected = groups.iter().any(|group| group.toggle() != Toggle::None);
    Source {
        id: scope.id(),
        test_id: format!("installer.your-data.source.{}", slug(&scope.id())),
        label,
        icon,
        summary: super::t("installer-your-data-no-accounts"),
        detail: Some(count_phrase(
            candidates.len(),
            "installer-your-data-account-one",
            "installer-your-data-account-count",
        )),
        note: matches!(scope, super::accounts::SourceAccountScope::Gnome)
            .then(|| super::t("installer-your-data-accounts-note")),
        items: Vec::new(),
        groups,
        state: if included { None } else { Some(Toggle::None) },
        selectable: true,
        included: included && selected,
    }
}

fn account_group(
    candidate: &imager_backend::account_sources::Candidate,
    selection: &super::accounts::SourceAccountSelection,
    included: bool,
) -> Group {
    let services = super::accounts::source_account_services(candidate);
    let items = services
        .iter()
        .map(|service| Item {
            private_label: None,
            id: format!("accountservice|{}|{}", candidate.id, service_key(*service)),
            test_id: format!(
                "installer.your-data.account-service.{}.{}",
                slug(&candidate.id),
                service_key(*service)
            ),
            label: super::t(service_label(*service)),
            detail: None,
            note: None,
            eligible: included,
            included: included && selection.includes_service(candidate, *service),
        })
        .collect::<Vec<_>>();
    Group {
        private_label: None,
        id: format!("account|{}", candidate.id),
        test_id: format!("installer.your-data.account.{}", slug(&candidate.id)),
        label: super::accounts::candidate_label(candidate),
        summary: String::new(),
        detail: Some(count_phrase(
            services.len(),
            "installer-your-data-service-one",
            "installer-your-data-services",
        )),
        note: (!candidate.can_import_credentials
            && candidate.origin == imager_backend::account_sources::Origin::Gnome)
            .then(|| super::t("installer-your-data-account-sign-in")),
        items,
        groups: Vec::new(),
        state: if included && !services.is_empty() {
            None
        } else {
            Some(Toggle::None)
        },
        selectable: included && !services.is_empty(),
    }
}

fn service_label(service: imager_backend::account_sources::Service) -> &'static str {
    use imager_backend::account_sources::Service;
    match service {
        Service::Files => "installer-your-data-service-files",
        Service::Mail => "installer-your-data-service-mail",
        Service::Calendar => "installer-your-data-service-calendar",
        Service::Contacts => "installer-your-data-service-contacts",
    }
}

pub(super) fn service_key(service: imager_backend::account_sources::Service) -> &'static str {
    use imager_backend::account_sources::Service;
    match service {
        Service::Files => "files",
        Service::Mail => "mail",
        Service::Calendar => "calendar",
        Service::Contacts => "contacts",
    }
}

pub(super) fn service_from_key(key: &str) -> Option<imager_backend::account_sources::Service> {
    use imager_backend::account_sources::Service;
    match key {
        "files" => Some(Service::Files),
        "mail" => Some(Service::Mail),
        "calendar" => Some(Service::Calendar),
        "contacts" => Some(Service::Contacts),
        _ => None,
    }
}

fn profile_group(
    profile: &imager_backend::browser_inventory::ProfileInventory,
    items: Vec<Item>,
) -> Group {
    use imager_backend::browser_inventory::Measure;
    let counts = [profile.bookmarks, profile.history]
        .into_iter()
        .filter_map(Measure::exact)
        .sum::<u64>();
    let has_sessions = items.iter().any(|item| item.id.starts_with("session|"));
    let note = if !profile.exportable && !has_sessions {
        Some(super::t(
            "installer-your-data-browser-profile-not-exportable",
        ))
    } else if profile.exportable && profile.incomplete() {
        Some(super::t("installer-your-data-browser-incomplete"))
    } else {
        None
    };
    let selectable = profile.exportable || has_sessions;
    Group {
        private_label: None,
        id: format!("profile|{}", profile.id),
        test_id: format!("installer.your-data.profile.{}", slug(&profile.id)),
        label: profile.profile_label.clone(),
        summary: String::new(),
        detail: (counts > 0).then(|| {
            count_phrase(
                usize::try_from(counts).unwrap_or(usize::MAX),
                "installer-your-data-browser-one-saved",
                "installer-your-data-browser-saved",
            )
        }),
        note,
        items,
        groups: Vec::new(),
        state: None,
        selectable,
    }
}

fn profile_domain_item(
    profile: &imager_backend::browser_inventory::ProfileInventory,
    domain: Domain,
    state: imager_backend::browser_selection::ProfileState,
) -> Option<Item> {
    use imager_backend::browser_inventory::{Capability, Measure};
    if !profile.exportable || !state.exportable {
        return None;
    }
    let (capability, measure, selected, key, suffix) = match domain {
        Domain::Bookmarks => (
            profile.capabilities.bookmarks,
            profile.bookmarks,
            state.bookmarks,
            "installer-your-data-bookmarks",
            "bookmarks",
        ),
        Domain::History => (
            profile.capabilities.history,
            profile.history,
            state.history,
            "installer-your-data-history",
            "history",
        ),
    };
    if capability == Capability::Absent {
        return None;
    }
    let eligible = capability.exported();
    Some(Item {
        private_label: None,
        id: format!("profiledomain|{}|{suffix}", profile.id),
        test_id: format!("installer.your-data.profile-{suffix}.{}", slug(&profile.id)),
        label: super::t(key),
        detail: measure
            .exact()
            .filter(|count| *count > 0)
            .map(|count| count.to_string()),
        note: (measure == Measure::Unknown).then(|| super::t("installer-your-data-counts-unknown")),
        eligible,
        included: eligible && state.included && selected,
    })
}

fn credentials_item(
    profile: &imager_backend::browser_inventory::ProfileInventory,
    accounts: u64,
) -> Item {
    Item {
        private_label: None,
        id: format!("browseraccounts|{}", profile.id),
        test_id: format!("installer.your-data.browser-accounts.{}", slug(&profile.id)),
        label: super::t("installer-your-data-browser-saved-signins"),
        detail: Some(count_phrase(
            usize::try_from(accounts).unwrap_or(usize::MAX),
            "installer-your-data-browser-one-signin",
            "installer-your-data-browser-signins",
        )),
        note: Some(super::t("installer-your-data-browser-passwords-note")),
        eligible: false,
        included: false,
    }
}

fn sign_in_item(
    detected: &super::service_sessions::DetectedSource<'_>,
    profile_included: Option<bool>,
) -> Item {
    use imager_backend::browser_selection::Service;
    use imager_backend::service_selection::{Material, Presence};
    let profile_included = profile_included.unwrap_or(true);
    Item {
        private_label: None,
        id: format!(
            "session|{}|{}",
            detected.source.id,
            detected.service.as_str()
        ),
        test_id: format!(
            "installer.your-data.session.{}.{}",
            detected.service.as_str(),
            detected.source.id
        ),
        label: super::t(match detected.service {
            Service::Slack => "installer-your-data-signin-slack",
            Service::Discord => "installer-your-data-signin-discord",
        }),
        detail: Some(super::t(if detected.presence == Presence::NeedsAccess {
            "installer-your-data-signin-needs-access"
        } else if detected.source.material == Material::DiscordOrigin {
            "installer-your-data-signin-origin"
        } else {
            "installer-your-data-signin-saved"
        })),
        note: None,
        eligible: profile_included && detected.selectable,
        included: profile_included && detected.included,
    }
}

pub(super) fn mail_sources(belongings: &super::belongings::Belongings) -> Vec<Source> {
    belongings
        .thunderbird_profiles()
        .map(|profile| mail_source(belongings, profile))
        .collect()
}

fn mail_source(
    belongings: &super::belongings::Belongings,
    profile: &imager_backend::account_sources::ThunderbirdProfile,
) -> Source {
    let selection = belongings.thunderbird_selection(&profile.source_id);
    let selected = |account_id: &str| {
        selection.and_then(|selection| {
            selection
                .accounts
                .iter()
                .find(|account| account.source_id == account_id)
        })
    };
    let accounts_selected = selection.is_some_and(|selection| !selection.accounts.is_empty());
    let chosen_accounts = selection.map_or(0, |selection| selection.accounts.len());
    let mut groups = Vec::new();
    for account in &profile.accounts {
        let entry = selected(&account.source_id);
        let items = account
            .folders
            .iter()
            .map(|folder| Item {
                private_label: None,
                id: format!(
                    "mailfolder|{}|{}|{}",
                    profile.source_id, account.source_id, folder.source_id
                ),
                test_id: format!(
                    "installer.your-data.mail-folder.{}",
                    slug(&folder.source_id)
                ),
                label: folder_label(&folder.hierarchy),
                detail: folder.messages.map(|messages| messages.to_string()),
                note: (!folder.counted).then(|| super::t("installer-your-data-counts-unknown")),
                eligible: true,
                included: entry
                    .is_some_and(|entry| entry.folders.iter().any(|id| id == &folder.source_id)),
            })
            .collect::<Vec<_>>();
        let state = if entry.is_none() {
            Toggle::None
        } else if items.iter().all(|item| item.included) {
            Toggle::All
        } else {
            Toggle::Mixed
        };
        groups.push(Group {
            private_label: None,
            id: format!("mailaccount|{}|{}", profile.source_id, account.source_id),
            test_id: format!(
                "installer.your-data.mail-account.{}",
                slug(&account.source_id)
            ),
            label: if account.display_name.trim().is_empty() {
                super::t("installer-your-data-mail-account")
            } else {
                account.display_name.clone()
            },
            summary: String::new(),
            detail: Some(count_phrase(
                account.folders.len(),
                "installer-your-data-mail-one-folder",
                "installer-your-data-mail-folders",
            )),
            note: account
                .needs_reconnect
                .then(|| super::t("installer-your-data-mail-reconnect")),
            items,
            groups: Vec::new(),
            state: Some(state),
            selectable: true,
        });
    }
    let mut items = Vec::new();
    for book in profile.address_books.iter().filter(|book| book.present) {
        items.push(personal_item(
            &profile.source_id,
            selection,
            accounts_selected,
            Personal {
                kind: "mailbook",
                id: &book.source_id,
                name: &book.name,
                book: true,
            },
        ));
    }
    for calendar in profile.calendars.iter().filter(|calendar| calendar.present) {
        items.push(personal_item(
            &profile.source_id,
            selection,
            accounts_selected,
            Personal {
                kind: "mailcalendar",
                id: &calendar.source_id,
                name: &calendar.name,
                book: false,
            },
        ));
    }
    Source {
        id: format!("mail|{}", profile.source_id),
        test_id: format!(
            "installer.your-data.source.mail.{}",
            slug(&profile.source_id)
        ),
        label: profile.label.clone(),
        icon: Icon::Mail,
        summary: super::t("installer-your-data-no-accounts"),
        detail: Some(count_phrase(
            profile.accounts.len(),
            "installer-your-data-mail-one-account",
            "installer-your-data-mail-accounts",
        )),
        note: Some(super::t("installer-your-data-mail-local")),
        items,
        groups,
        state: Some(
            if accounts_selected && chosen_accounts >= profile.accounts.len() {
                Toggle::All
            } else if accounts_selected {
                Toggle::Mixed
            } else {
                Toggle::None
            },
        ),
        selectable: !profile.accounts.is_empty(),
        included: belongings.thunderbird_export(&profile.source_id).is_some(),
    }
}

struct Personal<'a> {
    kind: &'a str,
    id: &'a str,
    name: &'a str,
    book: bool,
}

fn personal_item(
    profile_id: &str,
    selection: Option<&imager_backend::account_transfer::ThunderbirdSelection>,
    accounts_selected: bool,
    item: Personal<'_>,
) -> Item {
    let Personal {
        kind,
        id,
        name,
        book,
    } = item;
    let included = selection.is_some_and(|selection| {
        selection.accounts.iter().any(|account| {
            let list = if book {
                &account.address_books
            } else {
                &account.calendars
            };
            list.iter().any(|entry| entry == id)
        })
    });
    Item {
        private_label: None,
        id: format!("{kind}|{profile_id}|{id}"),
        test_id: format!(
            "installer.your-data.{}.{}",
            if book { "mail-book" } else { "mail-calendar" },
            slug(id)
        ),
        label: name.to_string(),
        detail: None,
        note: (!accounts_selected).then(|| super::t("installer-your-data-mail-needs-account")),
        eligible: accounts_selected,
        included: accounts_selected && included,
    }
}

fn folder_label(hierarchy: &[String]) -> String {
    let label = hierarchy.join(" / ");
    if label.is_empty() {
        super::t("installer-your-data-mail-folder")
    } else {
        label
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(id: &str, eligible: bool, included: bool) -> Item {
        Item {
            private_label: None,
            id: id.into(),
            test_id: format!("test.{}", slug(id)),
            label: id.into(),
            detail: None,
            note: None,
            eligible,
            included,
        }
    }

    fn group(id: &str, items: Vec<Item>, groups: Vec<Group>) -> Group {
        Group {
            private_label: None,
            id: id.into(),
            test_id: format!("test.{}", slug(id)),
            label: id.into(),
            summary: String::new(),
            detail: None,
            note: None,
            items,
            groups,
            state: None,
            selectable: true,
        }
    }

    fn source(id: &str, items: Vec<Item>, groups: Vec<Group>) -> Source {
        Source {
            id: id.into(),
            test_id: format!("test.{}", slug(id)),
            label: id.into(),
            icon: Icon::Folder,
            summary: String::new(),
            detail: None,
            note: None,
            items,
            groups,
            state: None,
            selectable: true,
            included: false,
        }
    }

    #[test]
    fn mixed_states_come_only_from_eligible_children() {
        let inventory = Inventory {
            sources: vec![source(
                "s",
                vec![],
                vec![group(
                    "g",
                    vec![
                        leaf("a", true, true),
                        leaf("b", true, false),
                        leaf("c", false, false),
                    ],
                    vec![],
                )],
            )],
        };
        let found = inventory.group("g").unwrap();
        assert_eq!(found.counts(), (1, 2));
        assert_eq!(found.toggle(), Toggle::Mixed);
        assert_eq!(inventory.counts(), (1, 2));
        assert_eq!(inventory.toggle(), Toggle::Mixed);
        assert!(inventory.report()["sources"][0]["mixed"].as_bool().unwrap());
    }

    #[test]
    fn nested_children_reach_the_source_and_the_shared_toggle() {
        let inventory = Inventory {
            sources: vec![source(
                "files",
                vec![],
                vec![group(
                    "documents",
                    vec![],
                    vec![group("work", vec![leaf("plan", true, true)], vec![])],
                )],
            )],
        };
        assert!(inventory.group("work").is_some());
        assert!(inventory.item("plan").is_some());
        assert_eq!(inventory.counts(), (1, 1));
        assert_eq!(inventory.toggle(), Toggle::All);
        assert_eq!(
            inventory.report()["sources"][0]["groups"][0]["groups"][0]["selected"],
            1
        );
    }

    #[test]
    fn a_source_without_eligible_children_has_no_invented_count() {
        let inventory = Inventory {
            sources: vec![source("mail", vec![], vec![])],
        };
        assert_eq!(inventory.counts(), (0, 0));
        assert_eq!(inventory.toggle(), Toggle::None);
        let report = inventory.report();
        assert_eq!(report["selected"], 0);
        assert_eq!(report["available"], 0);
        assert_eq!(report["sources"][0]["available"], 0);
    }

    #[test]
    fn an_owned_decision_is_not_rederived_from_children() {
        let mut files = source(
            "files",
            vec![],
            vec![group("documents", vec![leaf("a", true, true)], vec![])],
        );
        files.state = Some(Toggle::None);
        let inventory = Inventory {
            sources: vec![files],
        };
        assert_eq!(inventory.source("files").unwrap().toggle(), Toggle::None);
        let mut none = source("files", vec![], vec![]);
        none.state = Some(Toggle::None);
        none.included = true;
        let inventory = Inventory {
            sources: vec![none],
        };
        assert_eq!(inventory.toggle(), Toggle::None);
        assert_eq!(inventory.report()["state"], "none");
    }

    #[test]
    fn widget_names_are_stable_and_path_safe() {
        assert_eq!(slug("browser|Google Chrome"), "browser-Google-Chrome");
        assert_eq!(slug("session|0123ab|slack"), "session-0123ab-slack");
        assert_eq!(
            slug("flatpak|--installation=flathub|app/x/y/z"),
            "flatpak-installation-flathub-app-x-y-z"
        );
        assert!(!slug("a/b").contains('/'));
    }

    #[test]
    fn a_read_size_is_never_shown_as_zero() {
        assert_eq!(size_text(0), None);
        assert_eq!(size_text(999).as_deref(), Some("999 B"));
        assert_eq!(size_text(1_500).as_deref(), Some("1.5 kB"));
        assert_eq!(size_text(3_000_000).as_deref(), Some("3 MB"));
        assert_eq!(size_text(2_450_000_000).as_deref(), Some("2.4 GB"));
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "your_data_integration_tests.rs"]
mod integration_tests;
