//! The workspace's desktop in a browser: the windows its people have open on any device, read
//! from the workspace's shared layout and acted on there.
//!
//! The person's computer has Tasks open: its shell wrote the layout through the daemon, as a
//! session does. Their browser's desktop page is a view of the shell, run by the shell's own
//! process as any app's view is, and Tasks' window leads to the Tasks page: the window continues
//! on the web. Closing it on the page closes it on the layout through the layout's actions, which
//! the computer follows. The page's shell reads what a device's shell reads: the launcher lists
//! the apps the person may open, and opening one from it puts a window on the shared layout that
//! the computer shows. The person's agent's "show this" lands on the same layout, and the page
//! shows it.

#![cfg(target_os = "linux")]

use std::sync::Arc;
use std::time::Duration;

use runtime_collab::wire::shell::{ShellCall, ShellRead, ShellReply};
use serde_json::json;
use tilo_actions::remote::{Caller, Pending};
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT};
use tilo_runtime::device::services::Daemon;
use tilo_server_protocol::page::{OpenedPage, PageAction, PageAddress, PagePatch, PageRefusal};
use tilo_user_identity::DeviceCapabilities;
use tilo_workspace_service::app_serving::PageAct;

use crate::agent_fixture::act;
use crate::host;

const TASKS: &str = "so.tilo.tasks";

/// What `pending` settles to, within the bound a hung test is given.
async fn answered<T: Send + 'static>(pending: Pending<T>) -> T {
    tokio::task::spawn_blocking(move || {
        pending
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .expect("the page answers")
    })
    .await
    .unwrap()
}

/// A browser of the person's: a device their root certified, as a page's reader is.
fn browser(host: &host::Host) -> Caller {
    let (_, cert) = host.certify("Ada's browser", DeviceCapabilities::member());
    Caller::authenticated(cert.user_public_key, cert.device_public_key)
}

/// The workspace's desktop page, opened by `caller`.
async fn open(
    daemon: &Arc<Daemon>,
    workspace: &str,
    caller: &Caller,
) -> Result<OpenedPage, PageRefusal> {
    let pages = daemon
        .pages()
        .expect("the daemon serves its workspace's pages");
    answered(pages.open(
        collab_types::WorkspaceId::parse(workspace).unwrap(),
        caller,
        PageAddress {
            workspace: workspace.to_owned(),
            app: "desktop".into(),
            show: None,
        },
    ))
    .await
}

/// One desktop page a test reads: what it showed whole, and what moved on it since.
struct Page {
    daemon: Arc<Daemon>,
    workspace: collab_types::WorkspaceId,
    opened: OpenedPage,
}

impl Page {
    async fn of(daemon: &Arc<Daemon>, workspace: &str, caller: &Caller) -> Self {
        let opened = open(daemon, workspace, caller)
            .await
            .expect("a member's desktop opens");
        Self {
            daemon: Arc::clone(daemon),
            workspace: collab_types::WorkspaceId::parse(workspace).unwrap(),
            opened,
        }
    }

    /// `act` on the page, or with none, what moved on it since: its patches as one piece.
    async fn act(&self, act: Option<(u32, PageAction)>) -> String {
        let pages = self.daemon.pages().unwrap();
        let patches = answered(pages.act(
            self.workspace,
            self.opened.page,
            act.map(|(id, action)| PageAct {
                id,
                target: None,
                action,
                signed: None,
            }),
        ))
        .await
        .expect("the page is acted on");
        patched(&patches)
    }

    /// The page as it moves, until `shows` is true of what it showed: the page whole first, then
    /// each change. What it showed then.
    async fn until(&self, what: &str, shows: impl Fn(&str) -> bool) -> String {
        if shows(&self.opened.body) {
            return self.opened.body.clone();
        }
        tilo_test_env::within(what, async {
            loop {
                let moved = self.act(None).await;
                if shows(&moved) {
                    return moved;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
    }

    fn close(&self) {
        self.daemon
            .pages()
            .unwrap()
            .close(self.workspace, self.opened.page);
    }
}

/// The window frames the shell's view writes, each from its opening tag to the next one's.
fn windows(html: &str) -> Vec<&str> {
    html.split("data-w=\"WindowFrame\"").skip(1).collect()
}

/// The patches the page answered with, as one piece of HTML.
fn patched(patches: &[PagePatch]) -> String {
    patches
        .iter()
        .map(|patch| patch.html.as_str())
        .collect::<Vec<_>>()
        .join("")
}

/// The number of the innermost element labelled `label` in `html`.
fn labelled(html: &str, label: &str) -> Option<u32> {
    let at = html.rfind(&format!("aria-label=\"{label}\""))?;
    let open = html.get(html.get(..at)?.rfind('<')?..at)?;
    open.split("data-p=\"")
        .nth(1)?
        .split('"')
        .next()?
        .parse()
        .ok()
}

/// The number of the button labelled `label` in the window showing `words`.
fn button_in_window(html: &str, words: &str, label: &str) -> Option<u32> {
    let window = windows(html)
        .into_iter()
        .find(|window| window.contains(words))?;
    let at = window.find(&format!("aria-label=\"{label}\""))?;
    let open = window.get(window.get(..at)?.rfind("<button")?..)?;
    open.split('>')
        .next()?
        .split("data-p=\"")
        .nth(1)?
        .split('"')
        .next()?
        .parse()
        .ok()
}

/// The person's computer: the shell the daemon admits, which reads and writes the layout.
struct Computer {
    shell: runtime_collab::ShellClient,
    workspace: String,
}

impl Computer {
    fn of(daemon: &Daemon, workspace: &str) -> Self {
        let link = daemon.shell_link().unwrap();
        link.set_nonblocking(true).unwrap();
        let (shell, _) =
            runtime_collab::ShellClient::open(tokio::net::UnixStream::from_std(link).unwrap());
        Self {
            shell,
            workspace: workspace.to_owned(),
        }
    }

    /// The layout, as the computer reads it now.
    async fn layout(&self) -> Option<automerge::AutoCommit> {
        let read = self
            .shell
            .call(ShellCall::Read {
                workspace: self.workspace.clone(),
                app: DESKTOP_APP.to_owned(),
                resource: DESKTOP_DOCUMENT.to_owned(),
                since: None,
            })
            .await
            .unwrap();
        match read {
            ShellReply::Read(ShellRead::Whole { bytes, .. }) => {
                Some(automerge::AutoCommit::load(&bytes).unwrap())
            }
            _ => None,
        }
    }

    /// The apps the layout's windows run.
    async fn window_apps(&self) -> Vec<String> {
        self.layout()
            .await
            .and_then(|layout| tilo_desktop_model::shared_resource::read(&layout, None))
            .iter()
            .flat_map(|layout| layout.tree.tabs())
            .filter_map(|tab| match &tab.tile {
                tilo_desktop_model::TileContent::LocalApp { app_id, .. } => Some(app_id.clone()),
                _ => None,
            })
            .collect()
    }

    /// Write the layout with `app` open in one window, as the person's session first writes it.
    async fn open_first(&self, person: &str, (app, title): (&str, &str)) {
        use tilo_desktop_model::{Layout, Strip, StripId, Workspace, WorkspaceId};
        let id = WorkspaceId::parse(&self.workspace).unwrap();
        let mut shown = Workspace::new(id, "Workspace");
        shown.strips = vec![Strip::new(StripId(10), "Main")];
        let tree = Layout {
            active: id,
            workspaces: vec![shown],
            ..Layout::default()
        };
        let mut layout = automerge::AutoCommit::new();
        tilo_desktop_model::desktop_doc::write_shared(
            &mut layout,
            &tree,
            &[],
            &[],
            &Default::default(),
        );
        layout.commit();
        let change = tilo_desktop_model::place::place_view(
            &layout,
            &tilo_desktop_model::place::Opener {
                person,
                opened_by: None,
            },
            &tilo_desktop_model::place::View {
                app_id: app,
                title,
                document: "",
                beside: None,
            },
        )
        .unwrap()
        .expect("the window is added");
        layout.load_incremental(&change).unwrap();
        tilo_test_env::within("the daemon takes the computer's layout", async {
            loop {
                let written = self
                    .shell
                    .call(ShellCall::Write {
                        workspace: self.workspace.clone(),
                        app: DESKTOP_APP.to_owned(),
                        document: DESKTOP_DOCUMENT.to_owned(),
                        readers: None,
                        founding: true,
                        snapshot: true,
                        changes: layout.save(),
                    })
                    .await
                    .unwrap();
                match written {
                    ShellReply::Written(Ok(_)) => return,
                    ShellReply::Written(Err(refusal)) if refusal.contains("Waiting") => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    other => panic!("the daemon takes the computer's layout: {other:?}"),
                }
            }
        })
        .await;
    }

    /// Wait until the computer's layout runs the windows `apps`.
    async fn shows(&self, apps: &[&str]) {
        tilo_test_env::within("the computer follows the layout", async {
            loop {
                if self.window_apps().await == apps {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
    }
}

#[test]
fn a_window_open_on_a_computer_is_on_the_desktop_page_and_closes_from_there() {
    host::run_with(&["shell", "tasks"], |host| async move {
        let (daemon, workspace) = (host.daemon(), host.workspace().to_owned());

        // Her computer has Tasks open.
        let computer = Computer::of(&daemon, &workspace);
        computer.open_first(host.person(), (TASKS, "Tasks")).await;

        // Her browser opens the workspace's desktop: the shell's window frame for Tasks is there,
        // and its well leads to Tasks' page. The page is the shell's view written by its widgets,
        // not markup of the device's own.
        let caller = browser(&host);
        let page = Page::of(&daemon, &workspace, &caller).await;
        let tasks_page = format!("href=\"/w/{workspace}/tasks");
        let shown = page
            .until("the window is on the page, a link to its page", |html| {
                windows(html)
                    .iter()
                    .any(|window| window.contains("data-well=") && window.contains(&tasks_page))
            })
            .await;
        assert!(
            !page.opened.body.contains("tilo-home"),
            "no markup of the device's own: {}",
            page.opened.body
        );

        // She closes it on the page: her computer's layout loses it.
        let close = button_in_window(&shown, "Tasks", "Close")
            .unwrap_or_else(|| panic!("the window's Close button: {shown}"));
        page.act(Some((close, PageAction::Activate))).await;
        computer.shows(&[]).await;

        // The dock's launcher opens on the page, as it does on a device: the panel lists the apps
        // she may open.
        let whole = page.opened.body.clone();
        let launcher = labelled(&whole, "Launcher")
            .unwrap_or_else(|| panic!("the dock's launcher entry: {whole}"));
        let listed = page.act(Some((launcher, PageAction::Activate))).await;
        let listed = if labelled(&listed, "Tasks").is_some() {
            listed
        } else {
            page.until("the launcher lists Tasks", |html| {
                labelled(html, "Tasks").is_some()
            })
            .await
        };
        let tasks_row = labelled(&listed, "Tasks").unwrap();

        // She opens Tasks from the launcher: it stands on the shared layout as a window, her
        // computer shows it, and so does the page.
        page.act(Some((tasks_row, PageAction::Activate))).await;
        computer.shows(&[TASKS]).await;
        page.until("the page shows the window it opened", |html| {
            html.contains(&tasks_page)
        })
        .await;
        page.close();
    });
}

/// The person's agent shows Tasks: the view stands on their layout through the layout's own
/// `place`, and the desktop page their browser opens shows its window, a link to Tasks' page.
#[test]
fn a_view_an_agent_shows_is_on_the_persons_desktop_page() {
    host::run_with(&["shell", "tasks"], |host| async move {
        let (daemon, workspace) = (host.daemon(), host.workspace().to_owned());
        let agent = host.agent();
        let (opened, failed) = act(&agent, "tasks#open", json!({})).await;
        assert!(!failed, "the agent opens Tasks: {opened}");

        let computer = Computer::of(&daemon, &workspace);
        computer.shows(&[TASKS]).await;

        let page = Page::of(&daemon, &workspace, &browser(&host)).await;
        let tasks_page = format!("href=\"/w/{workspace}/tasks");
        page.until(
            "the window the agent showed is on the page, a link to Tasks' page",
            |html| {
                windows(html)
                    .iter()
                    .any(|window| window.contains(&tasks_page))
            },
        )
        .await;
        page.close();
    });
}

/// The layout's own actions, as any caller reaches them: a person places a window, reads the
/// layout with it on, and closes it, and the computer follows each step.
#[test]
fn the_layouts_actions_place_read_and_close_a_window() {
    host::run_with(&["tasks"], |host| async move {
        let (daemon, workspace) = (host.daemon(), host.workspace().to_owned());
        let key = host.key("Ada's script");
        let computer = Computer::of(&daemon, &workspace);
        let call = |path: &'static str, args: serde_json::Value| {
            let (key, workspace) = (&key, workspace.clone());
            async move {
                let result = key
                    .tool(
                        "tilo_do",
                        &json!({"workspace": workspace, "path": path, "args": args}),
                    )
                    .await;
                let text: String = result["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|part| part["text"].as_str())
                    .collect();
                assert!(
                    !result["isError"].as_bool().unwrap_or(false),
                    "{path}: {text}"
                );
                text
            }
        };

        call("workspace:layout#place", json!({ "app": TASKS })).await;
        computer.shows(&[TASKS]).await;

        let read = call("workspace:layout#read", json!({})).await;
        assert!(read.contains(TASKS), "the layout reads the window: {read}");
        let window = computer
            .layout()
            .await
            .and_then(|layout| tilo_desktop_model::shared_resource::read(&layout, None))
            .and_then(|layout| layout.tree.tabs().next().map(|tab| tab.id.to_string()))
            .expect("the window is on the layout");

        call("workspace:layout#close", json!({ "window": window })).await;
        computer.shows(&[]).await;
    });
}

/// The person picks the shell's canvas (its `frames` style) as their own way of showing the
/// layout, through the shell's own action: their desktop page draws the canvas, the window on
/// it still a link to its app's page.
#[test]
fn the_desktop_page_draws_the_canvas_a_person_picked() {
    host::run_with(&["shell", "tasks"], |host| async move {
        let (daemon, workspace) = (host.daemon(), host.workspace().to_owned());
        Computer::of(&daemon, &workspace)
            .open_first(host.person(), (TASKS, "Tasks"))
            .await;
        let key = host.key("Ada's script");
        let picked = key
            .tool(
                "tilo_do",
                &json!({
                    "workspace": workspace,
                    "path": "shell#show-style",
                    "args": {"style": "so.tilo.shell:frames"},
                }),
            )
            .await;
        assert!(
            !picked["isError"].as_bool().unwrap_or(false),
            "she picks the canvas: {picked}"
        );

        let page = Page::of(&daemon, &workspace, &browser(&host)).await;
        let tasks_page = format!("href=\"/w/{workspace}/tasks");
        page.until("the canvas, Tasks' window on it a link to its page", |html| {
            labelled(html, "Canvas").is_some()
                && windows(html)
                    .iter()
                    .any(|window| window.contains(&tasks_page))
        })
        .await;
        page.close();
    });
}
