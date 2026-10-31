//! "Add \<name\> to apps?": asked once, the first time a program the person
//! downloaded runs ([`tilo_app_launch::downloaded`]). The program has
//! already started; the question only decides whether it gets an entry in
//! the person's apps.
//!
//! The shared modal ([`ModalDialog`]) shows the app as it will be listed
//! (its artwork, its name, the file it runs from) and only answers that
//! can't break anything:
//!
//! - Add to apps, the default.
//! - Move to Applications, offered only for an `AppImage`: one
//!   self-contained file by design. A program from a tarball or a loose
//!   binary may need the files beside it, so it is never offered a move.
//!
//! Any other way of closing it is an answer too: that file is not asked
//! about again. The shell carries the answer out through the downloaded
//! programs' own registry and keeps the sheet current through
//! [`AddSeat`].

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use ui::framework::{Dispatcher, Effected, ResourceApp, ViewCx};
use ui::icons::Icon;
use ui::prelude::*;
use ui::widget::{Column, Element, ListRow, MediaAspect, MediaSource, ModalDialog, Toggle};

use crate::presentation::Presentation;
use crate::t;

/// The app the shell mounts the sheet as.
pub const APP_ID: &str = "so.tilo.add-app";

/// The dialog, whose footer buttons are `add-app.primary` and
/// `add-app.secondary`.
pub const SHEET: &str = "add-app";
/// The row showing the app.
pub const APP: &str = "add-app-row";
/// The "Move to Applications" row, and the switch in it.
pub const MOVE: &str = "add-app-move";
pub const MOVE_SWITCH: &str = "add-app-move-switch";

/// The largest file whose bytes are read as a program's artwork: past it,
/// the row keeps the system's generic application icon.
const MAX_ARTWORK_BYTES: u64 = 8 * 1024 * 1024;

/// The sheet's resource: the door its actions are published on. It holds
/// nothing; the question is the shell's, which is what answers it.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct AddToApps {}

#[resource(kind = "add-app.question", label = "Add to apps")]
impl AddToApps {}

/// What the person is asked about: a program that just started and has no
/// entry in their apps.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Offer {
    /// The file that starts it; its answer names it.
    pub file: PathBuf,
    /// What it calls itself.
    pub name: String,
    /// Whether the file is an `AppImage`, which alone may be moved.
    pub appimage: bool,
    /// Its artwork, when the program brings any: an absolute file, or an
    /// icon name in the person's themes.
    pub artwork: Option<PathBuf>,
}

impl Offer {
    /// The file's name, as the row names it.
    #[must_use]
    pub fn file_name(&self) -> String {
        self.file.file_name().map_or_else(
            || self.file.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
    }
}

/// What the person answered.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "so.tilo.add-app")]
pub enum AddAppEffect {
    /// Add this program to your apps
    #[effect(
        own_device,
        icon = "plus",
        keywords = "add app, appimage, downloaded program, add to apps, hinzufügen"
    )]
    Add {
        #[param("Move the AppImage into the Applications folder")]
        move_to_applications: Option<bool>,
    },
    /// Do not add it, and do not ask about this file again
    #[effect(
        own_device,
        idempotent,
        icon = "x",
        keywords = "do not add, decline, ignore, downloaded program, ablehnen"
    )]
    Decline,
}

/// What the shell lends the sheet: which program is asked about, the switch
/// beside it, and what the person answered.
#[derive(Clone, Default)]
pub struct AddSeat {
    presentation: Presentation,
    offer: Rc<RefCell<Option<Offer>>>,
    shown: Rc<RefCell<Option<LocalState<Offer>>>>,
    moved: Rc<RefCell<Option<LocalState<bool>>>>,
    asked: Rc<RefCell<Vec<AddAppEffect>>>,
}

impl std::fmt::Debug for AddSeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AddSeat")
            .field("offer", &self.offer.borrow())
            .finish_non_exhaustive()
    }
}

impl crate::Seat for AddSeat {
    fn presentation(&self) -> &Presentation {
        &self.presentation
    }
}

impl AddSeat {
    /// Ask about `offer`, as a new question: the switch starts off, as
    /// adding where the file is does.
    pub fn show(&self, offer: Offer) {
        if *self.offer.borrow() == Some(offer.clone()) {
            return;
        }
        if let Some(shown) = self.shown.borrow().as_ref() {
            shown.set(offer.clone());
        }
        if let Some(moved) = self.moved.borrow().as_ref() {
            moved.set(false);
        }
        *self.offer.borrow_mut() = Some(offer);
    }

    /// The program asked about now.
    #[must_use]
    pub fn offer(&self) -> Option<Offer> {
        self.offer.borrow().clone()
    }

    /// Whether the sheet offers "Move to Applications" now.
    #[must_use]
    pub fn offers_move(&self) -> bool {
        self.offer
            .borrow()
            .as_ref()
            .is_some_and(|offer| offer.appimage)
    }

    /// The answers made since the shell last asked, oldest first.
    pub fn take_asked(&self) -> Vec<AddAppEffect> {
        std::mem::take(&mut *self.asked.borrow_mut())
    }
}

/// The sheet as an app.
pub struct AddApp;

impl ResourceApp for AddApp {
    type Resource = AddToApps;
    const ID: &'static str = APP_ID;
    const TITLE: &'static str = "Add to apps";
    const ICON: Option<&'static str> = Some("plus");
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

    fn view(cx: &mut ViewCx<AddToApps>) -> Element {
        let seat = cx.take_lent::<AddSeat>();
        let initial = match &seat {
            Some(seat) => seat.offer(),
            None => cx.fixture::<Offer>().map(|offer| (*offer).clone()),
        };
        let shown = cx.local(initial.unwrap_or_default());
        let moved = cx.local(false);
        if let Some(seat) = &seat {
            *seat.shown.borrow_mut() = Some(shown.clone());
            *seat.moved.borrow_mut() = Some(moved.clone());
        }
        let answering = seat;
        cx.effects(move |effect: AddAppEffect, _cx| match &answering {
            Some(seat) => {
                seat.asked.borrow_mut().push(effect);
                Ok(Effected::done())
            }
            None => Err("no apps to add this program to here".to_owned()),
        });
        let dispatch = cx.dispatcher();
        cx.switch(&shown, move |_cx, offer| card(offer, &moved, &dispatch))
    }

    fn scenarios() -> &'static [&'static str] {
        &["appimage", "program"]
    }

    fn fixture(fixture: &mut ui::framework::Fixture<AddToApps>) {
        let appimage = fixture.scenario() != Some("program");
        fixture.provide(Offer {
            file: if appimage {
                PathBuf::from("/home/ute/Downloads/krita-5.2.AppImage")
            } else {
                PathBuf::from("/home/ute/Downloads/krita-5.2/bin/krita")
            },
            name: "Krita".to_owned(),
            appimage,
            artwork: None,
        });
    }
}

/// The question about `offer`: the app as it will be listed, the move when
/// the file may move, and the two answers.
fn card(offer: &Offer, moved: &LocalState<bool>, dispatch: &Dispatcher<AddToApps>) -> Element {
    let title = crate::t_args(
        "add-app-title",
        &[("name", ui::i18n::ArgValue::Text(offer.name.clone()))],
    );
    let app = ListRow::new(APP, offer.name.clone()).secondary(offer.file_name());
    let app = match &offer.artwork {
        Some(artwork) => app.media(
            MediaSource::Url {
                store: artwork_store(),
                url: artwork.display().to_string(),
            },
            MediaAspect::Square,
        ),
        None => app.icon(Icon::AppWindow),
    };
    let mut body = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .push(Box::new(app));
    if offer.appimage {
        body = body.push(Box::new(
            ListRow::new(MOVE, t("add-app-move"))
                .secondary(t("add-app-move-detail"))
                .trailing(Box::new(
                    Toggle::from_state(moved)
                        .with_id(MOVE_SWITCH)
                        .with_accessible_name(&t("add-app-move")),
                )),
        ));
    }
    let adding = dispatch.clone();
    let toggled = moved.clone();
    let declining = dispatch.clone();
    let dismissed = dispatch.clone();
    Box::new(
        ModalDialog::new(title, body)
            .with_test_id(SHEET)
            .with_max_width(f64::from(ui::tokens::Metrics::BASE.panel_width_sheet))
            .open(true)
            .primary_action(t("add-app-add"), move || {
                adding.effect(&AddAppEffect::Add {
                    move_to_applications: toggled.get().then_some(true),
                });
            })
            .secondary_action(t("add-app-decline"), move || {
                declining.effect(&AddAppEffect::Decline);
            })
            .on_dismiss(move || dismissed.effect(&AddAppEffect::Decline)),
    )
}

/// The pictures the rows lead with: the artwork each program brings, read
/// and decoded off the UI thread the first time it is painted. Made once per
/// thread, like any image store.
fn artwork_store() -> ui::ImageStore {
    thread_local! {
        static STORE: std::cell::OnceCell<ui::ImageStore> = const { std::cell::OnceCell::new() };
    }
    STORE.with(|store| {
        store
            .get_or_init(|| {
                let fetch: ui::Fetcher = std::sync::Arc::new(|path: &str| {
                    // Artwork, not a document: a file too large to be one is
                    // read as none rather than into memory.
                    let size = std::fs::metadata(path).ok()?.len();
                    if size > MAX_ARTWORK_BYTES {
                        return None;
                    }
                    std::fs::read(path).ok()
                });
                ui::ImageStore::new(&fetch)
            })
            .clone()
    })
}

/// The app's definition, which the shell mounts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(AddApp)
}
