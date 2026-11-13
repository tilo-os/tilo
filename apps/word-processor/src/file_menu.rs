//! The File commands: open, save, save as (also how a document is exported) and a new
//! document from a template. A template replaces the document, so the menu asks first
//! when it has words.

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileChoice {
    Open,
    Save,
    SaveAs,
    Template(Template),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Choose(FileChoice),
    AskTemplate(Template),
    Cancel,
    Inert,
}

pub(crate) struct FileMenu {
    menu: ContextMenu,
    rows: Vec<Row>,
    text_scale: f32,
    placed: Option<(Rect, Rect)>,
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

    #[cfg(test)]
    pub(crate) fn is_asking(&self) -> bool {
        self.asking.is_some() && self.is_open()
    }

    pub(crate) fn open_anchored(&mut self, anchor: Rect, viewport: Rect, has_words: bool) {
        let text_scale = self.text_scale;
        *self = Self::default();
        self.set_text_scale(text_scale);
        self.set_rows(Self::file_rows(has_words));
        self.placed = Some((anchor, viewport));
        self.menu.open_anchored(anchor, viewport);
    }

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

    pub(crate) fn menu_mut(&mut self) -> &mut ContextMenu {
        &mut self.menu
    }

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

    pub(crate) fn take_selection(&mut self) -> Option<FileChoice> {
        let index = self.menu.take_selection()?;
        self.choose(Some(index))
    }

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

    #[cfg(test)]
    pub(crate) fn item_center(&self, template: Template) -> Option<Point> {
        let index = self.rows.iter().position(|row| {
            matches!(row, Row::Choose(FileChoice::Template(t)) | Row::AskTemplate(t) if *t == template)
        })?;
        self.menu.item_rect(index).map(|rect| rect.center())
    }

    #[cfg(test)]
    pub(crate) fn choice_row(&self, choice: FileChoice) -> Option<usize> {
        self.rows.iter().position(|row| *row == Row::Choose(choice))
    }

    #[cfg(test)]
    pub(crate) fn choice_center(&self, choice: FileChoice) -> Option<Point> {
        let index = self
            .rows
            .iter()
            .position(|row| *row == Row::Choose(choice))?;
        self.menu.item_rect(index).map(|rect| rect.center())
    }

    #[cfg(test)]
    pub(crate) fn cancel_row(&self) -> Option<usize> {
        self.rows.iter().position(|row| *row == Row::Cancel)
    }

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

    #[test]
    fn the_file_menu_maps_rows_to_choices_at_every_scale() {
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

        {
            let mut menu = FileMenu::default();
            open(&mut menu, false);
            assert_eq!(menu.click(Point::new(0.0, 0.0)), None);
            assert!(!menu.is_open());
        }
    }

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
