//! "Add \<name\> to apps?": asked once, the first time a downloaded program
//! runs ([`crate::shell::downloaded_apps`]). The program has already
//! started; the question only decides whether it gets an entry in the
//! person's apps.
//!
//! The shared modal ([`ModalDialog`]) shows the app as it will be listed
//! (its icon, its name, the file it runs from) and only answers that can't
//! break anything:
//!
//! - Add to apps, the default.
//! - Move to Applications, offered only for an `AppImage`: one
//!   self-contained file by design. A program from a tarball or a loose
//!   binary may need the files beside it, so it is never offered a move.
//!
//! Any other way of closing it is an answer too: that file is not asked
//! about again.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use ui::LocalState;
use ui::widget::{Column, ListRow, ModalDialog, Toggle};

use crate::shell::i18n::{t, t_args};

/// The sheet's address; its footer buttons are `<this>.primary` and
/// `<this>.secondary`.
pub const SHEET: &str = "downloaded-app-add";
/// The row showing the app.
pub const APP: &str = "downloaded-app-add.app";
/// The "Move to Applications" switch's address.
pub const MOVE: &str = "downloaded-app-add.move";

/// What there is to ask about one running program.
#[derive(Clone, Debug)]
pub struct Ask {
    /// The window the program showed; its answer is filed under it.
    pub surface_id: u64,
    pub name: String,
    /// The file it runs from.
    pub file: PathBuf,
    /// Whether the file is an `AppImage`, which alone may be moved.
    pub appimage: bool,
    /// Its icon as its entry will show it, drawn at the row's leading
    /// size: its own artwork, else the system's generic application icon.
    pub icon: Option<crate::app_icons::Raster>,
}

/// What the person answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Add {
        surface_id: u64,
        move_to_applications: bool,
    },
    /// Don't add, and don't ask about this file again.
    Declined { surface_id: u64 },
}

/// One question on screen.
pub struct Asking {
    pub ask: Ask,
    /// The question, as the sheet's title says it.
    pub title: String,
    pub dialog: ModalDialog<Column>,
    /// "Move to Applications"; off by default, and only shown for an
    /// `AppImage`.
    pub move_to_applications: LocalState<bool>,
    added: Rc<Cell<bool>>,
    declined: Rc<Cell<bool>>,
}

impl Asking {
    /// The sheet for `ask`.
    #[must_use]
    pub fn new(ask: Ask) -> Self {
        let move_to_applications = LocalState::new(false);
        let added = Rc::new(Cell::new(false));
        let declined = Rc::new(Cell::new(false));
        let title = t_args(
            "downloaded-app-add-title",
            &[("name", ask.name.as_str().into())],
        );
        let file = ask.file.file_name().map_or_else(
            || ask.file.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let app = ListRow::new(APP, ask.name.clone()).secondary(file);
        let app = match &ask.icon {
            Some(icon) => app.thumbnail(icon.image_data()),
            None => app.icon(ui::icons::Icon::AppWindow),
        };
        let mut body = Column::new()
            .with_intrinsic_height()
            .with_no_padding()
            .push(Box::new(app));
        if ask.appimage {
            body = body.push(Box::new(
                ListRow::new(MOVE, t("downloaded-app-move"))
                    .secondary(t("downloaded-app-move-detail"))
                    .trailing(Box::new(
                        Toggle::from_state(&move_to_applications)
                            .with_id(MOVE)
                            .with_accessible_name(&t("downloaded-app-move")),
                    )),
            ));
        }
        let (on_add, on_decline, on_dismiss) = (added.clone(), declined.clone(), declined.clone());
        let mut dialog = ModalDialog::new(title.clone(), body)
            .with_test_id(SHEET)
            .primary_action(t("downloaded-app-add-action"), move || on_add.set(true))
            .secondary_action(t("downloaded-app-add-decline"), move || {
                on_decline.set(true);
            })
            .on_dismiss(move || on_dismiss.set(true));
        // Asked now: it arrives (`motion::Appear`), from a click on a
        // motion, from a key or an action at once.
        dialog.set_open(true);
        Self {
            ask,
            title,
            dialog,
            move_to_applications,
            added,
            declined,
        }
    }

    /// The person's answer, once they gave one.
    #[must_use]
    pub fn answer(&self) -> Option<Answer> {
        let surface_id = self.ask.surface_id;
        if self.added.get() {
            return Some(Answer::Add {
                surface_id,
                move_to_applications: self.ask.appimage && self.move_to_applications.get(),
            });
        }
        self.declined
            .get()
            .then_some(Answer::Declined { surface_id })
    }

    /// Whether the sheet offers "Move to Applications".
    #[must_use]
    pub fn offers_move(&self) -> bool {
        self.ask.appimage
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(appimage: bool) -> Ask {
        Ask {
            surface_id: 7,
            name: "Krita".into(),
            file: if appimage {
                "/home/ute/Downloads/krita-5.2.AppImage".into()
            } else {
                "/home/ute/Downloads/krita-5.2/bin/krita".into()
            },
            appimage,
            icon: None,
        }
    }

    fn holds(dialog: &ModalDialog<Column>, id: &str) -> bool {
        fn walk(widget: &dyn ui::widget::Widget, id: &str) -> bool {
            widget.test_id() == Some(id) || widget.children().into_iter().any(|w| walk(w, id))
        }
        walk(dialog.content(), id)
    }

    /// The person presses the footer button `secondary` or the primary one
    /// on the sheet as shown.
    fn press(sheet: &mut Asking, secondary: bool) {
        let dialog = std::mem::replace(&mut sheet.dialog, ModalDialog::new("", Column::new()));
        let mut framed = ui::headless::Framed::new(
            ui::State::new(),
            dialog,
            (1280, 800),
            ui::theme::Theme::dark(),
        );
        let _ = ui::headless::Framed::harness_mut(&mut framed).poll_frame();
        let button = if secondary {
            framed.secondary_rect().center()
        } else {
            framed.primary_rect().center()
        };
        ui::headless::Framed::harness_mut(&mut framed).click_at(button.x, button.y);
    }

    #[test]
    fn only_an_appimage_is_offered_a_move() {
        let appimage = Asking::new(ask(true));
        assert_eq!(appimage.title, "Add Krita to apps?");
        assert!(appimage.offers_move() && holds(&appimage.dialog, MOVE));
        assert!(
            !appimage.move_to_applications.get(),
            "adding is the default"
        );
        assert_eq!(appimage.answer(), None, "nothing happens before an answer");

        let program = Asking::new(ask(false));
        assert!(!program.offers_move() && !holds(&program.dialog, MOVE));
        assert!(holds(&program.dialog, APP));
    }

    #[test]
    fn adding_carries_the_move_switch() {
        let mut sheet = Asking::new(ask(true));
        sheet.move_to_applications.set(true);
        press(&mut sheet, false);
        assert_eq!(
            sheet.answer(),
            Some(Answer::Add {
                surface_id: 7,
                move_to_applications: true
            })
        );
    }

    #[test]
    fn declining_is_an_answer() {
        let mut sheet = Asking::new(ask(false));
        press(&mut sheet, true);
        assert_eq!(sheet.answer(), Some(Answer::Declined { surface_id: 7 }));
    }
}
