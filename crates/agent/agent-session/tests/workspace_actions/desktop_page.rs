//! The workspace's desktop in a browser: the windows its people have open on any device, read
//! from the workspace's shared layout and acted on there.
//!
//! A person's computer has Tasks open. Their browser's desktop page is the shell's own view of
//! the layout, its windows strip by strip, and Tasks' window leads to the Tasks page: the window
//! continues on the web. Closing it on the page closes it on the layout, which the computer
//! follows.
use super::*;
use tilo_server_protocol::page::{OpenedPage, PageAction, PageAddress, PagePatch};

const TASKS: &str = "so.tilo.tasks";

/// What the page answered, within the bound a hung test is given.
fn answered<T>(pending: tilo_actions::remote::Pending<T>) -> T {
    pending
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("the page answers")
}

/// The window frames the shell's view writes, each from its opening tag to the next one's.
fn windows(html: &str) -> Vec<&str> {
    html.split("data-w=\"WindowFrame\"").skip(1).collect()
}

/// The number of the button labelled `label` in the window holding `words`.
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

/// The apps the layout's windows run, as `layout` holds them.
fn window_apps(layout: Option<&tilo_desktop_model::ownership::SharedDesktop>) -> Vec<String> {
    layout
        .iter()
        .flat_map(|layout| layout.tree.tabs())
        .filter_map(|tab| match &tab.tile {
            tilo_desktop_model::TileContent::LocalApp { app_id, .. } => Some(app_id.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_window_open_on_a_computer_is_on_the_desktop_page_and_closes_from_there() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let person = hex::encode(root.verifying_key().to_bytes());
    let host = enrol(&root, "home-server", DeviceCapabilities::member());
    let computer = enrol(&root, "Ada's computer", DeviceCapabilities::member());
    let browser = enrol(&root, "Ada's browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let actions = hosted.roster.actions.clone();

    // Her computer has Tasks open.
    let mut desk = Browser::open(&hosted, &computer, &host).await;
    desk.put_layout();
    crate::workspace_actions::saved(&desk.peer.barrier().unwrap()).unwrap();
    desk.open_on_layout(TASKS, "Tasks", &person);
    crate::workspace_actions::saved(&desk.peer.barrier().unwrap()).unwrap();
    let (workspace, workspace_id) = (WORKSPACE.to_owned(), desk.workspace);

    // Her browser opens the workspace's desktop: the window is there, a link to its page.
    let caller = tilo_actions::remote::Caller::authenticated(
        browser.cert.user_public_key,
        browser.cert.device_public_key,
    );
    let address = PageAddress {
        workspace: workspace.clone(),
        app: "desktop".into(),
        show: None,
    };
    let tasks_page = format!("href=\"/w/{workspace}/tasks");
    let deadline = tilo_test_env::hang_deadline();
    let opened: OpenedPage = loop {
        let opened = answered(actions.open_page(workspace_id, &caller, address.clone()))
            .expect("a member's desktop opens");
        if opened.body.contains(&tasks_page) {
            break opened;
        }
        actions.close_page(workspace_id, opened.page);
        assert!(
            Instant::now() < deadline,
            "the computer's window never showed on the page: {}",
            opened.body
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(opened.title, "Desktop");

    // The page is the shell's view written by its widgets, not markup of its own: the window
    // is the shell's window frame, its link stands in the frame's well, and its Close is the
    // frame's own button.
    let shown = windows(&opened.body);
    assert_eq!(shown.len(), 1, "one window, one frame: {}", opened.body);
    assert!(
        shown[0].contains("data-well=") && shown[0].contains(&tasks_page),
        "the window's well leads to its page: {}",
        opened.body
    );
    assert!(
        !opened.body.contains("tilo-desktop"),
        "no markup of the page's own: {}",
        opened.body
    );

    // A pass with nothing moved answers nothing.
    let unchanged: Vec<PagePatch> =
        answered(actions.act_on_page(workspace_id, opened.page, None)).expect("a pass is answered");
    assert!(unchanged.is_empty(), "{unchanged:?}");

    // She closes it on the page: her computer's layout loses it, and the page shows it gone.
    let close = button_in_window(&opened.body, "Tasks", "Close")
        .unwrap_or_else(|| panic!("the window's Close button: {}", opened.body));
    let patches = answered(actions.act_on_page(
        workspace_id,
        opened.page,
        Some((close, None, PageAction::Activate)),
    ))
    .expect("the close is acted on");
    assert!(
        !patches.is_empty()
            && patches
                .iter()
                .all(|patch| !patch.html.contains(&tasks_page)),
        "the page shows the window gone: {patches:?}"
    );
    desk.until(|desk| {
        let apps = window_apps(desk.layout().as_ref());
        if apps.is_empty() {
            Ok(())
        } else {
            Err(format!("the computer still shows {apps:?}"))
        }
    })
    .await;
    let again = answered(actions.open_page(workspace_id, &caller, address))
        .expect("the desktop opens again");
    assert!(
        windows(&again.body)
            .iter()
            .all(|window| !window.contains(&tasks_page)),
        "the closed window stays closed: {}",
        again.body
    );

    actions.close_page(workspace_id, again.page);
    actions.close_page(workspace_id, opened.page);
    desk.peer.close();
    hosted.agents.shutdown();
    hosted.server.shutdown().await;
}
