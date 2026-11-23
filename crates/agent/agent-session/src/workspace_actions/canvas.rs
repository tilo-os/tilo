//! The workspace's canvas on its front page: the shell's own canvas (`apps/tl/shell`, its
//! `frames` style) over the layout the workspace shares, as a member's device shows it. The
//! shell runs here as it runs in a session, its views in an instance written as semantic HTML
//! ([`AppPage::hosted`]), and the layout is written into it as the session writes it
//! ([`LayoutOrigin`]). Where a device shows a place's window, the page shows a link to that
//! app's own page; when another member moves a window, the page's next pass moves it here.
//!
//! The workspace's desktop (`/w/<workspace>/desktop`) is the same shell in its `strips` style:
//! the windows its people have open, strip by strip. What the shell's view asks of the session
//! (a window's Close) the page carries out on the shared layout, as the session does on a
//! device, and every device showing the workspace follows.

use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::rc::Rc;

use tilo_desktop_model::reading::{LayoutPlaceReading, LayoutReading, Naming, SHELL};
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
    /// The canvas over `reading`, each place's well holding what `wells` gives it, shown to
    /// `me` (their member id and name) in the shell's style `view`: the page and its stylesheet
    /// and body.
    fn open(
        reading: LayoutReading,
        wells: HashMap<String, String>,
        me: (&str, &str),
        view: &str,
    ) -> Result<(Self, String, String), String> {
        let packages = shell_packages()?;
        let origin = Rc::new(RefCell::new(LayoutOrigin::default()));
        origin.borrow_mut().read(reading);
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
        page.fill_wells(wells);
        let css = page.css();
        let body = page.render();
        Ok((Self { page, origin }, css, body))
    }

    /// What changed on the canvas now that the layout reads `reading`: the elements to replace,
    /// none when nothing moved.
    fn follow(&mut self, reading: LayoutReading, wells: HashMap<String, String>) -> Vec<PagePatch> {
        if !self.origin.borrow_mut().read(reading) {
            return Vec::new();
        }
        self.page.fill_wells(wells);
        patches(self.page.follow())
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
    /// The layout `workspace_id` shares, as the shell reads it, and what each of its places'
    /// wells holds on a page: a link to the page of the app it shows, when the app is served as
    /// one here. A workspace with no layout yet reads as an empty one: its canvas is empty.
    pub(super) fn canvas_layout(
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
        let records = &desktop.records;
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
                    let record = records
                        .iter()
                        .find(|(shown, record)| *shown == tab.id && !record.is_empty())
                        .map(|(_, record)| format!("/{}", encode_part(record)))
                        .unwrap_or_default();
                    let mut well = String::new();
                    let _ = write!(
                        well,
                        "<a class=\"tilo-canvas-open\" href=\"/w/{workspace_id}/{short}{record}\">\
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

    /// The shell's view `view` of `workspace_id`'s layout, shown to `caller`: the page to hold,
    /// its stylesheet and its body.
    pub(super) fn open_canvas(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        view: &str,
    ) -> Result<(CanvasPage, String, String), PageRefusal> {
        let (layout, wells) = self.canvas_layout(workspace_id);
        let me = caller
            .user_public_key()
            .map(hex::encode)
            .unwrap_or_default();
        let (canvas, css, body) =
            Canvas::open(layout, wells, (&me, ""), view).map_err(unavailable)?;
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
    /// others' changes to the layout, read again. What the shell's view asked of the session is
    /// carried out on the layout, and the page answers the layout that left.
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
        let (layout, wells) = self.canvas_layout(workspace_id);
        let canvas = self.canvas(page)?;
        let mut patches = canvas.follow(layout, wells);
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
            self.carry_out_on_layout(workspace_id, &caller, invocation)?;
        }
        let (layout, wells) = self.canvas_layout(workspace_id);
        patches.extend(self.canvas(page)?.follow(layout, wells));
        Ok(patches)
    }

    fn canvas(&mut self, page: u64) -> Result<&mut Canvas, PageRefusal> {
        self.pages
            .canvas_mut(page)
            .map(|held| &mut *held.canvas)
            .ok_or_else(|| unavailable("the page was closed"))
    }

    /// Carry out on `workspace_id`'s layout, as `caller`, what the shell's view asked of the
    /// session: a window's Close closes its place, as the session's `close` does on a device.
    /// The session's other arranging actions are a device's, refused here by name.
    fn carry_out_on_layout(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        invocation: &Invocation,
    ) -> Result<(), PageRefusal> {
        let place = match invocation {
            Invocation::Action {
                app, name, target, ..
            } if app.as_str() == tilolang_tilo::platform_apps::SESSION_APP && name == "close" => {
                target.as_ref()
            }
            _ => {
                return Err(unavailable(format!(
                    "\"{}\" is done on a device of yours, not on a web page",
                    invocation.name()
                )));
            }
        };
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
