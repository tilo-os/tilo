//! The word processor's commands as slots in the window's bar
//! ([`ui::window_toolbar`]): the location, the sidebar toggle, the Format and Insert
//! menus, the inspector's toggle, and the File commands and ruler in its overflow.

use ui::window_toolbar::{ToolbarAction, ToolbarEvent, ToolbarLocation, WindowToolbar};

use super::Editor;
use super::toolbar::ToolAction;
use crate::Template;
use crate::file_menu::FileChoice;
use crate::i18n::t;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BarCommand {
    Tool(ToolAction),
    File(FileChoice),
}

impl Editor {
    fn tool_slot(&self, action: ToolAction, name: String, on: bool) -> ToolbarAction {
        let icon = action.icon().map_or("", ui::icons::Icon::name);
        self.bar.entry(icon, name, BarCommand::Tool(action)).on(on)
    }

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

    pub(super) fn apply_window_toolbar(&mut self, edits: &mut ui::widget::Edits<'_>) {
        for event in ui::window_toolbar::take_events() {
            self.window_toolbar_event(edits, &event);
        }
        ui::window_toolbar::declare(Some(self.window_bar()));
    }

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
