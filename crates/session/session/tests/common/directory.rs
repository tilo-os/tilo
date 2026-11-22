//! Reading a site's directory from the shell, and what a shared desktop shows: the fixtures of
//! the tests that browse what a site lists.

use std::time::Duration;

use tilo_desktop::verbs::CHROME_RESOURCE;
use tilo_desktop_model::ownership::SharedDesktop;
use tilo_session::Session;
use tilo_sheets::directory::Status;

/// Point this computer at the community site `site` for the test's lifetime. Returns the
/// configuration's directory, which goes with it.
pub fn community_site(env: &mut tilo_test_env::ScopedEnv, site: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a directory for the configuration");
    let config = dir.path().join("community.toml");
    std::fs::write(&config, format!("site = \"{site}\"\n")).expect("the configuration is written");
    env.set("TILO_COMMUNITY_CONFIG", &config);
    dir
}

/// Open "Join a workspace…" and wait until the directories answered.
pub fn open_directory(session: &mut Session) {
    session
        .invoke(CHROME_RESOURCE, "join-workspace", &serde_json::Map::new())
        .expect("the session opens the directory");
    session.step(Duration::from_millis(1)).unwrap();
    wait_until_the_directory_answered(session);
}

/// Run the session until the sheet's directories answered the read it is on.
pub fn wait_until_the_directory_answered(session: &mut Session) {
    // The site's directories are answered by other runtimes, and a busy machine takes as long at
    // that as the suite allows, not a second count.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let state = session
            .workspace_directory()
            .expect("the session hosts the workspace directory")
            .directory()
            .status;
        if state != Status::Loading {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the directory answered"
        );
        session.step(Duration::from_millis(10)).unwrap();
    }
}

/// The workspaces this computer holds: the shell's id, the name shown and the workspace's id.
pub fn held(session: &Session) -> Vec<(String, String, String)> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .map(|held| {
                (
                    held.id().to_string(),
                    held.name.clone(),
                    held.id().to_string(),
                )
            })
            .collect()
    })
}

/// Where the shell draws the tile of window `tab` , in the surface's coordinates, as of the last
/// layout: `None` while it is not shown.
pub fn tile_here(session: &Session, tab: u64) -> Option<ui::kurbo::Rect> {
    session
        .handle()
        .shown_tiles()
        .into_iter()
        .find(|(shown, _)| *shown == tab)
        .map(|(_, rect)| rect)
}

/// Whether the shell drew someone's pointer at `(x, y)` of the tile of `tab` .
pub fn pointer_here(session: &Session, tab: u64, (x, y): (f32, f32)) -> bool {
    session
        .drawn_peer_pointers()
        .iter()
        .any(|(_, (at_x, at_y))| {
            tile_here(session, tab).is_some_and(|tile| {
                (at_x - (tile.x0 + f64::from(x))).abs() < 0.5
                    && (at_y - (tile.y0 + f64::from(y))).abs() < 0.5
            })
        })
}

/// The strips and windows a shell shows in `workspace` , in order: each strip by its name and the
/// titles of the windows in it.
pub fn layout(session: &Session, workspace: &str) -> Vec<(String, Vec<String>)> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .find(|held| held.id().to_string() == workspace)
            .map(|held| {
                held.strips
                    .iter()
                    .map(|strip| {
                        (
                            strip.name.clone(),
                            strip
                                .window_groups
                                .iter()
                                .flat_map(|group| group.tabs.iter().map(|tab| tab.title.clone()))
                                .collect(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// The strips and windows a shared desktop holds, as [`layout`] gives a shell's: what every
/// device showing the workspace should see.
pub fn recorded_layout(desktop: &SharedDesktop) -> Vec<(String, Vec<String>)> {
    let mut layout: Vec<(String, Vec<String>)> = desktop
        .tree
        .workspaces
        .first()
        .map(|workspace| {
            workspace
                .strips
                .iter()
                .map(|strip| {
                    (
                        strip.name.clone(),
                        strip
                            .window_groups
                            .iter()
                            .flat_map(|group| group.tabs.iter().map(|tab| tab.title.clone()))
                            .collect(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    layout.retain(|(_, windows): &(String, Vec<String>)| !windows.is_empty());
    layout
}
