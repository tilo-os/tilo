//! The workspace's desktop as a page (`/w/<workspace>/desktop`): the windows its people have
//! open, as every device of theirs shows them, read from the workspace's shared layout.
//!
//! The page holds no layout of its own. Each window is a link to its record's page, so a window
//! opened on a computer continues in the browser on the same record; its Close button closes it
//! on the layout, which every device showing the workspace follows; an app opened here is placed
//! on the layout as an agent's view is ([`tilo_desktop_model::place`]), and every device opens
//! it. The person's other workspaces are links to their desktops. What another device opens or
//! closes reaches the page on its next pass.

use std::fmt::Write as _;

use automerge::AutoCommit;
use tilo_desktop_model::TileContent;
use tilo_desktop_model::ownership::SharedDesktop;
use tilo_server_protocol::page::{OpenedPage, PageAction, PagePatch, PageRefusal, encode_part};

use super::{HeldPage, escape, initial_tile, page_app_name, refused, unavailable};
use crate::workspace_actions::{Caller, DocumentMutation, Needs, Worker, WorkspaceId};

/// The app name a page address carries for the workspace's desktop.
pub(super) const DESKTOP_PAGE: &str = "desktop";

/// The element holding the windows: the one a desktop page patches.
const WINDOWS: u32 = 1;
/// The first window's Close button; the next window's is one more.
const FIRST_CLOSE: u32 = 1 << 10;
/// The first app's Open button; the next app's is one more.
const FIRST_OPEN: u32 = 1 << 20;

/// What a desktop page holds: whose it is, and what its buttons stood for when it was last
/// shown.
pub(in crate::workspace_actions) struct DesktopPage {
    pub(super) workspace_id: WorkspaceId,
    pub(super) caller: Caller,
    /// The window each Close button closes, in the order the page shows them.
    windows: Vec<u64>,
    /// The app each Open button opens.
    apps: Vec<&'static str>,
    /// The windows as the page shows them now: a pass answers only what moved.
    shown: String,
}

/// One window of the layout, as the page lists it.
struct Window {
    tab: u64,
    title: String,
    /// The page that shows its record, when the workspace's pages show its app.
    href: Option<String>,
    /// What the window is beside its title: the app it runs.
    kind: String,
    icon: String,
}

/// The windows of `layout`, strip by strip under each of its workspaces: the HTML of the
/// windows element and the window each Close button names.
fn windows_html(
    layout: Option<&SharedDesktop>,
    window: &dyn Fn(&tilo_desktop_model::Tab, &str) -> Window,
) -> (String, Vec<u64>) {
    let mut out = format!("<div class=\"tilo-desktop-windows\" data-p=\"{WINDOWS}\">");
    let mut closes = Vec::new();
    let several = layout.is_some_and(|layout| layout.tree.workspaces.len() > 1);
    for workspace in layout.iter().flat_map(|layout| &layout.tree.workspaces) {
        if several {
            let _ = write!(
                out,
                "<h2 class=\"tilo-desktop-workspace\">{}</h2>",
                escape(&workspace.name)
            );
        }
        for strip in &workspace.strips {
            let tabs: Vec<_> = strip
                .window_groups
                .iter()
                .flat_map(|group| &group.tabs)
                .collect();
            if tabs.is_empty() {
                continue;
            }
            let _ = write!(
                out,
                "<section class=\"tilo-strip\" aria-label=\"{name}\"><h3>{name}</h3>\
                 <ul class=\"tilo-strip-windows\">",
                name = escape(&strip.name)
            );
            for tab in tabs {
                let record = layout
                    .and_then(|layout| {
                        layout
                            .records
                            .iter()
                            .find(|(shown, _)| *shown == tab.id)
                            .map(|(_, record)| record.as_str())
                    })
                    .unwrap_or_default();
                let shown = window(tab, record);
                let id =
                    FIRST_CLOSE.saturating_add(u32::try_from(closes.len()).unwrap_or(u32::MAX));
                closes.push(shown.tab);
                let inner = format!(
                    "{icon}<span class=\"tilo-window-text\"><span class=\"tilo-window-title\">\
                     {title}</span><span class=\"tilo-window-kind\">{kind}</span></span>",
                    icon = shown.icon,
                    title = escape(&shown.title),
                    kind = escape(&shown.kind),
                );
                out.push_str("<li class=\"tilo-window\">");
                match &shown.href {
                    Some(href) => {
                        let _ = write!(
                            out,
                            "<a class=\"tilo-window-show\" href=\"{}\">{inner}</a>",
                            escape(href)
                        );
                    }
                    None => {
                        let _ = write!(out, "<span class=\"tilo-window-show\">{inner}</span>");
                    }
                }
                let _ = write!(
                    out,
                    "<button class=\"tilo-window-close\" type=\"button\" data-a=\"activate\" \
                     data-p=\"{id}\" aria-label=\"Close {title}\">Close</button></li>",
                    title = escape(&shown.title),
                );
            }
            out.push_str("</ul></section>");
        }
    }
    if closes.is_empty() {
        out.push_str(
            "<p class=\"tilo-desktop-empty\">Nothing is open. What anyone opens in this \
             workspace, on any device, shows here.</p>",
        );
    }
    out.push_str("</div>");
    (out, closes)
}

impl Worker {
    /// The layout of `workspace_id` as the service holds it now, read as its pages read it.
    fn desktop_layout(&mut self, workspace_id: WorkspaceId) -> Option<AutoCommit> {
        self.held_layout(workspace_id, &workspace_id.to_string())
            .ok()
            .flatten()
    }

    /// One window of the layout as the page lists it: its record's page when the workspace's
    /// pages show its app, and the app's icon.
    fn desktop_window(
        &self,
        workspace_id: WorkspaceId,
        tab: &tilo_desktop_model::Tab,
        record: &str,
    ) -> Window {
        let app_id = match &tab.tile {
            TileContent::LocalApp { app_id, .. } | TileContent::Foreign { app_id, .. } => {
                Some(app_id.as_str())
            }
            _ => None,
        };
        let app = app_id
            .and_then(|app_id| self.catalogue.get(app_id))
            .filter(|app| app.keeps_workspace_resource());
        let href = app.map(|app| {
            let mut href = format!("/w/{workspace_id}/{}", page_app_name(app.app_id()));
            if !record.is_empty() {
                let _ = write!(href, "/{}", encode_part(record));
            }
            href
        });
        let title = if tab.title.is_empty() {
            app.map_or_else(|| "Window".to_owned(), |app| app.title().to_owned())
        } else {
            tab.title.clone()
        };
        let kind = match (app, href.is_some()) {
            (Some(app), _) if app.title() != title => app.title().to_owned(),
            (_, true) => String::new(),
            // A window of an app no page shows yet is open on the people's devices.
            (_, false) => "Open on your devices".to_owned(),
        };
        let icon = app.map_or_else(|| initial_tile(&title), super::app_icon);
        Window {
            tab: tab.id,
            title,
            href,
            kind,
            icon,
        }
    }

    /// The windows element of `workspace_id`'s desktop as the layout holds it now, and the
    /// window each Close button names.
    fn desktop_windows(&mut self, workspace_id: WorkspaceId) -> (String, Vec<u64>) {
        let layout = self
            .desktop_layout(workspace_id)
            .and_then(|doc| tilo_desktop_model::shared_resource::read(&doc, None));
        windows_html(layout.as_ref(), &|tab, record| {
            self.desktop_window(workspace_id, tab, record)
        })
    }

    /// Show `caller` the desktop of `workspace_id`: the windows open on it, the apps they open
    /// here, and the other workspaces they are in.
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
        let (windows_html, windows) = self.desktop_windows(workspace_id);
        // The person's workspaces, each named as its layout names it: the one shown first.
        let mut others: Vec<WorkspaceId> = self
            .workspaces
            .keys()
            .copied()
            .filter(|other| *other != workspace_id && self.admits_member(*other, caller))
            .collect();
        others.sort_by_key(ToString::to_string);
        let mut switcher = String::new();
        for (other, current) in std::iter::once((workspace_id, true))
            .chain(others.into_iter().map(|other| (other, false)))
        {
            let name = self
                .desktop_layout(other)
                .and_then(|doc| tilo_desktop_model::shared_resource::read(&doc, None))
                .and_then(|layout| layout.tree.workspaces.first().map(|w| w.name.clone()))
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "Workspace".to_owned());
            let _ = write!(
                switcher,
                "<li><a class=\"tilo-desktop-switch\" href=\"/w/{other}/{DESKTOP_PAGE}\"{current}>\
                 {name}</a></li>",
                current = if current {
                    " aria-current=\"page\""
                } else {
                    ""
                },
                name = escape(&name),
            );
        }
        let mut apps: Vec<_> = self
            .catalogue
            .iter()
            .filter(|app| app.keeps_workspace_resource())
            .collect();
        apps.sort_by_key(|app| app.title().to_owned());
        let mut open = String::new();
        for (at, app) in apps.iter().enumerate() {
            let id = FIRST_OPEN.saturating_add(u32::try_from(at).unwrap_or(u32::MAX));
            let _ = write!(
                open,
                "<li><button class=\"tilo-desktop-app\" type=\"button\" data-a=\"activate\" \
                 data-p=\"{id}\">{icon}<span>{title}</span></button></li>",
                icon = super::app_icon(app),
                title = escape(app.title()),
            );
        }
        let body = format!(
            "<div class=\"tilo-view tilo-desktop\" data-p=\"0\">\
             <nav class=\"tilo-desktop-bar\" aria-label=\"Workspaces\">\
             <ul class=\"tilo-desktop-switcher\">{switcher}</ul>\
             <a class=\"tilo-desktop-link\" href=\"/w/{workspace_id}/{search}\">Search</a></nav>\
             {windows_html}\
             <section class=\"tilo-desktop-open\" aria-label=\"Open an app\"><h2>Open</h2>\
             <ul class=\"tilo-desktop-apps\">{open}</ul></section></div>",
            search = super::SEARCH_APP,
        );
        let page = self.pages.add(HeldPage::Desktop(DesktopPage {
            workspace_id,
            caller: caller.clone(),
            windows,
            apps: apps.iter().map(|app| app.app_id()).collect(),
            shown: windows_html,
        }));
        Ok(OpenedPage {
            page,
            title: "Desktop".to_owned(),
            css: ui::composition::page::theme_css(&ui::Theme::default())
                + ui::composition::page::PAGE_CSS
                + DESKTOP_CSS,
            body,
        })
    }

    /// Act on the desktop page `page`: a Close button closes its window on the layout, an Open
    /// button places its app's window there; a pass answers what another device changed.
    pub(super) fn act_on_desktop(
        &mut self,
        page: u64,
        act: Option<&(u32, Option<String>, PageAction)>,
    ) -> Result<Vec<PagePatch>, PageRefusal> {
        let Some(HeldPage::Desktop(held)) = self.pages.held.get(&page) else {
            return Err(unavailable("the page was closed"));
        };
        let (workspace_id, caller) = (held.workspace_id, held.caller.clone());
        let pressed = match act {
            Some((id, _, PageAction::Activate)) => Some(*id),
            Some(_) => return Ok(Vec::new()),
            None => None,
        };
        // What the button pressed stood for when the person saw it: a window, or an app.
        let window = pressed
            .filter(|id| *id < FIRST_OPEN)
            .and_then(|id| id.checked_sub(FIRST_CLOSE))
            .and_then(|at| held.windows.get(usize::try_from(at).ok()?).copied());
        let app = pressed
            .and_then(|id| id.checked_sub(FIRST_OPEN))
            .and_then(|at| held.apps.get(usize::try_from(at).ok()?).copied());
        let named = workspace_id.to_string();
        let layout = crate::workspace_documents::desktop_route(&named);
        self.refresh(
            workspace_id,
            Needs::Read {
                app_id: &layout.app_id,
                record: &layout.document,
            },
        )
        .map_err(refused)?;
        if let Some(tab) = window {
            self.close_on_layout(workspace_id, &caller, tab)?;
        } else if let Some(app) = app {
            self.place_window(workspace_id, &caller, (&named, app, ""), None)
                .map_err(outcome_refusal)?;
        }
        let (html, windows) = self.desktop_windows(workspace_id);
        let Some(HeldPage::Desktop(held)) = self.pages.held.get_mut(&page) else {
            return Err(unavailable("the page was closed"));
        };
        held.windows = windows;
        if html == held.shown {
            return Ok(Vec::new());
        }
        held.shown.clone_from(&html);
        Ok(vec![PagePatch { id: WINDOWS, html }])
    }

    /// Close the window `tab` on `workspace_id`'s layout as the person `caller` acts for.
    fn close_on_layout(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        tab: u64,
    ) -> Result<(), PageRefusal> {
        let person = self
            .layout_person(workspace_id, caller)
            .ok_or(PageRefusal::NotMember)?;
        let Some(held) = self.desktop_layout(workspace_id) else {
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

/// What a page says of an action on the layout that did not happen.
fn outcome_refusal(outcome: crate::record_actions::Outcome) -> PageRefusal {
    match outcome {
        crate::record_actions::Outcome::Rejected(error) => refused(error),
        crate::record_actions::Outcome::Failed(failure) => unavailable(failure.message),
    }
}

/// The desktop page's own ground: the workspaces over the windows, strip by strip, over the
/// apps a window opens with. After the app pages' stylesheet (`PAGE_CSS`).
const DESKTOP_CSS: &str = "\
.tilo-desktop{height:100%;overflow-y:auto;display:flex;flex-direction:column;gap:24px;\
padding:clamp(20px,5vh,48px) clamp(16px,6vw,64px);background:var(--tilo-ground);box-sizing:border-box;}\
.tilo-desktop>*{width:100%;max-width:880px;}\
.tilo-desktop-bar{display:flex;align-items:center;gap:12px;flex-wrap:wrap;}\
.tilo-desktop-switcher{display:flex;gap:4px;flex-wrap:wrap;margin:0;padding:0;list-style:none;flex:1;min-width:0;}\
.tilo-desktop-switch,.tilo-desktop-link{display:block;padding:6px 12px;border-radius:999px;\
color:var(--tilo-text-dim);text-decoration:none;font-size:14px;font-weight:500;\
white-space:nowrap;overflow:hidden;text-overflow:ellipsis;max-width:240px;}\
.tilo-desktop-switch:hover,.tilo-desktop-link:hover{background:var(--tilo-hover);color:var(--tilo-text);}\
.tilo-desktop-switch[aria-current]{background:var(--tilo-raised);color:var(--tilo-text);}\
.tilo-desktop-switch:focus-visible,.tilo-desktop-link:focus-visible{outline:2px solid var(--tilo-accent);outline-offset:-2px;}\
.tilo-desktop-windows{display:flex;flex-direction:column;gap:18px;}\
.tilo-desktop-workspace{margin:0;font-size:15px;font-weight:600;}\
.tilo-strip{display:flex;flex-direction:column;gap:6px;}\
.tilo-strip h3{margin:0;font-size:13px;font-weight:600;color:var(--tilo-text-dim);}\
.tilo-strip-windows{display:flex;flex-direction:column;gap:2px;margin:0;padding:0;list-style:none;}\
.tilo-window{display:flex;align-items:center;gap:4px;border-radius:10px;}\
.tilo-window:hover{background:var(--tilo-hover);}\
.tilo-window-show{display:flex;align-items:center;gap:12px;flex:1;min-width:0;padding:8px 10px;\
border-radius:10px;color:var(--tilo-text);text-decoration:none;}\
a.tilo-window-show:active{background:var(--tilo-press);}\
.tilo-window-show:focus-visible{outline:2px solid var(--tilo-accent);outline-offset:-2px;}\
.tilo-window-show svg,.tilo-window-show .tilo-home-initial{width:32px;height:32px;flex:none;\
border-radius:8px;font-size:15px;}\
.tilo-window-text{display:flex;flex-direction:column;min-width:0;}\
.tilo-window-title{font-size:14px;font-weight:500;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}\
.tilo-window-kind{font-size:12px;color:var(--tilo-text-tertiary);overflow:hidden;\
text-overflow:ellipsis;white-space:nowrap;}\
.tilo-window-kind:empty{display:none;}\
.tilo-window-close{flex:none;margin-right:6px;padding:6px 10px;border:0;border-radius:8px;\
background:transparent;color:var(--tilo-text-dim);font:inherit;font-size:13px;cursor:pointer;}\
.tilo-window-close:hover{background:var(--tilo-press);color:var(--tilo-text);}\
.tilo-window-close:focus-visible{outline:2px solid var(--tilo-accent);outline-offset:-2px;}\
.tilo-desktop-empty{margin:0;padding:16px;border-radius:12px;background:var(--tilo-raised);\
font-size:14px;color:var(--tilo-text-dim);}\
.tilo-desktop-open{display:flex;flex-direction:column;gap:8px;}\
.tilo-desktop-open h2{margin:0;font-size:13px;font-weight:600;color:var(--tilo-text-dim);}\
.tilo-desktop-apps{display:grid;grid-template-columns:repeat(auto-fill,minmax(104px,1fr));\
gap:4px;margin:0;padding:0;list-style:none;}\
.tilo-desktop-app{display:flex;flex-direction:column;align-items:center;gap:8px;width:100%;\
padding:14px 8px;border:0;border-radius:12px;background:transparent;color:var(--tilo-text);\
font:inherit;font-size:13px;font-weight:500;cursor:pointer;}\
.tilo-desktop-app:hover{background:var(--tilo-hover);}\
.tilo-desktop-app:active{background:var(--tilo-press);}\
.tilo-desktop-app:focus-visible{outline:2px solid var(--tilo-accent);outline-offset:-2px;}\
.tilo-desktop-app svg,.tilo-desktop-app .tilo-home-initial{width:44px;height:44px;border-radius:10px;}";
