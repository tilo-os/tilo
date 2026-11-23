//! A workspace's front page on the web is its canvas: the shell's own canvas over the layout the
//! workspace shares, each window where its members put it, beside the people and the apps. A
//! window opens on its app's own page from there, and when a member moves a window on their
//! device, the open page's next pass moves it too.

use super::*;
use tilo_actions::remote::Caller;
use tilo_desktop_model::{
    CanvasPlace, Layout, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
};
use tilo_server_protocol::page::{PageAddress, PageRefusal};

/// The workspace's layout as a member arranged it: a note and a task list, each a window on the
/// canvas, the task list at `tasks_at`.
fn arranged(workspace: &str, tasks_at: (i32, i32)) -> Layout {
    let id = tilo_desktop_model::WorkspaceId::parse(workspace).unwrap();
    let window = |group: u64, app: &str, title: &str| WindowGroup {
        id: group,
        tabs: vec![Tab {
            id: group + 100,
            title: title.to_owned(),
            tile: TileContent::LocalApp {
                app_id: app.to_owned(),
                label: title.to_owned(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        }],
        active_tab: group + 100,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    };
    let mut strip = Strip::new(StripId(10), "Main");
    strip.window_groups = vec![
        window(1, "so.tilo.notes", "Groceries"),
        window(2, "so.tilo.tasks", "Launch"),
    ];
    let mut workspace = Workspace::new(id, "Team");
    workspace.strips = vec![strip];
    workspace
        .layout
        .places
        .insert(1, CanvasPlace::new(40, 40, 480, 360));
    workspace
        .layout
        .places
        .insert(2, CanvasPlace::new(tasks_at.0, tasks_at.1, 480, 360));
    Layout {
        active: id,
        workspaces: vec![workspace],
        ..Layout::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_front_page_shows_the_canvas_and_follows_a_window_a_member_moves() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = crate::harness::hosted::start(
        &root,
        &server,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let actions = hosted.roster.actions.clone();
    let workspace = crate::common::WORKSPACE.to_owned();
    let id = WorkspaceId::parse(&workspace).unwrap();
    // The person's laptop arranged the workspace's windows on its canvas.
    let mut on_laptop = Browser::open(&hosted, &laptop, &server).await;
    on_laptop.put_layout_tree(&arranged(&workspace, (600, 120)));
    on_laptop.durable().await;

    let person = Caller::authenticated(
        root.verifying_key().to_bytes(),
        browser.cert.device_public_key,
    )
    .in_workspace(&workspace);
    let home = PageAddress {
        workspace: workspace.clone(),
        app: String::new(),
        show: None,
    };
    let opened =
        tilo_test_env::within("the front page opens", actions.open_page(id, &person, home))
            .await
            .unwrap()
            .expect("the front page is shown");
    let body = &opened.body;
    // The canvas, both windows on it, each where the laptop put it.
    assert!(
        body.contains("data-w=\"Map\""),
        "the front page is the canvas: {body}"
    );
    assert_eq!(
        body.matches("data-w=\"WindowFrame\"").count(),
        2,
        "both windows stand on it: {body}"
    );
    assert!(
        body.contains("Groceries") && body.contains("Launch"),
        "{body}"
    );
    // Each window's well opens its app's own page.
    let notes = format!("href=\"/w/{workspace}/notes\"");
    assert!(
        body.contains(&notes),
        "the note's window opens Notes: {body}"
    );
    let shown = tilo_test_env::within(
        "the note's window opens on its own page",
        actions.open_page(
            id,
            &person,
            PageAddress {
                workspace: workspace.clone(),
                app: "notes".into(),
                show: None,
            },
        ),
    )
    .await
    .unwrap();
    assert!(
        !matches!(shown, Err(PageRefusal::NoSuchApp | PageRefusal::NotMember)),
        "Notes opens from the canvas: {shown:?}"
    );

    // The laptop moves the task list; the open page's next pass moves it, and nothing else.
    on_laptop.put_layout_tree(&arranged(&workspace, (720, 200)));
    on_laptop.durable().await;
    let moved = tilo_test_env::within(
        "a pass over the front page",
        actions.act_on_page(id, opened.page, None),
    )
    .await
    .unwrap()
    .expect("the pass is answered");
    assert!(!moved.is_empty(), "the move reaches the open page");
    assert!(
        moved.iter().all(|patch| !patch.html.contains("Groceries")),
        "the window that stayed is not sent again: {moved:?}"
    );
    actions.close_page(id, opened.page);
}
