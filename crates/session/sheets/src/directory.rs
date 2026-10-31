//! "Join a workspace…": the public workspaces a person can join, the way
//! chat apps let people browse and join servers.
//!
//! The sheet lists what the sites this computer knows serve in their
//! directories, the site's own community workspace first: each with its
//! name, what it is, who is in it now and how lively it is, and Join (Open
//! once this computer holds it). Search narrows the list as the person
//! types; Enter hands what was typed to the shell, which looks a site up or
//! takes a link or an invitation code to the join field. The shell reads
//! the directories and joins; it keeps the sheet current through
//! [`DirectorySeat`].

use std::cell::RefCell;
use std::rc::Rc;

use ui::advanced::WindowEvent;
use ui::framework::{Dispatcher, Effected, ResourceApp, ViewCx};
use ui::prelude::*;
use ui::widget::{
    Avatar, Button, ButtonRole, Column, Element, ListRow, ModalDialog, Row, Text, TextInput, Widget,
};

use crate::presentation::Presentation;
use crate::t;

/// The app the shell mounts the sheet as.
pub const APP_ID: &str = "so.tilo.workspace-directory";

/// Faces shown before the rest collapse into "+N".
const FACES_SHOWN: usize = 4;

/// The sheet's resource: the door its actions are published on. It holds
/// nothing; the directories are the sites'.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct WorkspaceDirectory {}

#[resource(kind = "workspace-directory.listing", label = "Public workspaces")]
impl WorkspaceDirectory {}

/// What the sheet asks of the shell.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "so.tilo.workspace-directory.join")]
pub enum DirectoryEffect {
    /// Join a public workspace
    #[effect(own_device)]
    Join {
        #[param("The workspace, by its key in the list")]
        key: String,
    },
    /// Open a workspace this computer holds
    #[effect(own_device, kind = "read")]
    Open {
        #[param("The workspace, by its key in the list")]
        key: String,
    },
    /// Look a site up, or join with a link or an invitation code
    #[effect(own_device, kind = "read")]
    Look {
        #[param("A site, a link or an invitation code")]
        text: String,
    },
    /// Join with a link or an invitation code
    #[effect(own_device, kind = "read")]
    LinkOrCode {
        #[param("The link or code typed so far")]
        text: String,
    },
    /// Read the directories again
    #[effect(own_device, kind = "read")]
    Refresh,
    /// Close the sheet
    #[effect(own_device, kind = "read")]
    Done,
}

/// What a listing's button does now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ListingAction {
    #[default]
    Join,
    /// This computer holds it already.
    Open,
    /// Joining it is on its way.
    Joining,
    /// The last join failed; pressing tries again.
    TryAgain,
}

/// One public workspace, as the sheet shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    /// What it is joined and found again by.
    pub key: String,
    pub name: String,
    /// What it is, or the host listing it when it says nothing.
    pub about: String,
    /// What the search matches besides its name and what it is: its host.
    pub host: String,
    /// Whose own workspace it is, for a site's community workspace.
    pub tag: Option<String>,
    /// How lively it is and how many belong, or why the last join failed.
    pub activity: String,
    /// The role people who join get, as its button describes it.
    pub joins_as: String,
    /// Who is in it now: names and identities, and how many in all.
    pub here: Vec<(String, String)>,
    pub here_count: usize,
    pub action: ListingAction,
    /// Its button waits: another join is running.
    pub waiting: bool,
}

impl Listing {
    fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        query.is_empty()
            || [&self.name, &self.about, &self.host]
                .iter()
                .any(|field| field.to_lowercase().contains(&query))
    }
}

/// Where the list stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Status {
    /// Asking the directories.
    #[default]
    Loading,
    Listed,
    /// No directory answered; why.
    Failed(String),
}

/// What the sheet shows: plain data the shell keeps current.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Directory {
    pub status: Status,
    pub listings: Vec<Listing>,
    /// What became of the last join or read, when it went wrong.
    pub note: Option<String>,
}

/// What the shell lends the sheet: whether it is up, what it lists, and
/// where its requests go.
#[derive(Clone, Default)]
pub struct DirectorySeat {
    presentation: Presentation,
    directory: Rc<RefCell<Directory>>,
    shown: Rc<RefCell<Option<LocalState<Directory>>>>,
    query: Rc<RefCell<Option<LocalState<String>>>>,
    asked: Rc<RefCell<Vec<DirectoryEffect>>>,
}

impl std::fmt::Debug for DirectorySeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirectorySeat")
            .field("presentation", &self.presentation)
            .finish_non_exhaustive()
    }
}

impl crate::Seat for DirectorySeat {
    fn presentation(&self) -> &Presentation {
        &self.presentation
    }
}

impl DirectorySeat {
    /// Show `directory`, or the same directory as it stands now.
    pub fn show(&self, directory: Directory) {
        if *self.directory.borrow() == directory {
            return;
        }
        if let Some(shown) = self.shown.borrow().as_ref() {
            shown.set(directory.clone());
        }
        *self.directory.borrow_mut() = directory;
    }

    /// What the sheet lists now.
    pub fn directory(&self) -> Directory {
        self.directory.borrow().clone()
    }

    /// What was typed into the search.
    pub fn query(&self) -> String {
        self.query
            .borrow()
            .as_ref()
            .map(LocalState::get)
            .unwrap_or_default()
    }

    /// Start the search afresh, as a new opening does.
    pub fn clear_query(&self) {
        if let Some(query) = self.query.borrow().as_ref() {
            query.set(String::new());
        }
    }

    /// The requests made since the shell last asked, oldest first.
    pub fn take_asked(&self) -> Vec<DirectoryEffect> {
        std::mem::take(&mut *self.asked.borrow_mut())
    }
}

/// The sheet as an app.
pub struct DirectoryApp;

impl ResourceApp for DirectoryApp {
    type Resource = WorkspaceDirectory;
    const ID: &'static str = APP_ID;
    const TITLE: &'static str = "Join a workspace";
    const ICON: Option<&'static str> = Some("users");
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

    fn view(cx: &mut ViewCx<WorkspaceDirectory>) -> Element {
        let seat = cx.take_lent::<DirectorySeat>();
        let initial = match &seat {
            Some(seat) => seat.directory(),
            None => cx
                .fixture::<Directory>()
                .map(|directory| (*directory).clone())
                .unwrap_or_default(),
        };
        let shown = cx.local(initial);
        let query = cx.local(String::new());
        if let Some(seat) = &seat {
            *seat.shown.borrow_mut() = Some(shown.clone());
            *seat.query.borrow_mut() = Some(query.clone());
        }
        let asking = seat;
        cx.effects(move |effect: DirectoryEffect, _cx| match &asking {
            Some(seat) => {
                if effect == DirectoryEffect::Done {
                    seat.presentation.gone();
                }
                seat.asked.borrow_mut().push(effect);
                Ok(Effected::done())
            }
            None => Err("no network to join through here".to_owned()),
        });
        let dispatch = cx.dispatcher();
        let field = query.clone();
        let rows_query = query.clone();
        let rows_dispatch = dispatch.clone();
        let sheet_dispatch = dispatch.clone();
        let content = cx.switch(&shown, move |cx, directory| {
            let directory = directory.clone();
            let dispatch = rows_dispatch.clone();
            let rows = cx.switch(&rows_query, move |_cx, query| {
                rows(&directory, query, &dispatch)
            });
            sheet(rows, &field, &sheet_dispatch)
        });
        Box::new(Searching {
            content,
            dispatch,
            query,
        })
    }

    fn scenarios() -> &'static [&'static str] {
        &["listed", "loading", "failed", "joining"]
    }

    fn fixture(fixture: &mut ui::framework::Fixture<WorkspaceDirectory>) {
        let directory = match fixture.scenario() {
            Some("loading") => Directory::default(),
            Some("failed") => Directory {
                status: Status::Failed("no site answered".to_owned()),
                ..Directory::default()
            },
            Some("joining") => {
                let mut directory = sample_directory();
                if let Some(first) = directory.listings.first_mut() {
                    first.action = ListingAction::Joining;
                }
                for listing in directory.listings.iter_mut().skip(1) {
                    listing.waiting = true;
                }
                directory
            }
            _ => sample_directory(),
        };
        fixture.provide(directory);
    }
}

/// Public workspaces as a fresh computer finds them, for captures.
fn sample_directory() -> Directory {
    let listing = |key: &str, name: &str, about: &str, here: usize| Listing {
        key: key.to_owned(),
        name: name.to_owned(),
        about: about.to_owned(),
        host: "tilo.coffee".to_owned(),
        tag: None,
        activity: format!("{here} here now"),
        joins_as: "You join as an editor".to_owned(),
        here: (0..here.min(FACES_SHOWN))
            .map(|n| (format!("Person {n}"), format!("person-{n}")))
            .collect(),
        here_count: here,
        action: ListingAction::Join,
        waiting: false,
    };
    Directory {
        status: Status::Listed,
        listings: vec![
            Listing {
                tag: Some("tilo.coffee's own".to_owned()),
                ..listing("coffee", "Tilo Coffee", "The community of tilo.coffee", 6)
            },
            listing("garden", "Gardeners", "Seeds, soil and swaps", 2),
            Listing {
                action: ListingAction::Open,
                ..listing("chess", "Chess club", "Weekly games", 0)
            },
        ],
        note: None,
    }
}

/// The sheet around `rows`: the search, the list, the way in with a link.
fn sheet(
    rows: Element,
    query: &LocalState<String>,
    dispatch: &Dispatcher<WorkspaceDirectory>,
) -> Element {
    let link = dispatch.clone();
    let typed = query.clone();
    let dismissed = dispatch.clone();
    let search = TextInput::from_state(query)
        .with_placeholder(t("directory-search"))
        .with_id("directory-search");
    let body = Column::new()
        .with_gap(ui::tokens::Spacing::BASE.s3)
        .push(Box::new(search))
        .push(rows);
    Box::new(
        ModalDialog::new(t("directory-title"), body)
            .with_test_id("directory")
            .with_max_width(f64::from(ui::tokens::Metrics::BASE.panel_width_sheet))
            .open(true)
            .secondary_action(t("directory-link-or-code"), move || {
                link.effect(&DirectoryEffect::LinkOrCode { text: typed.get() });
            })
            .on_dismiss(move || dismissed.effect(&DirectoryEffect::Done)),
    )
}

/// The listings that answer `query`, or what stands in their place.
fn rows(directory: &Directory, query: &str, dispatch: &Dispatcher<WorkspaceDirectory>) -> Element {
    let caption = |text: &str| -> Element {
        Box::new(
            Text::new(text)
                .with_role(TextRole::Caption)
                .with_id("directory-status"),
        )
    };
    let mut column = Column::new().with_gap(ui::tokens::Spacing::BASE.s1);
    if let Some(note) = &directory.note {
        column = column.push(caption(note));
    }
    match &directory.status {
        Status::Loading => return Box::new(column.push(caption(&t("directory-loading")))),
        Status::Failed(error) => {
            let retry = dispatch.clone();
            return Box::new(
                column
                    .push(caption(&crate::t_args(
                        "directory-failed",
                        &[("error", ui::i18n::ArgValue::Text(error.clone()))],
                    )))
                    .push(Box::new(
                        Button::new("directory-retry", &t("directory-try-again"))
                            .on_view(move || retry.effect(&DirectoryEffect::Refresh)),
                    )),
            );
        }
        Status::Listed => {}
    }
    let shown: Vec<&Listing> = directory
        .listings
        .iter()
        .filter(|listing| listing.matches(query))
        .collect();
    if shown.is_empty() {
        let says = if directory.listings.is_empty() {
            t("directory-empty")
        } else {
            crate::t_args(
                "directory-no-match",
                &[("query", ui::i18n::ArgValue::Text(query.trim().to_owned()))],
            )
        };
        return Box::new(column.push(caption(&says)));
    }
    for listing in shown {
        column = column.push(row(listing, dispatch));
    }
    Box::new(column)
}

/// One workspace: its mark, name and what it is, who is here, its button.
fn row(listing: &Listing, dispatch: &Dispatcher<WorkspaceDirectory>) -> Element {
    let (label, named, primary) = match listing.action {
        ListingAction::Open => ("directory-open", "directory-open-named", false),
        ListingAction::Joining => ("directory-joining", "directory-join-named", true),
        ListingAction::TryAgain => ("directory-try-again", "directory-join-named", true),
        ListingAction::Join => ("directory-join", "directory-join-named", true),
    };
    let press = dispatch.clone();
    let effect = match listing.action {
        ListingAction::Open => DirectoryEffect::Open {
            key: listing.key.clone(),
        },
        _ => DirectoryEffect::Join {
            key: listing.key.clone(),
        },
    };
    let mut button = Button::new(&format!("directory-row-{}", listing.key), &t(label))
        .with_accessible_name(&crate::t_args(
            named,
            &[("name", ui::i18n::ArgValue::Text(listing.name.clone()))],
        ))
        .tooltip(listing.joins_as.clone())
        .on_own_device_effect(move || press.effect(&effect))
        .with_enabled(!listing.waiting && listing.action != ListingAction::Joining);
    if primary {
        button = button.role(ButtonRole::Primary);
    }
    let mut trailing = Row::new().with_gap(ui::tokens::Spacing::BASE.s1);
    for (name, identity) in listing.here.iter().take(FACES_SHOWN) {
        trailing = trailing.push(Box::new(Avatar::person(name.clone(), identity.clone())));
    }
    let extra = listing
        .here_count
        .max(listing.here.len())
        .saturating_sub(listing.here.len().min(FACES_SHOWN));
    if extra > 0 {
        trailing = trailing.push(Box::new(
            Text::new(&format!("+{extra}")).with_role(TextRole::Caption),
        ));
    }
    trailing = trailing.push(Box::new(button));
    let mut row = ListRow::new(
        format!("directory-listing-{}", listing.key),
        listing.name.clone(),
    )
    .avatar(listing.name.clone(), listing.key.clone())
    .square()
    .subtitle(listing.about.replace('\n', " "))
    .secondary(listing.activity.clone())
    .trailing(Box::new(trailing));
    if let Some(tag) = &listing.tag {
        row = row.badge(tag.clone());
    }
    Box::new(row)
}

/// Enter hands what was typed to the shell; Escape clears the search
/// before it closes the sheet. A row's button takes Space or a press.
struct Searching {
    content: Element,
    dispatch: Dispatcher<WorkspaceDirectory>,
    query: LocalState<String>,
}

impl ui::widget::Decorator for Searching {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        use ui::winit::keyboard::{Key, NamedKey};
        let pressed = match event {
            WindowEvent::KeyboardInput { event: key, .. }
                if key.state == ui::winit::event::ElementState::Pressed =>
            {
                Some(key.logical_key.clone())
            }
            _ => None,
        };
        if pressed == Some(Key::Named(NamedKey::Escape)) && !self.query.get().is_empty() {
            self.query.set(String::new());
            *ctx.handled = true;
            return;
        }
        if pressed == Some(Key::Named(NamedKey::Enter)) {
            let text = self.query.get().trim().to_owned();
            if !text.is_empty() {
                self.dispatch.effect(&DirectoryEffect::Look { text });
                *ctx.handled = true;
                return;
            }
        }
        self.content.handle_event(event, ctx);
    }
}

/// The app's definition, which the shell mounts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(DirectoryApp)
}
