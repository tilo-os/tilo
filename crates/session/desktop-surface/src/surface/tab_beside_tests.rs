//! An app's window opened beside one of its own tabs: it lands in that tab's
//! window group, right after it.

use super::tests::desk_with;
use super::*;
use crate::navigation::Route;
use ui::widget::{TileContent, TileHost};

/// The apps this desk runs, each opening a tab named by its id.
struct Apps;

impl TileSource for Apps {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }

    fn launch(&mut self, _: &Workspace, id: TabId, app: &str) -> Option<Tab> {
        let title = app.strip_prefix("so.tilo.")?.to_owned();
        Some(Tab {
            id,
            title: title.clone(),
            tile: tilo_desktop_model::TileContent::LocalApp {
                app_id: app.into(),
                label: title,
                note: String::new(),
            },
            app_title: None,
        })
    }
}

/// The shown strip's window groups, each as its tab titles and the title of
/// its tab in front.
fn group_titles(handle: &DesktopHandle) -> Vec<(Vec<String>, String)> {
    handle.read(|tree| {
        let strip = tree
            .workspaces
            .first()
            .and_then(|workspace| workspace.strips.first());
        strip
            .into_iter()
            .flat_map(|strip| &strip.window_groups)
            .map(|group| {
                let titles = group.tabs.iter().map(|tab| tab.title.clone()).collect();
                let front = group
                    .tabs
                    .iter()
                    .find(|tab| tab.id == group.active_tab)
                    .map(|tab| tab.title.clone())
                    .unwrap_or_default();
                (titles, front)
            })
            .collect()
    })
}

fn mount() -> (ui::headless::FrameHarness, DesktopHandle) {
    let (surface, handle) = desk_with(2, Box::new(Apps));
    let mut frame =
        ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(surface), (1280, 800));
    frame.relayout();
    (frame, handle)
}

fn open_beside(
    frame: &mut ui::headless::FrameHarness,
    handle: &DesktopHandle,
    beside: Route,
    background: bool,
) {
    handle
        .dispatch(
            &Command::TabOpenBeside {
                generation: "g".into(),
                app: "so.tilo.notes".into(),
                beside,
                background,
            },
            "g",
        )
        .unwrap();
    frame.relayout();
}

#[test]
fn an_apps_new_tab_lands_right_after_its_own() {
    let (mut frame, handle) = mount();
    let before = handle.snapshot("g", vec![]).entries;
    open_beside(&mut frame, &handle, before[0].route.clone(), false);

    assert_eq!(
        group_titles(&handle),
        [
            (
                vec!["App0".to_owned(), "notes".to_owned()],
                "notes".to_owned()
            ),
            (vec!["App1".to_owned()], "App1".to_owned()),
        ],
        "the new tab is in the app's own window group, in front"
    );
    let snapshot = handle.snapshot("g", vec![]);
    assert_eq!(snapshot.entries.len(), 3);
    let active = snapshot.active.unwrap();
    assert_eq!(active.group, before[0].route.group, "focused in its group");
    assert_ne!(active.tab, before[0].route.tab);
}

#[test]
fn a_tab_opened_behind_takes_no_focus() {
    let (mut frame, handle) = mount();
    let before = handle.snapshot("g", vec![]).entries;
    open_beside(&mut frame, &handle, before[0].route.clone(), true);

    assert_eq!(
        group_titles(&handle)[0],
        (
            vec!["App0".to_owned(), "notes".to_owned()],
            "App0".to_owned()
        ),
        "the tab behind leaves the front tab in front"
    );
    assert_eq!(
        handle.snapshot("g", vec![]).active.unwrap(),
        before[0].route,
        "the window the person worked in keeps focus"
    );
}

#[test]
fn a_refused_launch_leaves_no_tab() {
    let (mut frame, handle) = mount();
    let before = handle.snapshot("g", vec![]).entries;
    handle
        .dispatch(
            &Command::TabOpenBeside {
                generation: "g".into(),
                app: "org.example.other".into(),
                beside: before[0].route.clone(),
                background: false,
            },
            "g",
        )
        .unwrap();
    frame.relayout();

    assert_eq!(
        group_titles(&handle),
        [
            (vec!["App0".to_owned()], "App0".to_owned()),
            (vec!["App1".to_owned()], "App1".to_owned()),
        ],
        "an app that opens nothing leaves no tab"
    );
    assert_eq!(handle.snapshot("g", vec![]).entries.len(), before.len());
}
