//! The workspace's canvas on its front page: the shell's own canvas (`apps/tl/shell`, its
//! `frames` style) over the layout the workspace shares, as a member's device shows it. The
//! shell runs here as it runs in a session, its views in an instance written as semantic HTML
//! ([`AppPage::hosted`]), and the layout is written into it as the session writes it
//! ([`LayoutOrigin`]). Where a device shows a place's window, the page shows a link to that
//! app's own page; when another member moves a window, the page's next pass moves it here.
//!
//! The workspace's desktop (`/w/<workspace>/desktop`) is the same shell in its `strips` style:
//! the windows its people have open, strip by strip. A page's shell reads what a device's shell
//! reads — the workspace's installed apps (`session:apps`), the windows on its layout
//! (`session:windows`) and the surface over it (`desktop:overlays`) — so its bar, its dock and
//! its launcher are the same views over the same names. What the shell's view asks of the
//! session it does not run (a window's Close, an app's Open, the launcher's surface) the page
//! carries out where that state is: the shared layout, and the page's own overlays.

use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::rc::Rc;

use serde_json::{Value, json};
use tilo_desktop_model::reading::{
    LayoutPlaceReading, LayoutReading, LayoutWorkspaceReading, Naming, SHELL,
};
use tilo_server_protocol::page::{OpenedPage, PageAction, PagePatch, PageRefusal, encode_part};
use tilolang_tilo::runtime::delegate::Invocation;
use tilolang_tilo::shell::LayoutOrigin;
use tilolang_web::window::{AppPage, Hosting};

use super::pages::{HeldPage, refused, unavailable};
use super::{Caller, DocumentMutation, Needs, Worker, WorkspaceDocuments, WorkspaceId};

/// The app name a page address carries for the workspace's desktop.
pub(super) const DESKTOP_PAGE: &str = "desktop";

/// The shell's style the desktop page shows: the windows strip by strip.
const DESKTOP_STYLE: &str = "strips";

/// The session's app, whose arranging actions a page carries out on the workspace's layout.
const SESSION_APP: &str = tilolang_tilo::platform_apps::SESSION_APP;

/// The device's app, whose `show-surface` a page carries out on its own overlays.
const DEVICE_APP: &str = tilolang_tilo::platform_apps::DEVICE_APP;

/// The overlays the page shows over its windows, as the session publishes them (`desktop:overlays`).
const OVERLAYS: &str = "desktop:overlays";

/// What the session publishes a place as (`shell:place:<key>`), its key being the window's tab.
const PLACE: &str = "shell:place:";

/// The size the canvas is laid out at before the browser lays it out at its own: a laptop's
/// window, where each place stands at its point on the canvas.
const VIEWPORT: (u32, u32) = (1280, 800);

thread_local! {
    /// The shell's package, compiled once on each of the service's threads that shows a
    /// canvas: every page of its workspaces runs the same.
    static SHELL_PACKAGES: OnceCell<Result<Rc<tilolang_web::Packages>, String>> =
        const { OnceCell::new() };
}

fn shell_packages() -> Result<Rc<tilolang_web::Packages>, String> {
    SHELL_PACKAGES.with(|held| {
        held.get_or_init(|| {
            let shell = tilolang_tilo::shell::compile()?;
            let platform = tilolang_tilo::platform_apps::platform_packages()?;
            Ok(Rc::new(tilolang_web::Packages::new(
                Rc::new(shell),
                platform,
            )))
        })
        .clone()
    })
}

/// A workspace's canvas a page shows: the shell's page and the layout it is written from.
pub(super) struct Canvas {
    page: AppPage,
    origin: Rc<RefCell<LayoutOrigin>>,
    /// The surface the page's shell shows over its windows (`desktop:overlays`): the page's own,
    /// as a device's shell keeps its own. `none` at rest.
    here: String,
}

/// What a page's shell reads of the host's own state, by the names a device's shell reads: the
/// workspace's installed apps, the windows on its layout, and the surface over them.
#[derive(Default)]
pub(super) struct Readout {
    /// The layout as the shell's views read it, and what each place's well holds.
    reading: LayoutReading,
    wells: HashMap<String, String>,
    /// `session:apps`, as a device's launcher lists the apps it can open.
    apps: Value,
    /// `session:windows`, as a device's dock, launcher and switcher walk the layout's windows.
    windows: Value,
}

impl Readout {
    /// Hand `canvas` everything it reads, so its next render is the shell a device shows.
    fn show(&self, canvas: &mut Canvas) -> Result<(), String> {
        // The page's shell shows one surface over its windows, and nothing else a device's
        // stack holds: the person's questions, the About sheet and the Super keys are a
        // device's own.
        canvas.page.fill_reading(
            OVERLAYS,
            &json!({
                "here": if canvas.here.is_empty() { "none" } else { canvas.here.as_str() },
                "switcher_revealed": false,
                "switcher_scope": "workspace",
                "spotlight": "",
                "ask": "",
                "ask_detail": "",
                "ask_answer": "",
                "ask_open": false,
                "about": false,
                "emoji": false,
                "peek": Value::Null,
                "about_sheet": Value::Null,
            }),
        )?;
        canvas.page.fill_reading("session:apps", &self.apps)?;
        canvas.page.fill_reading("session:windows", &self.windows)?;
        Ok(())
    }
}

/// A page showing the shell over the workspace's layout: the front page's canvas or the desktop.
pub(in crate::workspace_actions) struct CanvasPage {
    workspace_id: WorkspaceId,
    /// Who opened it: what the shell's view asks of the session is done as them.
    pub(super) caller: Caller,
    canvas: Box<Canvas>,
    /// The front page: a pass reads the workspace again first, so one no longer readable here is
    /// refused.
    home: bool,
}

impl Canvas {
    /// The canvas over `readout`, each place's well holding what it gives it, shown to `me` (their
    /// member id and name) in the shell's style `view`: the page and its stylesheet and body.
    fn open(
        readout: Readout,
        me: (&str, &str),
        view: &str,
    ) -> Result<(Self, String, String), String> {
        let packages = shell_packages()?;
        let origin = Rc::new(RefCell::new(LayoutOrigin::default()));
        origin.borrow_mut().read(readout.reading.clone());
        let mut page = AppPage::hosted(
            tilolang_tilo::shell::SHELL_APP,
            &packages,
            Hosting {
                root: "layout",
                view,
                origin: Rc::clone(&origin) as tilolang_tilo::origin::SharedOrigin,
                me,
                viewport: VIEWPORT,
                dark: true,
                link_home: None,
            },
        )?;
        page.fill_wells(readout.wells.clone());
        let mut canvas = Self {
            page,
            origin,
            here: String::new(),
        };
        // The shell reads the host's own state first: its dock, its bar and its launcher stand
        // as a device's do before the page is written.
        readout.show(&mut canvas)?;
        let _ = canvas.page.follow();
        let css = canvas.page.css();
        let body = canvas.page.render();
        Ok((canvas, css, body))
    }

    /// What changed on the canvas now that the layout reads `readout` and the shell reads what it
    /// holds: the elements to replace, none when nothing moved.
    fn follow(&mut self, readout: Readout) -> Result<Vec<PagePatch>, String> {
        self.origin.borrow_mut().read(readout.reading.clone());
        self.page.fill_wells(readout.wells.clone());
        readout.show(self)?;
        Ok(patches(self.page.follow()))
    }

    /// What the person did on the canvas (a press, a pan), acted on the shell's view as it is
    /// in a window: what it changed.
    fn act(
        &mut self,
        id: u32,
        target: Option<&str>,
        action: &PageAction,
    ) -> Result<Vec<PagePatch>, String> {
        let action = crate::worker::proto::semantic(action);
        self.page.act(id, target, &action).map(patches)
    }
}

fn patches(patches: Vec<ui::composition::page::Patch>) -> Vec<PagePatch> {
    patches
        .into_iter()
        .map(|patch| PagePatch {
            id: patch.id,
            html: patch.html,
        })
        .collect()
}

impl Worker {
    /// What a page's shell of `workspace_id` reads, as a device's shell reads it off its own
    /// session: the layout and what each place's well holds (a link to the page of the app it
    /// shows, when the app is served as one here), the workspace's installed apps
    /// (`session:apps`), the windows on its layout (`session:windows`), and the person's
    /// workspaces, each with the page it opens as. A workspace with no layout yet reads as an
    /// empty one: its canvas is empty.
    pub(super) fn canvas_readout(&mut self, workspace_id: WorkspaceId, caller: &Caller) -> Readout {
        let (mut reading, wells) = self.layout_readout(workspace_id);
        // The person's workspaces, each by the page it opens as: the one shown first, then the
        // others this device holds and this person is in. A workspace's name is the one its own
        // layout gives it, as the page's bar and switcher name it.
        for workspace in &mut reading.workspaces {
            workspace.home = format!("/w/{workspace_id}/{DESKTOP_PAGE}");
        }
        let mut others: Vec<WorkspaceId> = self
            .workspaces
            .keys()
            .copied()
            .filter(|other| *other != workspace_id && self.admits_member(*other, caller))
            .collect();
        others.sort_by_key(ToString::to_string);
        for other in others {
            let name = self
                .layout_name(other)
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "Workspace".to_owned());
            reading.workspaces.push(LayoutWorkspaceReading {
                key: other.to_string(),
                name,
                current: false,
                home: format!("/w/{other}/{DESKTOP_PAGE}"),
            });
        }
        let apps = self.apps_reading();
        let windows = windows_reading(&reading, &self.catalogue);
        Readout {
            reading,
            wells,
            apps,
            windows,
        }
    }

    /// The layout `workspace_id` shares as the shell reads it, and what each place's well holds:
    /// a link to the page of the app it shows, when the app is served as one here. A workspace
    /// with no layout yet reads as an empty one.
    fn layout_readout(
        &mut self,
        workspace_id: WorkspaceId,
    ) -> (LayoutReading, HashMap<String, String>) {
        let named = workspace_id.to_string();
        let layout = crate::workspace_documents::desktop_route(&named);
        // Read, never waited for: a layout not here yet is an empty canvas until it is.
        let _ = self.refresh(
            workspace_id,
            Needs::Read {
                app_id: &layout.app_id,
                record: &layout.document,
            },
        );
        let desktop = self
            .workspaces
            .get_mut(&workspace_id)
            .and_then(|workspace| {
                let reads = workspace.reads.clone();
                workspace
                    .documents
                    .get_or_insert_with(|| WorkspaceDocuments::new(&named, reads))
                    .desktop()
            });
        let Some(desktop) =
            desktop.and_then(|doc| tilo_desktop_model::shared_resource::read(&doc, None))
        else {
            return (LayoutReading::default(), HashMap::new());
        };
        let record = &desktop.records;
        let mut wells = HashMap::new();
        let reading = tilo_desktop_model::reading::of(
            &desktop.tree,
            String::new(),
            &Naming {
                workspace: &|name: &str| name.trim().to_owned(),
                row: &|name: &str, at: usize| {
                    let name = name.trim();
                    if name.is_empty() {
                        format!("Strip {}", at.saturating_add(1))
                    } else {
                        name.to_owned()
                    }
                },
            },
            |tab, strip| {
                let app_id = tilo_desktop_model::apps::canonical_app_id(&tab.tile)
                    .map(String::from)
                    .unwrap_or_default();
                let app = self.catalogue.get(&app_id);
                let app_title = app.map_or_else(String::new, |app| app.title().to_owned());
                let title = tab.title.trim().to_owned();
                let name = match (title.is_empty(), app_title.is_empty()) {
                    (false, false) if title != app_title => format!("{title} — {app_title}"),
                    (false, _) => title.clone(),
                    (true, _) => app_title.clone(),
                };
                // The window's own page, where the app is served as one, on the record it
                // shows: the well is the way into it, and the window continues there.
                if let Some(app) = app.filter(|app| app.keeps_workspace_resource()) {
                    let short = app
                        .app_id()
                        .rsplit_once('.')
                        .map_or_else(|| app.app_id(), |(_, last)| last);
                    let shown = record
                        .iter()
                        .find(|(shown, record)| *shown == tab.id && !record.is_empty())
                        .map(|(_, record)| format!("/{}", encode_part(record)))
                        .unwrap_or_default();
                    let mut well = String::new();
                    let _ = write!(
                        well,
                        "<a class=\"tilo-canvas-open\" href=\"/w/{workspace_id}/{short}{shown}\">\
                         Open {}</a>",
                        super::pages::escape(&app_title)
                    );
                    wells.insert(tab.id.to_string(), well);
                }
                LayoutPlaceReading {
                    key: tab.id.to_string(),
                    strip: strip.to_string(),
                    shows: app_id,
                    shown_as: tab.view.clone(),
                    // What the window shows is its app's to draw, on the app's own page.
                    resource: String::new(),
                    title,
                    name,
                    pinned: tab.pinned,
                    shell: SHELL.to_owned(),
                }
            },
        );
        (reading, wells)
    }

    /// The name the layout `workspace_id` holds gives it: what its bar and its switcher call it.
    fn layout_name(&mut self, workspace_id: WorkspaceId) -> Option<String> {
        let held = self.held_layout(workspace_id).ok().flatten()?;
        tilo_desktop_model::shared_resource::read(&held, None)?
            .tree
            .workspaces
            .first()
            .map(|workspace| workspace.name.trim().to_owned())
    }

    /// The apps this workspace's shell lists, as `session:apps` publishes them to a device's
    /// launcher: the installed apps whose records the workspace's pages serve, by the catalogue
    /// the server already holds. The catalogue names no category for them, so the reading carries
    /// none.
    fn apps_reading(&self) -> Value {
        let mut apps: Vec<&ui::application::ApplicationDefinition> = self
            .catalogue
            .iter()
            .filter(|app| app.keeps_workspace_resource())
            .collect();
        apps.sort_by_key(|app| app.title().to_owned());
        let installed: Vec<Value> = apps
            .into_iter()
            .map(|app| {
                json!({
                    "key": app.app_id(),
                    "name": app.title(),
                    "icon": ui::icons::app_glyph(app.app_id(), None).name(),
                    "category": "",
                })
            })
            .collect();
        json!({ "installed": installed })
    }

    /// The shell's view `view` of `workspace_id`'s layout, shown to `caller`: the page to hold,
    /// its stylesheet and its body.
    pub(super) fn open_canvas(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        view: &str,
    ) -> Result<(CanvasPage, String, String), PageRefusal> {
        let readout = self.canvas_readout(workspace_id, caller);
        let me = caller
            .user_public_key()
            .map(hex::encode)
            .unwrap_or_default();
        let (canvas, css, body) = Canvas::open(readout, (&me, ""), view).map_err(unavailable)?;
        let held = CanvasPage {
            workspace_id,
            caller: caller.clone(),
            canvas: Box::new(canvas),
            home: view != DESKTOP_STYLE,
        };
        Ok((held, css, body))
    }

    /// Show `caller` the desktop of `workspace_id`: the windows its people have open on any
    /// device, strip by strip, as the shell shows them.
    pub(super) fn open_desktop(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
    ) -> Result<OpenedPage, PageRefusal> {
        let layout = crate::workspace_documents::desktop_route(&workspace_id.to_string());
        let state = self
            .refresh(
                workspace_id,
                Needs::Read {
                    app_id: &layout.app_id,
                    record: &layout.document,
                },
            )
            .map_err(refused)?;
        state.workspace.as_ref().ok_or(PageRefusal::Elsewhere)?;
        let (held, css, body) = self.open_canvas(workspace_id, caller, DESKTOP_STYLE)?;
        let page = self.pages.add(HeldPage::Desktop(held));
        Ok(OpenedPage {
            page,
            title: "Desktop".to_owned(),
            css,
            body,
        })
    }

    /// What the person did on the canvas `page`, or with nothing done, what moved on it since:
    /// others' changes to the layout, read again. What the shell's view asked of its host is
    /// carried out where that state is — the shared layout and the page's own overlays — and the
    /// page answers what that left.
    pub(super) fn act_on_canvas(
        &mut self,
        page: u64,
        act: Option<(u32, Option<String>, PageAction)>,
    ) -> Result<Vec<PagePatch>, PageRefusal> {
        let held = self
            .pages
            .canvas_mut(page)
            .ok_or_else(|| unavailable("the page was closed"))?;
        let (workspace_id, caller, home) = (held.workspace_id, held.caller.clone(), held.home);
        if home {
            self.refresh(workspace_id, Needs::Workspace)
                .map_err(refused)?;
        }
        let readout = self.canvas_readout(workspace_id, &caller);
        let canvas = self.canvas(page)?;
        let mut patches = canvas.follow(readout).map_err(unavailable)?;
        let Some((id, target, action)) = act else {
            return Ok(patches);
        };
        patches.extend(
            canvas
                .act(id, target.as_deref(), &action)
                .map_err(unavailable)?,
        );
        let asked = canvas.page.take_asked();
        if asked.is_empty() {
            return Ok(patches);
        }
        for invocation in &asked {
            self.carry_out_on_page(page, workspace_id, &caller, invocation)?;
        }
        let readout = self.canvas_readout(workspace_id, &caller);
        patches.extend(self.canvas(page)?.follow(readout).map_err(unavailable)?);
        Ok(patches)
    }

    fn canvas(&mut self, page: u64) -> Result<&mut Canvas, PageRefusal> {
        self.pages
            .canvas_mut(page)
            .map(|held| &mut *held.canvas)
            .ok_or_else(|| unavailable("the page was closed"))
    }

    /// Carry out what the view of the canvas `page` asked of its host, as `caller`: the state it
    /// names is a device's, and the page's own overlays are the page's.
    fn carry_out_on_page(
        &mut self,
        page: u64,
        workspace_id: WorkspaceId,
        caller: &Caller,
        invocation: &Invocation,
    ) -> Result<(), PageRefusal> {
        if let Invocation::Action {
            app, name, target, args, ..
        } = invocation
        {
            if app.as_str() == SESSION_APP && name == "close" {
                return self.close_on_layout(workspace_id, caller, target.as_ref());
            }
            if app.as_str() == SESSION_APP && name == "open-app" {
                let opened = argument(args, "app")
                    .ok_or_else(|| unavailable("the app to open is not one the launcher names"))?;
                // What the launcher opened stands on the layout, and the launcher closes over
                // it, as it does on a device: the window is what the person asked for.
                if let Some(held) = self.pages.canvas_mut(page) {
                    held.canvas.here = String::new();
                }
                return self
                    .place_window(workspace_id, caller, (opened.as_str(), ""), None)
                    .map_err(|outcome| match outcome {
                        crate::record_actions::Outcome::Rejected(error) => refused(error),
                        crate::record_actions::Outcome::Failed(failure) => {
                            unavailable(failure.message)
                        }
                    });
            }
            // The surface over the page's windows is the page's own: the person opens and closes
            // the shell's panels there as on a device, and nobody else's screen moves.
            if app.as_str() == DEVICE_APP
                && name == "show-surface"
                && target
                    .as_ref()
                    .is_some_and(|target| target.id_string() == OVERLAYS)
            {
                let surface = argument(args, "surface").unwrap_or_default();
                let here = self
                    .pages
                    .canvas_mut(page)
                    .ok_or_else(|| unavailable("the page was closed"))?;
                here.canvas.here = if here.canvas.here == surface {
                    String::new()
                } else {
                    surface
                };
                return Ok(());
            }
        }
        Err(unavailable(format!(
            "\"{}\" is done on a device of yours, not on a web page",
            invocation.name()
        )))
    }

    /// Close the place `place` names on `workspace_id`'s layout, as the session's `close` does on
    /// a device.
    fn close_on_layout(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        place: Option<&tilolang_tilo::runtime::ResourceRef>,
    ) -> Result<(), PageRefusal> {
        let tab = place
            .and_then(|place| place.id_string().strip_prefix(PLACE)?.parse().ok())
            .ok_or_else(|| unavailable("the window to close is not one of the layout's"))?;
        let person = self
            .layout_person(workspace_id, caller)
            .ok_or(PageRefusal::NotMember)?;
        let Some(held) = self.held_layout(workspace_id).ok().flatten() else {
            return Ok(());
        };
        let Some(change) = tilo_desktop_model::place::close_window(&held, person.as_str(), tab)
            .map_err(unavailable)?
        else {
            return Ok(());
        };
        let layout = crate::workspace_documents::desktop_route(&workspace_id.to_string());
        self.commit(
            workspace_id,
            &layout,
            &DocumentMutation::Incremental(change),
        )
        .map_err(unavailable)
    }
}

/// The text `args` gives the parameter `name`, as the shell's view sent it.
fn argument(args: &[(String, tilolang_tilo::runtime::Value)], name: &str) -> Option<String> {
    args.iter()
        .find(|(parameter, _)| parameter == name)
        .and_then(|(_, value)| match value {
            tilolang_tilo::runtime::Value::Text(text) => Some(text.clone()),
            _ => None,
        })
}

/// The windows on `reading`'s layout as `session:windows` publishes them to a device's dock,
/// launcher and switcher: every place of every strip, with where it stands and the app it runs,
/// named by `catalogue` as a device names it. A page's shell reads them by the same name, so its
/// dock and launcher offer what a device's do.
fn windows_reading(reading: &LayoutReading, catalogue: &crate::apps::AppCatalogue) -> Value {
    let mut all = Vec::with_capacity(reading.places.len());
    for (at, place) in reading.places.iter().enumerate() {
        let strip = reading
            .strips
            .iter()
            .find(|strip| strip.key == place.strip);
        let workspace = strip.and_then(|strip| {
            reading
                .workspaces
                .iter()
                .find(|workspace| workspace.key == strip.workspace)
        });
        let app = catalogue.get(&place.shows);
        let title = if place.title.trim().is_empty() {
            place.name.clone()
        } else {
            place.title.clone()
        };
        // The share of its row the window keeps, as a fraction of the width the strip names.
        let width = strip.map_or(1000, |strip| strip.width);
        let share = f64::from(i32::try_from(width).unwrap_or_default()) / 1000.0;
        let at = i64::try_from(at.saturating_add(1)).unwrap_or(i64::MAX);
        all.push(json!({
            "key": place.key,
            "title": title,
            "origin_app": app.map_or("", |app| app.title()),
            "app_key": place.shows,
            "icon": ui::icons::app_glyph(&place.shows, None).name(),
            "strip": strip.map_or("", |strip| strip.name.as_str()),
            "strip_key": place.strip,
            "workspace": workspace.map_or("", |workspace| workspace.name.as_str()),
            "workspace_key": strip.map_or("", |strip| strip.workspace.as_str()),
            "group_key": place.strip,
            "tab_index": 0,
            "share": share,
            "local": true,
            "device": "",
            "device_name": "",
            "place": at,
            "recency": at,
            "focused": strip.is_some_and(|strip| strip.active == place.key),
            "preview": "none",
            "not_responding": false,
        }));
    }
    json!({ "all": all })
}
