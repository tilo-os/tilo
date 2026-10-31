//! "Uninstall \<name\>?": the one question asked before an app leaves the
//! person's apps, whichever door it was asked from (Files, search, the
//! Store).
//!
//! The shared modal ([`ModalDialog`]) holds what the answer does to this
//! kind of app ([`Going`]): a downloaded program's copies, which go to the
//! Trash, each named with the folder it is in; an installed app's "Remove
//! its data" switch, on until the person turns it off, and that it is
//! removed for everyone when it was installed for everyone; for a Tilo app,
//! that the person's documents stay; and for a part of the system, that it
//! only leaves their apps. The compositor draws it, so no app can answer it
//! for the person. The shell carries the answer out through the app's own
//! registry ([`tilo_app_launch::downloaded`]) and keeps the sheet current
//! through [`UninstallSeat`].

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use ui::framework::{Dispatcher, Effected, ResourceApp, ViewCx};
use ui::prelude::*;
use ui::widget::{Column, Element, ListRow, ModalDialog, Text, Toggle};

use crate::presentation::Presentation;
use crate::t;

/// The app the shell mounts the sheet as.
pub const APP_ID: &str = "so.tilo.uninstall-app";

/// The dialog, whose footer buttons are `uninstall.primary` and
/// `uninstall.secondary`.
pub const SHEET: &str = "uninstall";
/// The "Remove its data" row, and the switch in it.
pub const REMOVE_DATA: &str = "uninstall-remove-data";
pub const REMOVE_DATA_SWITCH: &str = "uninstall-remove-data-switch";
/// The rows naming each copy of a downloaded program that goes to the
/// Trash: `uninstall.copy.<index>`.
pub const COPY: &str = "uninstall-copy";

/// The sheet's resource: the door its actions are published on. It holds
/// nothing; the question is the shell's, which is what answers it.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Uninstall {}

#[resource(kind = "uninstall.question", label = "Uninstall")]
impl Uninstall {}

/// Whether the app goes from this computer or only off the person's apps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Removal {
    /// It goes from this computer.
    #[default]
    Uninstall,
    /// It stays on the computer and stops showing in the person's apps.
    FromApps,
}

/// What goes with the app, as the sheet says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Going {
    /// Every copy of a downloaded program, which go to the Trash: the file
    /// that starts it first, and `home` to name the folder each is in as the
    /// person knows it.
    Files {
        copies: Vec<PathBuf>,
        home: Option<PathBuf>,
    },
    /// The app itself, and what it saved unless the person keeps that;
    /// `device_wide` when it was installed for everyone on this computer.
    Data { device_wide: bool },
    /// Nothing on this computer changes: its entry only leaves the apps.
    Entry,
    /// What the person made in it stays in their workspaces.
    Documents,
}

impl Default for Going {
    fn default() -> Self {
        Self::Files {
            copies: Vec::new(),
            home: None,
        }
    }
}

/// The app the question is about.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct App {
    /// The entry's desktop id: the answer names it.
    pub desktop_id: String,
    /// What the person calls it.
    pub name: String,
    pub removal: Removal,
    pub going: Going,
}

/// What the person answered.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "so.tilo.uninstall")]
pub enum UninstallEffect {
    /// Uninstall it, and what it saved when the question offers that
    #[effect(
        own_device,
        icon = "trash",
        keywords = "uninstall, delete, trash, remove app, deinstallieren"
    )]
    Confirm {
        #[param("Remove what the app saved")]
        remove_data: Option<bool>,
    },
    /// Keep it
    #[effect(
        own_device,
        kind = "read",
        icon = "x",
        keywords = "keep, cancel, abbrechen"
    )]
    Cancel,
}

/// What the shell lends the sheet: which app is asked about, whether its
/// data goes with it, and what the person answered.
#[derive(Clone, Default)]
pub struct UninstallSeat {
    presentation: Presentation,
    app: Rc<RefCell<Option<App>>>,
    shown: Rc<RefCell<Option<LocalState<App>>>>,
    remove_data: Rc<RefCell<Option<LocalState<bool>>>>,
    asked: Rc<RefCell<Vec<UninstallEffect>>>,
}

impl std::fmt::Debug for UninstallSeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UninstallSeat")
            .field("app", &self.app.borrow())
            .finish_non_exhaustive()
    }
}

impl crate::Seat for UninstallSeat {
    fn presentation(&self) -> &Presentation {
        &self.presentation
    }
}

impl UninstallSeat {
    /// Ask about `app`, as a new question: what the app saved goes with it
    /// until the person says otherwise.
    pub fn show(&self, app: App) {
        if *self.app.borrow() == Some(app.clone()) {
            return;
        }
        if let Some(shown) = self.shown.borrow().as_ref() {
            shown.set(app.clone());
        }
        if let Some(remove_data) = self.remove_data.borrow().as_ref() {
            remove_data.set(true);
        }
        *self.app.borrow_mut() = Some(app);
    }

    /// The app asked about now.
    #[must_use]
    pub fn app(&self) -> Option<App> {
        self.app.borrow().clone()
    }

    /// Whether the sheet offers "Remove its data" now.
    #[must_use]
    pub fn offers_data_switch(&self) -> bool {
        self.app
            .borrow()
            .as_ref()
            .is_some_and(|app| matches!(app.going, Going::Data { .. }))
    }

    /// The answers made since the shell last asked, oldest first.
    pub fn take_asked(&self) -> Vec<UninstallEffect> {
        std::mem::take(&mut *self.asked.borrow_mut())
    }
}

/// The sheet as an app.
pub struct UninstallApp;

impl ResourceApp for UninstallApp {
    type Resource = Uninstall;
    const ID: &'static str = APP_ID;
    const TITLE: &'static str = "Uninstall";
    const ICON: Option<&'static str> = Some("trash");
    // Over the desktop: what is not the sheet shows the desktop under it.
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        window: ui::application::WindowRole::Sheet,
        ..ui::application::ComponentBehaviour::DEFAULT
    };
    // Nothing is kept: there is no past to look back on.
    const DOCUMENT_HISTORY: bool = false;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::CATALOG)
    }

    fn view(cx: &mut ViewCx<Uninstall>) -> Element {
        let seat = cx.take_lent::<UninstallSeat>();
        let initial = match &seat {
            Some(seat) => seat.app(),
            None => cx.fixture::<App>().map(|app| (*app).clone()),
        };
        let shown = cx.local(initial.unwrap_or_default());
        let remove_data = cx.local(true);
        if let Some(seat) = &seat {
            *seat.shown.borrow_mut() = Some(shown.clone());
            *seat.remove_data.borrow_mut() = Some(remove_data.clone());
        }
        let answering = seat;
        cx.effects(move |effect: UninstallEffect, _cx| match &answering {
            Some(seat) => {
                seat.asked.borrow_mut().push(effect);
                Ok(Effected::done())
            }
            None => Err("no apps to uninstall here".to_owned()),
        });
        let dispatch = cx.dispatcher();
        cx.switch(&shown, move |_cx, app| card(app, &remove_data, &dispatch))
    }

    fn scenarios() -> &'static [&'static str] {
        &["copies", "one-copy", "installed", "from-apps"]
    }

    fn fixture(fixture: &mut ui::framework::Fixture<Uninstall>) {
        let app = match fixture.scenario() {
            Some("one-copy") => App {
                going: Going::Files {
                    copies: vec![PathBuf::from("/home/ute/Downloads/tilo-imager-linux")],
                    home: Some(PathBuf::from("/home/ute")),
                },
                ..sample_app()
            },
            Some("installed") => App {
                name: "Transmission".to_owned(),
                going: Going::Data { device_wide: false },
                ..sample_app()
            },
            Some("from-apps") => App {
                name: "Transmission".to_owned(),
                removal: Removal::FromApps,
                going: Going::Entry,
                ..sample_app()
            },
            _ => App {
                going: Going::Files {
                    copies: vec![
                        PathBuf::from("/home/ute/Applications/tilo-imager.AppImage"),
                        PathBuf::from("/home/ute/Downloads/tilo-imager.AppImage"),
                    ],
                    home: Some(PathBuf::from("/home/ute")),
                },
                ..sample_app()
            },
        };
        fixture.provide(app);
    }
}

/// A downloaded app as the sheet asks about it, for captures.
fn sample_app() -> App {
    App {
        desktop_id: "tilo-downloaded-tilo-imager".to_owned(),
        name: "Tilo Imager".to_owned(),
        removal: Removal::Uninstall,
        going: Going::default(),
    }
}

/// The question about `app`: what goes, the data switch when it is offered,
/// and the two answers.
fn card(app: &App, remove_data: &LocalState<bool>, dispatch: &Dispatcher<Uninstall>) -> Element {
    let name = [("name", ui::i18n::ArgValue::Text(app.name.clone()))];
    let (title, action) = match app.removal {
        Removal::Uninstall => (
            crate::t_args("uninstall-title", &name),
            t("uninstall-action"),
        ),
        Removal::FromApps => (
            crate::t_args("uninstall-from-apps-title", &name),
            t("uninstall-from-apps-action"),
        ),
    };
    let mut body = Column::new().with_intrinsic_height().with_no_padding();
    match &app.going {
        Going::Files { copies, home } => {
            body = body.push(Box::new(Text::new(&crate::t_args(
                "uninstall-files",
                &[("count", ui::i18n::ArgValue::Number(count_of(copies.len())))],
            ))));
            for (index, copy) in copies.iter().enumerate() {
                let (file, folder) = copy_label(copy, home.as_deref());
                body = body.push(Box::new(
                    ListRow::new(format!("{COPY}-{index}"), file).secondary(folder),
                ));
            }
        }
        Going::Data { device_wide } => {
            if *device_wide {
                body = body.push(Box::new(Text::new(&t("uninstall-device-body"))));
            }
            body = body.push(Box::new(
                ListRow::new(REMOVE_DATA, t("uninstall-remove-data"))
                    .secondary(t("uninstall-remove-data-detail"))
                    .trailing(Box::new(
                        Toggle::from_state(remove_data)
                            .with_id(REMOVE_DATA_SWITCH)
                            .with_accessible_name(&t("uninstall-remove-data")),
                    )),
            ));
        }
        Going::Entry => {
            body = body.push(Box::new(Text::new(&t("uninstall-entry-body"))));
        }
        Going::Documents => {
            body = body.push(Box::new(Text::new(&t("uninstall-documents-body"))));
        }
    }
    let confirming = dispatch.clone();
    let offers_data = matches!(app.going, Going::Data { .. });
    let toggled = remove_data.clone();
    let cancelling = dispatch.clone();
    let dismissed = dispatch.clone();
    Box::new(
        ModalDialog::new(title, body)
            .with_test_id(SHEET)
            .with_max_width(f64::from(ui::tokens::Metrics::BASE.panel_width_sheet))
            .open(true)
            .with_destructive(app.removal != Removal::FromApps)
            .primary_action(action, move || {
                confirming.effect(&UninstallEffect::Confirm {
                    remove_data: offers_data.then(|| toggled.get()),
                });
            })
            .secondary_action(t("uninstall-cancel"), move || {
                cancelling.effect(&UninstallEffect::Cancel);
            })
            .on_dismiss(move || dismissed.effect(&UninstallEffect::Cancel)),
    )
}

/// `count` as the catalogue's plural rules read it.
fn count_of(count: usize) -> f64 {
    u32::try_from(count).map_or(f64::MAX, f64::from)
}

/// A copy's file name and the folder it is in, as the person knows it:
/// `Downloads/tilo-imager` under their home, the full path elsewhere, and
/// their home folder itself named as the sheet names it.
fn copy_label(copy: &Path, home: Option<&Path>) -> (String, String) {
    let name = copy.file_name().map_or_else(
        || copy.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let folder = copy.parent().map_or_else(String::new, |folder| {
        match home.and_then(|home| folder.strip_prefix(home).ok()) {
            Some(inside) if inside.as_os_str().is_empty() => t("uninstall-home-folder"),
            Some(inside) => inside.display().to_string(),
            None => folder.display().to_string(),
        }
    });
    (name, folder)
}

/// The app's definition, which the shell mounts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(UninstallApp)
}
