//! What an action is called: the stable target assistive technology holds,
//! the words a person reads in a tooltip and a screen reader, and the state
//! it reports.

use ui::a11y::{AccessState, Role};
use ui::i18n::ArgValue;
use uuid::Uuid;

use super::{Action, BooksApp, progress_label};
use crate::model::{ColorScheme, FontEntry, HighlightColor};
use crate::resource::CATALOG;

impl BooksApp {
    /// when the shelf is sorted or annotations are reordered.
    pub(super) fn semantic_target(&self, action: &Action) -> Option<String> {
        let target = match action {
            Action::OpenBook(index) => format!("book.open:{}", self.shelf_book(*index)?.id),
            Action::ShareToWorkspace(id) => format!("book.share:{id}"),
            Action::RemoveFromLibrary(id) => format!("book.remove:{id}"),
            Action::Back => "reader.back".into(),
            Action::PrevPage => "reader.previous-page".into(),
            Action::NextPage => "reader.next-page".into(),
            Action::SeekProgress => "reader.progress".into(),
            Action::GotoChapter(index) => format!("reader.chapter:{index}"),
            Action::ToggleToc => "reader.contents".into(),
            Action::ToggleNotes => "reader.annotations".into(),
            Action::ToggleTypePopover => "reader.typography".into(),
            Action::ToggleBookmark => "reader.bookmark".into(),
            Action::FontInc => "reader.font-size.increment".into(),
            Action::FontDec => "reader.font-size.decrement".into(),
            Action::SpacingInc => "reader.line-spacing.increment".into(),
            Action::SpacingDec => "reader.line-spacing.decrement".into(),
            Action::CycleFont => "reader.font.next".into(),
            Action::SetFont(id) => format!("reader.font:{id}"),
            Action::SetReadTheme(theme) => format!("reader.theme:{}", theme.as_str()),
            Action::SetMode(mode) => format!("reader.mode:{}", mode.as_str()),
            Action::SetComposition(style) => format!("reader.composition:{}", style.as_str()),
            Action::SetColor(color) => format!("reader.highlight:{color:?}"),
            Action::AddNote => "reader.add-note".into(),
            Action::CopySelection => "reader.copy-selection".into(),
            Action::JumpTo(id) => format!("annotation.reveal:{id}"),
            Action::Delete(id) => format!("annotation.delete:{id}"),
            Action::EditNote(id) => format!("annotation.edit:{id}"),
            Action::OpenComments(id) => format!("highlight.comments:{id}"),
            Action::CloseComments => "highlight.comments.close".into(),
            Action::DeleteHighlight(id) => format!("highlight.delete:{id}"),
            Action::AddComment(id) => format!("highlight.comment.add:{id}"),
            Action::EditComment(id) => format!("highlight.comment.edit:{id}"),
            Action::DeleteComment(id) => format!("highlight.comment.delete:{id}"),
            Action::OpenViaPortal => "library.open".into(),
            Action::ToggleWatchFolder(path) => {
                format!("library.watch:{}", ui::a11y::hash_key(path))
            }
            Action::ToggleKeepCatalogues => "library.keep-catalogues".into(),
            Action::OpenSettings => "settings.open".into(),
            Action::CloseSettings => "settings.close".into(),
            Action::SetSettingsTab(tab) => format!("settings.tab:{tab:?}"),
            Action::BeginShortcutCapture(shortcut) => {
                format!("settings.shortcut:{}", shortcut.id())
            }
            Action::SetScheme(id) => format!("settings.scheme.select:{id}"),
            Action::ToggleSchemeEnabled(id) => format!("settings.scheme.enabled:{id}"),
            Action::DuplicateScheme(id) => format!("settings.scheme.duplicate:{id}"),
            Action::DeleteScheme(id) => format!("settings.scheme.delete:{id}"),
            Action::EditScheme(id) => format!("settings.scheme.edit:{id}"),
            Action::CloseSchemeEditor => "settings.scheme-editor.close".into(),
            Action::NewSchemeViaPicker => "settings.scheme.new".into(),
            Action::PickSchemeColor(id, slot) => format!("settings.scheme.color:{id}:{slot}"),
            Action::BeginRenameScheme(id) => format!("settings.scheme.rename:{id}"),
            Action::ToggleFontShown(id) => format!("settings.font.shown:{id}"),
            Action::RemoveFont(id) => format!("settings.font.remove:{id}"),
            Action::OpenFontPicker => "settings.font.add".into(),
            Action::CloseFontPicker => "settings.font-picker.close".into(),
            Action::AddSystemFont(family) => {
                format!("settings.font.add:{}", ui::a11y::hash_key(family))
            }
            Action::BeginRenameFont(id) => format!("settings.font.rename:{id}"),
            Action::OpenHit(anchor) => format!("search.open:{anchor}"),
            Action::GetBook(id) => format!("search.get:{id}"),
            Action::RetryRemote(source) => format!("search.retry:{}", ui::a11y::hash_key(source)),
            Action::CloseSearch => "search.back".into(),
            Action::DismissOverlay
            | Action::Swallow
            | Action::SetQuery(_)
            | Action::OpenFacet(_) => return None,
        };
        Some(format!("books.{target}"))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the exhaustive action-to-accessible-label mapping is intentionally co-located"
    )]
    pub(super) fn semantic_label(&self, action: &Action) -> String {
        match action {
            Action::OpenBook(index) => self.shelf_book(*index).map_or_else(
                || CATALOG.t("ebook-action-open-book"),
                |book| {
                    CATALOG.t_args(
                        "ebook-query-open",
                        &[("title", ArgValue::Text(book.title.clone()))],
                    )
                },
            ),
            Action::ShareToWorkspace(id) => {
                self.library.iter().find(|book| book.id == *id).map_or_else(
                    || CATALOG.t("ebook-action-share-book"),
                    |book| {
                        CATALOG.t_args(
                            "ebook-action-share-book-title",
                            &[("book", ArgValue::Text(book.title.clone()))],
                        )
                    },
                )
            }
            Action::RemoveFromLibrary(id) => {
                self.library.iter().find(|book| book.id == *id).map_or_else(
                    || CATALOG.t("ebook-action-remove-book"),
                    |book| {
                        CATALOG.t_args(
                            "ebook-action-remove-book-title",
                            &[("book", ArgValue::Text(book.title.clone()))],
                        )
                    },
                )
            }
            Action::Back | Action::CloseSettings => CATALOG.t("ebook-action-back-to-library"),
            Action::PrevPage => CATALOG.t("ebook-action-previous-page"),
            Action::NextPage => CATALOG.t("ebook-action-next-page"),
            Action::SeekProgress => CATALOG.t_args(
                "ebook-action-reading-progress",
                &[(
                    "progress",
                    ArgValue::Text(progress_label(self.reading_progress)),
                )],
            ),
            Action::GotoChapter(index) => self
                .open_book()
                .and_then(|book| book.chapters.get(*index))
                .map_or_else(
                    || {
                        CATALOG.t_args(
                            "ebook-action-go-to-chapter",
                            &[("number", ArgValue::Number(*index as f64 + 1.0))],
                        )
                    },
                    |c| c.title.clone(),
                ),
            Action::ToggleToc => CATALOG.t("ebook-action-contents"),
            Action::ToggleNotes => CATALOG.t("ebook-action-annotations"),
            Action::ToggleTypePopover => CATALOG.t("ebook-action-reading-appearance"),
            Action::ToggleBookmark => CATALOG.t("ebook-action-bookmark-page"),
            Action::FontInc => CATALOG.t("ebook-action-increase-text"),
            Action::FontDec => CATALOG.t("ebook-action-decrease-text"),
            Action::SpacingInc => CATALOG.t("ebook-action-increase-spacing"),
            Action::SpacingDec => CATALOG.t("ebook-action-decrease-spacing"),
            Action::CycleFont => CATALOG.t("ebook-action-next-typeface"),
            Action::SetFont(id) => self
                .fonts
                .iter()
                .find(|font| font.id == *id)
                .map_or_else(|| id.clone(), FontEntry::title),
            Action::SetReadTheme(theme) => CATALOG.t_args(
                "ebook-reading-theme",
                &[("theme", ArgValue::Text(theme.as_str().to_owned()))],
            ),
            Action::SetMode(mode) => CATALOG.t_args(
                "ebook-action-mode",
                &[("mode", ArgValue::Text(mode.as_str().to_owned()))],
            ),
            Action::SetComposition(style) => CATALOG.t_args(
                "ebook-action-composition",
                &[("style", ArgValue::Text(style.as_str().to_owned()))],
            ),
            Action::SetColor(color) => CATALOG.t_args(
                "ebook-action-highlight",
                &[(
                    "colour",
                    ArgValue::Text(highlight_colour_id(*color).to_owned()),
                )],
            ),
            Action::AddNote => CATALOG.t("ebook-add-note-button"),
            Action::CopySelection => CATALOG.t("ebook-action-copy-selection"),
            Action::JumpTo(id) => {
                self.annotation_label(*id, CATALOG.t("ebook-action-reveal-annotation"))
            }
            Action::Delete(id) => {
                self.annotation_label(*id, CATALOG.t("ebook-action-delete-annotation"))
            }
            Action::EditNote(id) => self.annotation_label(*id, CATALOG.t("ebook-a11y-edit-note")),
            Action::OpenComments(_) => CATALOG.t("ebook-action-open-comments"),
            Action::CloseComments => CATALOG.t("ebook-action-close-comments"),
            Action::DeleteHighlight(_) => CATALOG.t("ebook-action-delete-highlight"),
            Action::AddComment(_) => CATALOG.t("ebook-action-add-comment"),
            Action::EditComment(id) => {
                self.annotation_label(*id, CATALOG.t("ebook-action-edit-comment"))
            }
            Action::DeleteComment(id) => {
                self.annotation_label(*id, CATALOG.t("ebook-action-delete-comment"))
            }
            Action::OpenViaPortal => CATALOG.t("ebook-action-open-book"),
            Action::ToggleWatchFolder(path) => CATALOG.t_args(
                "ebook-action-watch-folder",
                &[("folder", ArgValue::Text(path.display().to_string()))],
            ),
            Action::ToggleKeepCatalogues => CATALOG.t("ebook-keep-catalogues"),
            Action::OpenSettings => CATALOG.t("ebook-view-settings"),
            Action::SetSettingsTab(tab) => CATALOG.t(tab.message_id()),
            Action::BeginShortcutCapture(shortcut) => {
                if self.shortcut_capture == Some(*shortcut) {
                    self.shortcut_capture_error.clone().map_or_else(
                        || {
                            CATALOG.t_args(
                                "ebook-action-press-key",
                                &[("shortcut", ArgValue::Text(shortcut.label()))],
                            )
                        },
                        |error| {
                            CATALOG.t_args(
                                "ebook-action-shortcut-error",
                                &[
                                    ("shortcut", ArgValue::Text(shortcut.label())),
                                    ("error", ArgValue::Text(error)),
                                ],
                            )
                        },
                    )
                } else {
                    CATALOG.t_args(
                        "ebook-action-change-shortcut",
                        &[
                            ("shortcut", ArgValue::Text(shortcut.label())),
                            (
                                "keys",
                                ArgValue::Text(
                                    self.reader_keys
                                        .shortcut(shortcut.id(), shortcut.default_key()),
                                ),
                            ),
                        ],
                    )
                }
            }
            Action::SetScheme(id) => self.scheme_name(id),
            Action::ToggleSchemeEnabled(id) => CATALOG.t_args(
                "ebook-action-show-scheme",
                &[("scheme", ArgValue::Text(self.scheme_name(id)))],
            ),
            Action::DuplicateScheme(id) => CATALOG.t_args(
                "ebook-action-duplicate-scheme",
                &[("scheme", ArgValue::Text(self.scheme_name(id)))],
            ),
            Action::DeleteScheme(id) => CATALOG.t_args(
                "ebook-action-delete-scheme",
                &[("scheme", ArgValue::Text(self.scheme_name(id)))],
            ),
            Action::EditScheme(id) => CATALOG.t_args(
                "ebook-action-edit-scheme",
                &[("scheme", ArgValue::Text(self.scheme_name(id)))],
            ),
            Action::CloseSchemeEditor => CATALOG.t("ebook-action-done-editing-theme"),
            Action::NewSchemeViaPicker => CATALOG.t("ebook-action-new-colour-theme"),
            Action::PickSchemeColor(id, slot) => CATALOG.t_args(
                "ebook-action-edit-scheme-colour",
                &[
                    ("scheme", ArgValue::Text(self.scheme_name(id))),
                    ("slot", ArgValue::Text(scheme_slot_id(*slot).to_owned())),
                ],
            ),
            Action::BeginRenameScheme(id) => CATALOG.t_args(
                "ebook-action-rename-scheme",
                &[("scheme", ArgValue::Text(self.scheme_name(id)))],
            ),
            Action::ToggleFontShown(id) => CATALOG.t_args(
                "ebook-action-show-font",
                &[("font", ArgValue::Text(self.font_name(id)))],
            ),
            Action::RemoveFont(id) => CATALOG.t_args(
                "ebook-action-remove-font",
                &[("font", ArgValue::Text(self.font_name(id)))],
            ),
            Action::OpenFontPicker => CATALOG.t("ebook-action-add-font"),
            Action::CloseFontPicker => CATALOG.t("ebook-action-close-font-picker"),
            Action::AddSystemFont(family) => CATALOG.t_args(
                "ebook-action-add-family",
                &[("family", ArgValue::Text(family.clone()))],
            ),
            Action::BeginRenameFont(id) => CATALOG.t_args(
                "ebook-action-rename-font",
                &[("font", ArgValue::Text(self.font_name(id)))],
            ),
            Action::OpenHit(anchor) => self
                .query
                .label_of(anchor)
                .map_or_else(|| CATALOG.t("ebook-action-open-result"), str::to_owned),
            Action::GetBook(id) => self
                .query
                .label_of(&format!("gutenberg:{id}"))
                .map_or_else(|| CATALOG.t("ebook-action-get-book"), str::to_owned),
            Action::RetryRemote(_) => crate::resource::CATALOG.t("arriving-retry"),
            Action::CloseSearch => CATALOG.t("ebook-action-back"),
            Action::DismissOverlay
            | Action::Swallow
            | Action::SetQuery(_)
            | Action::OpenFacet(_) => String::new(),
        }
    }

    /// Refresh the hover description from the topmost live hit. The reader
    /// draws several icon-only affordances on a narrow viewport, and a hit can
    /// also be covered by a modal; using the final hit stack means the tooltip
    /// always names the action that would actually receive a click.
    pub(super) fn refresh_hover_tooltip(&mut self) {
        self.hover_tooltip = self
            .hits
            .iter()
            .rev()
            .find(|hit| hit.rect.contains(self.cursor))
            .map(|hit| self.semantic_label(&hit.action))
            .filter(|label| !label.is_empty());
    }

    pub(super) fn annotation_label(&self, id: Uuid, fallback: String) -> String {
        let text = self
            .all_annotations
            .iter()
            .find(|annotation| annotation.id == id)
            .map(|annotation| {
                let note = annotation.note.trim();
                if note.is_empty() {
                    annotation.excerpt.trim()
                } else {
                    note
                }
            })
            .filter(|text| !text.is_empty());
        match text {
            Some(text) => CATALOG.t_args(
                "ebook-action-annotation",
                &[
                    ("action", ArgValue::Text(fallback)),
                    ("text", ArgValue::Text(text.to_owned())),
                ],
            ),
            None => fallback,
        }
    }

    pub(super) fn font_name(&self, id: &str) -> String {
        self.fonts
            .iter()
            .find(|font| font.id == id)
            .map_or_else(|| id.to_string(), FontEntry::title)
    }

    pub(super) fn scheme_name(&self, id: &str) -> String {
        self.schemes
            .iter()
            .find(|scheme| scheme.id == id)
            .map_or_else(|| id.to_string(), ColorScheme::title)
    }

    pub(super) fn semantic_role_state(&self, action: &Action) -> (Role, AccessState) {
        let focused = self
            .semantic_target(action)
            .is_some_and(|target| self.semantic_focus.as_deref() == Some(&target));
        let base = AccessState::new().focusable(true).focused(focused);
        match action {
            Action::SeekProgress => (Role::Slider, base),
            Action::ToggleToc => (Role::ToggleButton, base.checked(self.toc_open())),
            Action::ToggleNotes => (Role::ToggleButton, base.checked(self.notes_open())),
            Action::ToggleTypePopover => (
                Role::ToggleButton,
                base.checked(self.reading_settings.is_open()),
            ),
            Action::ToggleBookmark => (Role::ToggleButton, base.checked(self.page_bookmarked())),
            Action::SetSettingsTab(tab) => {
                (Role::RadioButton, base.checked(self.settings_tab == *tab))
            }
            Action::SetReadTheme(theme) => (
                Role::RadioButton,
                base.checked(self.settings.theme == *theme && self.settings.scheme_id.is_empty()),
            ),
            Action::SetMode(mode) => (Role::RadioButton, base.checked(self.settings.mode == *mode)),
            Action::SetComposition(style) => (
                Role::RadioButton,
                base.checked(self.settings.composition == *style),
            ),
            Action::SetFont(id) => (
                Role::RadioButton,
                base.checked(self.settings.font_id == *id),
            ),
            Action::SetScheme(id) => (
                Role::RadioButton,
                base.checked(self.settings.scheme_id == *id),
            ),
            Action::ToggleFontShown(id) => (
                Role::Switch,
                base.checked(
                    self.fonts
                        .iter()
                        .find(|font| font.id == *id)
                        .is_some_and(|f| f.shown),
                ),
            ),
            Action::ToggleSchemeEnabled(id) => (
                Role::Switch,
                base.checked(
                    self.schemes
                        .iter()
                        .find(|scheme| scheme.id == *id)
                        .is_some_and(|s| s.enabled),
                ),
            ),
            Action::ToggleWatchFolder(path) => (
                Role::Checkbox,
                base.checked(self.watched_folders.contains(path)),
            ),
            Action::ToggleKeepCatalogues => (Role::Switch, base.checked(self.keeps_catalogues())),
            _ => (Role::Button, base),
        }
    }
}

/// The Fluent variable naming a highlight's colour.
fn highlight_colour_id(color: HighlightColor) -> &'static str {
    match color {
        HighlightColor::Yellow => "yellow",
        HighlightColor::Green => "green",
        HighlightColor::Pink => "pink",
        HighlightColor::Blue => "blue",
        HighlightColor::Purple => "purple",
        HighlightColor::Custom(_) => "custom",
    }
}

/// The Fluent variable naming a colour scheme's slot.
fn scheme_slot_id(slot: usize) -> &'static str {
    match slot {
        0 => "page",
        1 => "text",
        2 => "accent",
        _ => "custom",
    }
}
