//! The word processor's File commands: open, save, save as (which is also
//! how a document is exported to Word, PDF and the other formats) and
//! starting over from a template.
//!
//! The commands are the window bar's File menu (`widget::window_bar`). A
//! template replaces the document, so on a document that has words the
//! editor first asks, in this menu, whether to replace it; the menu adapts
//! the shared [`ContextMenu`] into a typed [`FileChoice`] and the editor
//! carries out the choice.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use ui::PresenceCtx;
use ui::platform::Shortcut;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use ui::widget::context_menu::{ContextMenu, MenuItem};

use crate::Template;
use crate::i18n::{arg, t, t_args};

/// What the person chose from the File menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileChoice {
    Open,
    Save,
    SaveAs,
    /// Replace the document with this template (already confirmed when the
    /// document had words).
    Template(Template),
}

/// What a row of the menu does when chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Choose(FileChoice),
    /// A template row on a document with words: asks before replacing.
    AskTemplate(Template),
    /// Keep the document: closes the question.
    Cancel,
    /// A separator or a caption: chooses nothing.
    Inert,
}

/// Owns the File menu's lifecycle, drawing, hit-testing and typed outcome.
pub(crate) struct FileMenu {
    menu: ContextMenu,
    rows: Vec<Row>,
    text_scale: f32,
    /// Where the menu was opened, so the replace question opens in its place.
    placed: Option<(Rect, Rect)>,
    /// The template a pending replace question is about.
    asking: Option<Template>,
}

impl Default for FileMenu {
    fn default() -> Self {
        let mut menu = Self {
            menu: ContextMenu::new(),
            rows: Vec::new(),
            text_scale: 1.0,
            placed: None,
            asking: None,
        };
        menu.set_rows(Self::file_rows(false));
        menu
    }
}

impl FileMenu {
    /// The File menu's rows: the file commands, then the templates. On a
    /// document with words a template asks before it replaces anything.
    fn file_rows(guard: bool) -> Vec<(MenuItem, Row)> {
        let mut rows = vec![
            (
                MenuItem::action(t("file-menu-open"), || {}).with_shortcut(Shortcut::command("O")),
                Row::Choose(FileChoice::Open),
            ),
            (
                MenuItem::action(t("file-menu-save"), || {}).with_shortcut(Shortcut::command("S")),
                Row::Choose(FileChoice::Save),
            ),
            (
                MenuItem::action(t("file-menu-save-as"), || {})
                    .with_shortcut(Shortcut::command("S").shift()),
                Row::Choose(FileChoice::SaveAs),
            ),
            (MenuItem::separator(), Row::Inert),
            (MenuItem::caption(t("file-menu-templates")), Row::Inert),
        ];
        rows.extend(Template::all().iter().map(|template| {
            let row = if guard {
                Row::AskTemplate(*template)
            } else {
                Row::Choose(FileChoice::Template(*template))
            };
            (MenuItem::action(template.display_title(), || {}), row)
        }));
        rows
    }

    /// The question asked before `template` replaces a document with words.
    fn ask_rows(template: Template) -> Vec<(MenuItem, Row)> {
        vec![
            (
                MenuItem::caption(t_args(
                    "file-menu-replace-question",
                    &[arg("template", template.display_title())],
                )),
                Row::Inert,
            ),
            (
                MenuItem::destructive_selection(t("file-menu-replace")),
                Row::Choose(FileChoice::Template(template)),
            ),
            (MenuItem::action(t("file-menu-keep"), || {}), Row::Cancel),
        ]
    }

    fn set_rows(&mut self, rows: Vec<(MenuItem, Row)>) {
        let (items, rows): (Vec<_>, Vec<_>) = rows.into_iter().unzip();
        self.menu.set_items(items);
        self.rows = rows;
    }

    pub(crate) fn is_open(&self) -> bool {
        self.menu.is_open()
    }

    /// Whether the menu is asking whether to replace the document.
    #[cfg(test)]
    pub(crate) fn is_asking(&self) -> bool {
        self.asking.is_some() && self.is_open()
    }

    /// Open the menu under `anchor` (the File button), kept inside
    /// `viewport`. `has_words` says the document has something a template
    /// would replace.
    pub(crate) fn open_anchored(&mut self, anchor: Rect, viewport: Rect, has_words: bool) {
        let text_scale = self.text_scale;
        *self = Self::default();
        self.set_text_scale(text_scale);
        self.set_rows(Self::file_rows(has_words));
        self.placed = Some((anchor, viewport));
        self.menu.open_anchored(anchor, viewport);
    }

    /// Open the question whether `template` should replace the document's
    /// words, under `anchor` (the window bar's File menu chose it), kept
    /// inside `viewport`.
    pub(crate) fn ask_anchored(&mut self, template: Template, anchor: Rect, viewport: Rect) {
        self.open_anchored(anchor, viewport, true);
        let row = self
            .rows
            .iter()
            .position(|row| *row == Row::AskTemplate(template));
        let _ = self.choose(row);
    }

    pub(crate) fn set_text_scale(&mut self, text_scale: f32) {
        self.text_scale = text_scale;
        self.menu.set_text_scale(text_scale);
    }

    pub(crate) fn close(&mut self) {
        self.asking = None;
        self.menu.close();
    }

    /// The shared menu owns pointer tracking and keyboard navigation.
    pub(crate) fn menu_mut(&mut self) -> &mut ContextMenu {
        &mut self.menu
    }

    /// Carry out the row at `index`: a choice comes back, a template on a
    /// document with words turns the menu into its question, Cancel closes.
    fn choose(&mut self, index: Option<usize>) -> Option<FileChoice> {
        match index.and_then(|index| self.rows.get(index).copied()) {
            Some(Row::Choose(choice)) => {
                self.close();
                Some(choice)
            }
            Some(Row::AskTemplate(template)) => {
                self.asking = Some(template);
                self.set_rows(Self::ask_rows(template));
                if let Some((anchor, viewport)) = self.placed {
                    self.menu.open_anchored(anchor, viewport);
                }
                None
            }
            Some(Row::Cancel | Row::Inert) | None => {
                self.close();
                None
            }
        }
    }

    /// The row chosen with the keyboard since the last call, carried out.
    pub(crate) fn take_selection(&mut self) -> Option<FileChoice> {
        let index = self.menu.take_selection()?;
        self.choose(Some(index))
    }

    /// Consume a click while open. A click outside the menu dismisses it;
    /// clicking a row carries it out (see [`Self::choose`]).
    pub(crate) fn click(&mut self, point: Point) -> Option<FileChoice> {
        let index = self.menu.item_at([point.x, point.y]);
        self.choose(index)
    }

    pub(crate) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.is_open() {
            Widget::draw_overlay(
                &mut self.menu,
                scene,
                transform,
                text_renderer,
                theme,
                presence,
            );
        }
    }

    #[cfg(test)]
    fn labels(&self) -> Vec<&str> {
        self.menu
            .items()
            .iter()
            .filter_map(MenuItem::label)
            .collect()
    }

    /// Where the row that chooses or asks about `template` is drawn.
    #[cfg(test)]
    pub(crate) fn item_center(&self, template: Template) -> Option<Point> {
        let index = self.rows.iter().position(|row| {
            matches!(row, Row::Choose(FileChoice::Template(t)) | Row::AskTemplate(t) if *t == template)
        })?;
        self.menu.item_rect(index).map(|rect| rect.center())
    }

    /// The index of the row choosing `choice`.
    #[cfg(test)]
    pub(crate) fn choice_row(&self, choice: FileChoice) -> Option<usize> {
        self.rows.iter().position(|row| *row == Row::Choose(choice))
    }

    /// Where the row choosing `choice` is drawn.
    #[cfg(test)]
    pub(crate) fn choice_center(&self, choice: FileChoice) -> Option<Point> {
        let index = self
            .rows
            .iter()
            .position(|row| *row == Row::Choose(choice))?;
        self.menu.item_rect(index).map(|rect| rect.center())
    }

    /// The index of the replace question's Cancel row.
    #[cfg(test)]
    pub(crate) fn cancel_row(&self) -> Option<usize> {
        self.rows.iter().position(|row| *row == Row::Cancel)
    }

    /// Where the Cancel row of the replace question is drawn.
    #[cfg(test)]
    pub(crate) fn cancel_center(&self) -> Option<Point> {
        let index = self.rows.iter().position(|row| *row == Row::Cancel)?;
        self.menu.item_rect(index).map(|rect| rect.center())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(menu: &mut FileMenu, has_words: bool) {
        menu.open_anchored(
            Rect::new(10.0, 0.0, 60.0, 20.0),
            Rect::new(0.0, 0.0, 800.0, 600.0),
            has_words,
        );
    }

    /// The menu offers the file commands and every template, a click returns
    /// a typed choice and closes, a scaled menu keeps rows mapped, and an
    /// outside click dismisses without a choice.
    #[test]
    fn the_file_menu_maps_rows_to_choices_at_every_scale() {
        // Phase: rows_offer_the_file_commands_then_the_templates.
        {
            let mut menu = FileMenu::default();
            open(&mut menu, false);
            let labels = menu.labels();
            for key in ["file-menu-open", "file-menu-save", "file-menu-save-as"] {
                assert!(labels.contains(&t(key).as_str()), "{key} is offered");
            }
            for template in Template::all() {
                assert!(labels.contains(&template.display_title().as_str()));
            }
        }

        // Phase: click_returns_a_typed_choice_and_closes.
        {
            let mut menu = FileMenu::default();
            open(&mut menu, false);
            let report = menu.item_center(Template::Report).unwrap();
            assert_eq!(
                menu.click(report),
                Some(FileChoice::Template(Template::Report))
            );
            assert!(!menu.is_open());
            open(&mut menu, false);
            let save_as = menu.choice_center(FileChoice::SaveAs).unwrap();
            assert_eq!(menu.click(save_as), Some(FileChoice::SaveAs));
        }

        // Phase: scaled_menu_keeps_rows_mapped_to_their_choices.
        {
            let mut menu = FileMenu::default();
            menu.set_text_scale(2.0);
            open(&mut menu, false);
            let report = menu
                .item_center(Template::Report)
                .expect("report stays an actionable row at large text");
            assert_eq!(
                menu.click(report),
                Some(FileChoice::Template(Template::Report))
            );
        }

        // Phase: outside_click_dismisses_without_a_choice.
        {
            let mut menu = FileMenu::default();
            open(&mut menu, false);
            assert_eq!(menu.click(Point::new(0.0, 0.0)), None);
            assert!(!menu.is_open());
        }
    }

    /// On a document with words, a template asks first: Replace chooses it,
    /// Keep closes without a choice.
    #[test]
    fn a_template_asks_before_replacing_words() {
        let mut menu = FileMenu::default();
        open(&mut menu, true);
        let letter = menu.item_center(Template::Letter).unwrap();
        assert_eq!(menu.click(letter), None, "no replacement yet");
        assert!(menu.is_asking(), "the menu asks first");
        let replace = menu
            .choice_center(FileChoice::Template(Template::Letter))
            .expect("the question offers Replace");
        assert_eq!(
            menu.click(replace),
            Some(FileChoice::Template(Template::Letter))
        );
        assert!(!menu.is_open());

        open(&mut menu, true);
        let letter = menu.item_center(Template::Letter).unwrap();
        assert_eq!(menu.click(letter), None);
        let keep = menu.cancel_center().unwrap();
        assert_eq!(menu.click(keep), None, "Keep chooses nothing");
        assert!(!menu.is_open());
    }
}
