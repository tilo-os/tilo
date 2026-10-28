//! Where the reader is in each screenshot scenario: the view, the open book,
//! a selection, an open panel. The library the scenario shows is staged by
//! its actions (`crate::scenarios`) before this runs.

use std::path::PathBuf;
use std::sync::Arc;

use ui::text_render::FontChoice;
use ui::widget::EditableLabel;
use ui::widget::toast::Transient;

use super::{
    BooksApp, DocPos, ImportKind, LoadingImport, Rename, RenameTarget, SettingsTab, TextSel, View,
};
use crate::content;
use crate::input_controls::PickTarget;
use crate::scenarios::{
    ADDED_FONT, COMMENTED_HIGHLIGHT, COMPOSED_NOTE, CUSTOM_SCHEME, PICKED_PAGE, REVEALED_NOTE,
};

/// The sample book whose letters the "comments" scenario reads.
const TIDEGLASS: &str = "book:tideglass";

impl BooksApp {
    /// A fresh device opens its first book, whose text is on another
    /// device: one that is offline (`waiting`), or one connected device
    /// that says it does not have it.
    fn stage_open_elsewhere(&mut self, waiting: bool) {
        use ui::collaboration::record_content::{ContentLane, RecordContent};

        self.view = View::Library;
        let Some(entry) = self
            .record
            .entries
            .values()
            .find(|entry| entry.title == "Pride and Prejudice")
            .cloned()
        else {
            return;
        };
        let lane = ContentLane::of("so.tilo.books-capture", "fresh-device");
        let store = RecordContent::store(tilo_content::ContentStore::memory(), Arc::clone(&lane));
        if let Some(hash) = entry.content_hash() {
            // Asked for, and the host says where it stands.
            let _ = store.read(&hash);
            let standing = if waiting {
                tilo_content::ContentState::Waiting
            } else {
                tilo_content::ContentState::Unavailable
            };
            lane.stands(hash, standing);
        }
        self.content = Some(store);
        self.open_hit(&crate::resource::book_place_resource(&entry.id));
    }

    /// Stage the view state of `scenario`. The reader opens on the first
    /// sample book where its progress left off, unless the scenario shows
    /// something else.
    pub(crate) fn stage_scenario(&mut self, scenario: &str) {
        self.view = View::Reader;
        self.book_idx = self.book_index(&content::bundled_book_id());
        self.chapter = 0;
        self.page = 0;
        self.last_persisted = None;
        match scenario {
            "onboarding" | "shelf" | "library" | "fresh-device" => self.view = View::Library,
            "fresh-device-waiting" => self.stage_open_elsewhere(true),
            "fresh-device-unavailable" => self.stage_open_elsewhere(false),
            "library-scrolled" => {
                // The large value clamps to the real maximum in draw.
                self.view = View::Library;
                self.shelf_scroll = 5000.0;
            }
            "page-bubble" => self.arm_reader_bubble_for_shots(),
            "narrow" => {
                // Contents open as an overlay flap; annotations closed so the
                // scrimmed flap reads clearly against the page.
                self.toc_drawer.snap(true);
                self.notes_drawer.snap(false);
            }
            "import-error" => {
                self.report_failure(
                    "Couldn’t open damaged.epub: epub text is too large (maximum 64 MiB)".into(),
                );
            }
            "import-loading" => {
                self.import_loading = Some(LoadingImport {
                    generation: 1,
                    path: PathBuf::from("/home/reader/Books/Illustrated Atlas.epub"),
                    kind: ImportKind::Open,
                    expected_book_id: None,
                    store_book: None,
                    card: Transient::showing_progress(()),
                });
            }
            "type" => self.reading_settings.open(),
            "long-toc" => self.stage_long_toc(),
            "long-annotations" => self.notes_scroll_to_annotation = Some(REVEALED_NOTE),
            "reduced-motion" => {
                // The still is calm, but keeps the desktop reader chrome of
                // the normal and high-contrast references.
                self.reduce_motion = true;
                self.page_turn = None;
                self.toc_drawer.snap(true);
                self.notes_drawer.snap(true);
            }
            "comments" => {
                self.book_idx = self.book_index(TIDEGLASS);
                self.comment_overlay.open(COMMENTED_HIGHLIGHT);
            }
            "continuous" => self.scroll_y = 520.0,
            "search" | "search-elsewhere" => {
                self.view = View::Library;
                self.query.staged = Some((View::Library, "glass".into()));
            }
            "search-in-book" => {
                self.book_idx = self.book_index(TIDEGLASS);
                self.query.staged = Some((View::Reader, "patience".into()));
            }
            _ => {
                if !self.stage_settings(scenario) {
                    self.stage_selection(scenario);
                }
            }
        }
        self.layout = None;
        self.dirty = true;
    }

    /// Where the shelf holds the book `id` (the first book when it does not).
    fn book_index(&self, id: &str) -> usize {
        self.library
            .iter()
            .position(|book| book.id == id)
            .unwrap_or(0)
    }

    /// A book with 32 chapters, read deep into it, so the contents rail
    /// scrolls to the open one.
    fn stage_long_toc(&mut self) {
        self.toc_drawer.snap(true);
        self.notes_drawer.snap(false);
        let index = self.book_idx;
        let template = self
            .library
            .get(index)
            .and_then(|book| book.chapters.first())
            .cloned();
        if let Some(template) = template
            && let Some(book) = Arc::make_mut(&mut self.library).get_mut(index)
        {
            Arc::make_mut(book).chapters = (0..32)
                .map(|index| crate::model::Chapter {
                    title: format!("Chapter {} — a longer voyage", index + 1),
                    blocks: template.blocks.clone(),
                })
                .collect();
            self.chapter = 27;
            // Keep the staged chapter from being replaced by the book's
            // recorded position.
            self.last_persisted = Some((self.chapter, self.page, 0));
            self.toc_scroll_to_active = true;
        }
    }

    /// The settings screen scenarios; whether `scenario` is one.
    fn stage_settings(&mut self, scenario: &str) -> bool {
        let tab = match scenario {
            "settings-library" => SettingsTab::Library,
            "font-picker" | "settings-fonts" | "settings-fonts-rename" => SettingsTab::Fonts,
            "settings-colours" | "scheme-editor" | "picker" => SettingsTab::Colours,
            "settings-shortcuts" => SettingsTab::Shortcuts,
            _ => return false,
        };
        self.view = View::Settings;
        self.settings_tab = tab;
        match scenario {
            "settings-library" => {
                // The folders books were opened from, one already watched.
                self.local_library.watched_folders = vec![PathBuf::from("/home/reader/Books")];
                self.local_library.books.clear();
                for (id, path) in [
                    ("book:a", PathBuf::from("/home/reader/Books/one.epub")),
                    ("book:b", PathBuf::from("/home/reader/Downloads/two.epub")),
                    ("book:c", content::workspace_books_dir().join("shared.epub")),
                ] {
                    self.local_library.books.insert(id.into(), path);
                }
            }
            // The installed families, each previewed in its own face.
            "font-picker" => self.font_picker = true,
            "settings-fonts-rename" => {
                // An inline rename in progress, typeset in the font's face.
                let mut field = EditableLabel::new("DejaVu Serif")
                    .with_font_family(FontChoice::Named("DejaVu Serif".into()));
                field.begin_edit();
                self.renaming = Some(Rename {
                    target: RenameTarget::Font(ADDED_FONT.into()),
                    field,
                });
            }
            "scheme-editor" => self.editing_scheme = Some(CUSTOM_SCHEME.into()),
            "picker" => self
                .color_picker
                .open(PickTarget::Slot(CUSTOM_SCHEME.into(), 0), PICKED_PAGE),
            _ => {}
        }
        true
    }

    /// The selection scenarios: a passage selected as a drag would.
    fn stage_selection(&mut self, scenario: &str) {
        let at = |block, ch| DocPos { block, ch };
        let (anchor, head) = match scenario {
            "word-select" => {
                // The end of a double-click on a word dragged right by words:
                // snapped to word boundaries through the real block text.
                let block = 2;
                let Some(text) = self
                    .library
                    .get(self.book_idx)
                    .and_then(|book| book.chapters.first())
                    .map(|chapter| chapter.block_text(block))
                else {
                    return;
                };
                let n = text.chars().count();
                if n <= 6 {
                    return;
                }
                let (start, _) = ui::word_boundaries_chars(&text, 0);
                let target = (n / 3).clamp(6, 48).min(n - 1);
                let (_, end) = ui::word_boundaries_chars(&text, target);
                (at(block, start), at(block, end))
            }
            "select" => (at(2, 0), at(2, 64)),
            "two-up-select" | "spread-select" => (at(2, 0), at(2, 60)),
            "highlight" => (at(1, 4), at(1, 58)),
            "select-drag" | "note" => (at(1, 6), at(2, 60)),
            "continuous-select" => {
                self.scroll_y = 120.0;
                (at(1, 6), at(2, 54))
            }
            _ => return,
        };
        self.sel = Some(TextSel { anchor, head });
        self.selected_block = Some(anchor.block);
        if scenario == "note" {
            self.composing = Some(COMPOSED_NOTE);
            self.note_composer.open(COMPOSED_NOTE, Some(&self.dispatch));
        }
    }
}
