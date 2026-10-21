//! Properties: the selected item's facts in the window's inspector.
//!
//! What an item is, where it is, how big and how recent, and, for media and
//! collaborative files, what they hold and who shares them, as one
//! [`PropertyList`] of labelled values in human units: sizes and dates
//! through `ui::format`, the folder by its name, never raw byte counts,
//! content hashes or record ids (the system's bookkeeping, which agents read
//! through the actions). Sharing and versions are rows whose action takes the
//! person to sharing or to History, not counts to read. The inspector
//! follows the selection while it is open; confirmations stay modal.

use ui::icons::Icon;
use ui::text_render::TextKind;
use ui::widget::{Element, Property, PropertyList, Tone};

use super::FileExplorerWidget;
use crate::command::{CommandSink, FileCommand, push};
use crate::i18n::{arg, t, t_args};
use crate::model::Entry;

impl FileExplorerWidget {
    /// The single item the selection names, as listed.
    fn properties_target(&self) -> Option<Entry> {
        self.selection
            .primary()
            .and_then(|index| self.displayed_entries().get(index).cloned())
    }

    /// Show the selected item's properties in the inspector and open it.
    /// Returns whether there was an item to show.
    pub(super) fn show_properties(&mut self) -> bool {
        let Some(entry) = self.properties_target() else {
            return false;
        };
        self.close_collab_panel();
        self.present_properties(entry);
        self.nav.open_inspector();
        true
    }

    /// While the inspector shows properties, keep it on the selection: a
    /// newly selected item, or new facts about the one shown (a size that
    /// changed, a share that arrived), show at once.
    pub(super) fn sync_properties(&mut self) {
        if self.properties_for.is_none() {
            return;
        }
        if let Some(entry) = self.properties_target()
            && self.properties_for.as_ref() != Some(&entry)
        {
            self.present_properties(entry);
        }
    }

    fn present_properties(&mut self, entry: Entry) {
        let counts = entry
            .is_dir
            .then(|| {
                self.folder_counts
                    .count(&entry.path, entry.modified, self.show_hidden)
            })
            .flatten();
        let place = entry.path.parent().map(|parent| {
            crate::breadcrumbs::breadcrumb_components(
                parent,
                self.home_path.as_deref(),
                &self.workspace_roots,
            )
            .last()
            .map(|(label, _)| label.clone())
            .unwrap_or_else(|| parent.display().to_string())
        });
        let panel = properties_panel(&entry, counts, place, &self.cmds);
        self.nav.set_inspector(Some(panel));
        self.nav.set_inspector_title(entry.name.clone());
        self.properties_for = Some(entry);
    }
}

/// What kind of item `entry` is, in words: "Folder", "Image", "PDF
/// document", never a MIME type.
pub(crate) fn kind_label(entry: &Entry) -> String {
    use thumbnail::FileCategory;
    if entry.is_dir {
        return t("files-kind-folder");
    }
    t(match thumbnail::category_for_path(&entry.path) {
        FileCategory::Image => "files-kind-image",
        FileCategory::Video => "files-kind-video",
        FileCategory::Audio => "files-kind-audio",
        FileCategory::Pdf => "files-kind-pdf",
        FileCategory::Ebook => "files-kind-ebook",
        FileCategory::Document => "files-kind-document",
        FileCategory::Spreadsheet => "files-kind-spreadsheet",
        FileCategory::Presentation => "files-kind-presentation",
        FileCategory::Archive => "files-kind-archive",
        FileCategory::Code => "files-kind-code",
        FileCategory::Text => "files-kind-text",
        FileCategory::Font => "files-kind-font",
        FileCategory::Unknown => "files-kind-file",
    })
}

/// The properties of `entry` as the inspector shows them. `counts` is how
/// many items a folder holds, once counted; `place` the name of the folder
/// it is in. Each action queues its command on `sink`.
pub(crate) fn properties_panel(
    entry: &Entry,
    counts: Option<u64>,
    place: Option<String>,
    sink: &CommandSink,
) -> Element {
    let mut groups: Vec<(String, Vec<Property>)> =
        vec![(String::new(), basics(entry, counts, place, sink))];
    let media = media(entry);
    if !media.is_empty() {
        groups.push((t("files-properties-media"), media));
    }
    let shared = collaboration(entry, sink);
    if !shared.is_empty() {
        groups.push((t("files-properties-collaboration"), shared));
    }
    Box::new(PropertyList::sections(groups).with_id("files.properties"))
}

/// What the item is, how big, where and how recent.
fn basics(
    entry: &Entry,
    counts: Option<u64>,
    place: Option<String>,
    sink: &CommandSink,
) -> Vec<Property> {
    let mut rows = vec![Property::new(t("files-property-kind"), kind_label(entry))];
    let size = if entry.is_dir {
        counts.map(ui::format::items)
    } else {
        Some(ui::format::size(entry.size))
    };
    if let Some(size) = size {
        rows.push(Property::new(t("files-property-size"), size).with_kind(TextKind::Numeric));
    }
    if let Some(place) = place {
        let sink = sink.clone();
        let path = entry.path.clone();
        rows.push(Property::new(t("files-property-where"), place).action(
            Icon::FolderOpen,
            t("files-property-show-in-folder"),
            move || push(&sink, FileCommand::RevealInFolder(path.clone())),
        ));
    }
    if let Some(modified) = entry.modified {
        rows.push(
            Property::new(
                t("files-property-modified"),
                ui::format::timestamp(&modified),
            )
            .with_kind(TextKind::Numeric),
        );
    }
    rows
}

/// What a picture, a recording or a document holds.
fn media(entry: &Entry) -> Vec<Property> {
    let Some(metadata) = &entry.rich_metadata else {
        return Vec::new();
    };
    let properties = &metadata.properties;
    let mut rows = Vec::new();
    if let (Some(width), Some(height)) = (properties.image_width, properties.image_height) {
        rows.push(
            Property::new(
                t("files-property-dimensions"),
                t_args(
                    "files-property-dimensions-value",
                    &[
                        arg("width", ui::format::count(u64::from(width))),
                        arg("height", ui::format::count(u64::from(height))),
                    ],
                ),
            )
            .with_kind(TextKind::Numeric),
        );
    }
    if let Some(duration_ms) = properties.duration_ms {
        rows.push(
            Property::new(
                t("files-property-duration"),
                ui::format::duration(std::time::Duration::from_millis(duration_ms)),
            )
            .with_kind(TextKind::Numeric),
        );
    }
    if let Some(pages) = properties.page_count {
        rows.push(
            Property::new(
                t("files-property-pages"),
                ui::format::count(u64::from(pages)),
            )
            .with_kind(TextKind::Numeric),
        );
    }
    if let Some(codec) = &properties.codec {
        rows.push(Property::new(t("files-property-codec"), codec.clone()));
    }
    if let Some(container) = &properties.container {
        rows.push(Property::new(
            t("files-property-container"),
            container.clone(),
        ));
    }
    rows
}

/// Where a collaborative file's content is, who it is shared with and its
/// versions, each leading to where it is acted on.
fn collaboration(entry: &Entry, sink: &CommandSink) -> Vec<Property> {
    let mut rows = Vec::new();
    if let Some(availability) = &entry.availability {
        let stored = match availability.residency {
            fs_protocol::Residency::NotResident => t("files-property-stored-not"),
            fs_protocol::Residency::Partial { resident_bytes } => t_args(
                "files-property-stored-partly",
                &[
                    arg("stored", ui::format::size(resident_bytes)),
                    arg("size", ui::format::size(entry.size)),
                ],
            ),
            fs_protocol::Residency::Resident { .. } => t("files-property-stored-here"),
        };
        rows.push(Property::new(t("files-property-on-device"), stored));
    }
    if let Some(share) = &entry.share {
        let people = share.active_grants();
        let value = if people == 0 {
            t("files-property-shared-not")
        } else {
            t_args(
                "files-property-shared-with",
                &[arg("count", i64::try_from(people).unwrap_or(i64::MAX))],
            )
        };
        let sink = sink.clone();
        rows.push(Property::new(t("files-property-shared"), value).action(
            Icon::Users,
            t("files-property-open-sharing"),
            move || push(&sink, FileCommand::ShowSharing),
        ));
    }
    if let Some(history) = &entry.history {
        let conflicts = history.open_conflicts().count();
        let versions = t_args(
            "files-property-versions-value",
            &[arg(
                "count",
                i64::try_from(history.total_versions).unwrap_or(i64::MAX),
            )],
        );
        let (value, tone) = if conflicts == 0 {
            (versions, Tone::Neutral)
        } else {
            (
                t_args(
                    "files-property-versions-conflicts",
                    &[
                        arg("versions", versions),
                        arg("count", i64::try_from(conflicts).unwrap_or(i64::MAX)),
                    ],
                ),
                Tone::Warning,
            )
        };
        let sink = sink.clone();
        rows.push(
            Property::new(t("files-property-versions"), value)
                .tone(tone)
                .action(Icon::History, t("files-property-open-history"), move || {
                    push(&sink, FileCommand::ShowHistory);
                }),
        );
    }
    rows
}
