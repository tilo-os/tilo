//! "Uninstall <name>?": the one question asked before an app leaves the
//! person's apps, whichever door it was asked from (Files, search, the
//! Store).
//!
//! The shared modal ([`ModalDialog`]) holds what the answer does to this
//! kind of app ([`crate::shell::app_list::AppSource::removal`]): a Flatpak's
//! "Remove its data" switch, on until the person turns it off; a downloaded
//! program's file, which goes to the Trash; for a Tilo app, that the
//! person's documents stay; and for a part of the system, that it only
//! leaves their apps. The compositor draws it, so no app can answer it for
//! the person. The answer runs [`crate::shell::app_list::uninstall`].

use std::cell::Cell;
use std::rc::Rc;

use os_actions::AppRemoval;
use ui::LocalState;
use ui::widget::{Column, ListRow, ModalDialog, Text, Toggle};

use crate::shell::app_list::{AppListItem, AppSource};
use crate::shell::i18n::{t, t_args};

/// The sheet's address; its footer buttons are `<this>.primary` and
/// `<this>.secondary`.
pub const SHEET: &str = "app-uninstall";
/// The "Remove its data" switch's address.
pub const REMOVE_DATA: &str = "app-uninstall.remove-data";

/// What the person answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// Go ahead; for a Flatpak, with its data or without.
    Uninstall {
        item: AppListItem,
        remove_data: bool,
    },
    Cancelled,
}

/// One question on screen.
pub struct Asking {
    pub item: AppListItem,
    /// The question, as the sheet's title says it.
    pub title: String,
    /// The going-ahead button's label.
    pub action: String,
    pub dialog: ModalDialog<Column>,
    /// The "Remove its data" switch; on by default, and only shown for a
    /// Flatpak.
    pub remove_data: LocalState<bool>,
    confirmed: Rc<Cell<bool>>,
    dismissed: Rc<Cell<bool>>,
}

impl Asking {
    #[must_use]
    pub fn new(item: AppListItem) -> Self {
        let remove_data = LocalState::new(true);
        let confirmed = Rc::new(Cell::new(false));
        let dismissed = Rc::new(Cell::new(false));
        let removal = item.removal();
        let (title, action) = if removal == AppRemoval::RemoveFromApps {
            (
                t_args(
                    "app-uninstall-remove-title",
                    &[("name", item.name.as_str().into())],
                ),
                t("app-uninstall-remove-action"),
            )
        } else {
            (
                t_args(
                    "app-uninstall-title",
                    &[("name", item.name.as_str().into())],
                ),
                t("app-uninstall-action"),
            )
        };
        let mut body = Column::new().with_intrinsic_height().with_no_padding();
        match (&item.source, removal) {
            (_, AppRemoval::UninstallWithData) => {
                body = body.push(Box::new(
                    ListRow::new(REMOVE_DATA, t("app-uninstall-remove-data"))
                        .secondary(t("app-uninstall-remove-data-detail"))
                        .trailing(Box::new(
                            Toggle::from_state(&remove_data)
                                .with_id(REMOVE_DATA)
                                .with_accessible_name(&t("app-uninstall-remove-data")),
                        )),
                ));
            }
            (AppSource::Downloaded { program, .. }, _) => {
                let file = program.file_name().map_or_else(
                    || program.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                );
                body = body.push(Box::new(Text::new(&t_args(
                    "app-uninstall-file-body",
                    &[("file", file.into())],
                ))));
            }
            (AppSource::Tilo { .. }, _) => {
                body = body.push(Box::new(Text::new(&t("app-uninstall-tilo-body"))));
            }
            (_, AppRemoval::RemoveFromApps) => {
                body = body.push(Box::new(Text::new(&t("app-uninstall-remove-body"))));
            }
            _ => {}
        }
        let (on_confirm, on_cancel, on_dismiss) =
            (confirmed.clone(), dismissed.clone(), dismissed.clone());
        let dialog = ModalDialog::new(title.clone(), body)
            .with_test_id(SHEET)
            .primary_action(action.clone(), move || on_confirm.set(true))
            .secondary_action(t("app-uninstall-cancel"), move || on_cancel.set(true))
            .on_dismiss(move || on_dismiss.set(true))
            .with_destructive(removal != AppRemoval::RemoveFromApps)
            .open(true);
        Self {
            item,
            title,
            action,
            dialog,
            remove_data,
            confirmed,
            dismissed,
        }
    }

    /// The person's answer, once they gave one.
    #[must_use]
    pub fn answer(&self) -> Option<Answer> {
        if self.confirmed.get() {
            return Some(Answer::Uninstall {
                item: self.item.clone(),
                remove_data: self.item.removal() == AppRemoval::UninstallWithData
                    && self.remove_data.get(),
            });
        }
        self.dismissed.get().then_some(Answer::Cancelled)
    }

    #[must_use]
    pub fn offers_data_switch(&self) -> bool {
        self.item.removal() == AppRemoval::UninstallWithData
    }
}
