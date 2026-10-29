//! The word processor's commands, as slots in the window's bar
//! ([`ui::window_toolbar`]).
//!
//! The bar's location names the document, and its sidebar toggle shows
//! and hides the outline. Its row carries the
//! Format menu (the character formats, font, size, colour and lists), the Insert menu
//! (link, table or the table's edits inside a cell, image, contents) and the
//! inspector's toggle; its overflow menu the File commands and the ruler.
//! Each slot runs the same [`ToolAction`] the toolbar the editor used to draw
//! ran, with the document in hand; a template on a document with words
//! asks first, in the editor, before it replaces anything.

use ui::window_toolbar::{ToolbarAction, ToolbarEvent, ToolbarLocation, WindowToolbar};

use super::Editor;
use super::toolbar::ToolAction;
use crate::Template;
use crate::file_menu::FileChoice;
use crate::i18n::t;

/// What a slot of the bar runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BarCommand {
    /// A toolbar command.
    Tool(ToolAction),
    /// A File command: open, save, save as, or a template.
    File(FileChoice),
}

impl Editor {
    /// One command as a bar slot: its icon and name, on when it is a toggle
    /// that is on.
    fn tool_slot(&self, action: ToolAction, name: String, on: bool) -> ToolbarAction {
        let icon = action.icon().map_or("", ui::icons::Icon::name);
        self.bar.entry(icon, name, BarCommand::Tool(action)).on(on)
    }

    /// The Format menu's commands: the character formats as toggles, the
    /// font families with the selection's checked, the size and colours,
    /// then the list toggles.
    fn format_tools(&self) -> Vec<(ToolAction, String, bool)> {
        use ui::richtext::FontFamily;
        let font = self.current_font();
        let mut tools = vec![
            (
                ToolAction::Bold,
                t("toolbar-bold"),
                self.mark_active_in_selection("strong"),
            ),
            (
                ToolAction::Italic,
                t("toolbar-italic"),
                self.mark_active_in_selection("em"),
            ),
            (
                ToolAction::Underline,
                t("toolbar-underline"),
                self.mark_active_in_selection("underline"),
            ),
            (
                ToolAction::Strike,
                t("toolbar-strikethrough"),
                self.mark_active_in_selection("strike"),
            ),
            (
                ToolAction::Super,
                t("toolbar-superscript"),
                self.mark_active_in_selection("super"),
            ),
            (
                ToolAction::Sub,
                t("toolbar-subscript"),
                self.mark_active_in_selection("sub"),
            ),
        ];
        // The font picker's families, the selection's one checked.
        tools.extend(
            [FontFamily::Sans, FontFamily::Serif, FontFamily::Mono].map(|family| {
                (
                    ToolAction::SetFont(family),
                    super::controls::font_label(family),
                    family == font,
                )
            }),
        );
        tools.extend([
            (ToolAction::FontSizeDec, t("toolbar-size-down"), false),
            (ToolAction::FontSizeInc, t("toolbar-size-up"), false),
            (ToolAction::TextColorPalette, t("toolbar-text-color"), false),
            (ToolAction::HighlightPalette, t("toolbar-highlight"), false),
            (
                ToolAction::BulletList,
                t("toolbar-bulleted-list"),
                self.list_active(false),
            ),
            (
                ToolAction::NumberedList,
                t("toolbar-numbered-list"),
                self.list_active(true),
            ),
        ]);
        tools
    }

    /// The Insert menu's commands: a link over a selection, a table (or,
    /// inside a cell, the table's edits), an image and the contents.
    fn insert_tools(&self) -> Vec<(ToolAction, String)> {
        let mut tools = Vec::new();
        if !self.selection_blocks_cached().is_empty() {
            tools.push((ToolAction::Hyperlink, t("toolbar-link")));
        }
        if self.current_block_style().0 == "cell" {
            tools.extend([
                (ToolAction::TableRowAdd, t("toolbar-row-add")),
                (ToolAction::TableRowDel, t("toolbar-row-del")),
                (ToolAction::TableColAdd, t("toolbar-col-add")),
                (ToolAction::TableColDel, t("toolbar-col-del")),
                (ToolAction::TableDelete, t("toolbar-table-delete")),
            ]);
        } else {
            tools.push((ToolAction::InsertTable, t("toolbar-table")));
        }
        tools.extend([
            (ToolAction::InsertImage, t("toolbar-image")),
            (ToolAction::InsertToc, t("toolbar-toc")),
        ]);
        tools
    }

    /// What the document is called, as the bar's location names it: the
    /// file it was opened from or saved as, else its first heading, else its
    /// first line, else "Document".
    pub(super) fn document_title(&self) -> String {
        const WORDS: usize = 8;
        if let Some(stem) = self
            .path
            .as_ref()
            .and_then(|path| path.file_stem())
            .map(|stem| stem.to_string_lossy().into_owned())
            .filter(|stem| !stem.trim().is_empty())
        {
            return stem;
        }
        let written = |kind: Option<&str>| {
            self.blocks
                .iter()
                .filter(|block| kind.is_none_or(|kind| block.kind == kind))
                .map(|block| block.text.trim())
                .find(|text| !text.is_empty())
        };
        let Some(text) = written(Some("heading")).or_else(|| written(None)) else {
            return t("document-untitled");
        };
        let mut title = text
            .split_whitespace()
            .take(WORDS)
            .collect::<Vec<_>>()
            .join(" ");
        if text.split_whitespace().nth(WORDS).is_some() {
            title.push('…');
        }
        title
    }

    /// The window bar's slots.
    pub(super) fn window_bar(&self) -> WindowToolbar {
        let format = self
            .format_tools()
            .into_iter()
            .map(|(action, name, on)| self.tool_slot(action, name, on))
            .collect();
        let insert = self
            .insert_tools()
            .into_iter()
            .map(|(action, name)| self.tool_slot(action, name, false))
            .collect();
        let file = [
            (FileChoice::Open, t("file-menu-open"), "folder-open"),
            (FileChoice::Save, t("file-menu-save"), "file"),
            (FileChoice::SaveAs, t("file-menu-save-as"), "file-text"),
        ]
        .into_iter()
        .chain(Template::all().iter().map(|template| {
            (
                FileChoice::Template(*template),
                template.display_title(),
                "file-plus",
            )
        }))
        .map(|(choice, name, icon)| self.bar.entry(icon, name, BarCommand::File(choice)))
        .collect();
        // The menu buttons' ids are never sent: their items are.
        WindowToolbar {
            sidebar: Some(self.outline.is_open()),
            location: Some(ToolbarLocation {
                label: self.document_title(),
                // The document says whether the work in it is in its file:
                // a marker taken from anything that clears itself when the
                // frame is drawn (a repaint flag) would say "Edited" at
                // random, and say nothing the moment after a save.
                edited: self
                    .with_live(tilo_document::RichTextState::unsaved)
                    .unwrap_or(false),
                ..ToolbarLocation::default()
            }),
            actions: vec![
                ToolbarAction::new("format", "type", t("toolbar-format")).with_items(format),
                ToolbarAction::new("insert", "plus", t("toolbar-insert")).with_items(insert),
                self.bar
                    .entry(
                        "panel-right",
                        t("a11y-styles-panel"),
                        BarCommand::Tool(ToolAction::ToggleStyles),
                    )
                    .on(self.view.styles_panel_visible),
                ToolbarAction::new("file-menu", "file", t("toolbar-file"))
                    .with_items(file)
                    .in_menu(),
                self.tool_slot(
                    ToolAction::ToggleRuler,
                    t("toolbar-ruler"),
                    self.view.ruler_visible,
                )
                .in_menu(),
            ],
            ..WindowToolbar::default()
        }
    }

    /// Run what the person did in the window's bar, then declare the bar's
    /// slots for the document as it is now.
    pub(super) fn apply_window_toolbar(&mut self, edits: &mut ui::widget::Edits<'_>) {
        for event in ui::window_toolbar::take_events() {
            self.window_toolbar_event(edits, &event);
        }
        ui::window_toolbar::declare(Some(self.window_bar()));
    }

    /// Run one thing the person did in the window's bar.
    pub(super) fn window_toolbar_event(
        &mut self,
        edits: &mut ui::widget::Edits<'_>,
        event: &ToolbarEvent,
    ) {
        let command = if *event == ToolbarEvent::ToggleSidebar {
            BarCommand::Tool(ToolAction::ToggleOutline)
        } else if let Some(command) = self.bar.pressed(event) {
            command
        } else {
            return;
        };
        match command {
            BarCommand::Tool(action) => self.tool_clicked(edits, action),
            BarCommand::File(FileChoice::Template(template)) if self.has_words() => {
                // A template replaces the words: the editor asks first, at
                // the top of the page under the bar.
                let anchor =
                    vello::kurbo::Rect::new(self.rect.x1, self.rect.y0, self.rect.x1, self.rect.y0);
                self.file_menu
                    .set_text_scale(crate::num::f32_of_f64(self.text_scale));
                self.file_menu
                    .ask_anchored(template, anchor, self.floating_viewport());
                self.lifecycle.dirty = true;
            }
            BarCommand::File(choice) => {
                self.apply_file_choice(edits, choice);
                self.refresh_blocks(edits.doc());
                self.lifecycle.dirty = true;
            }
        }
    }
}
