//! An app's window opened beside one of its own tabs: it lands in that tab's window group, right
//! after it.

use super::tests::desk_with;
use super::*;
use crate::navigation::{ChromeEffect, Route};
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
            view: String::new(),
            pinned: false,
        })
    }
}

/// The shown strip's window groups, each as its tab titles and the title of its tab in front.
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
        .dispatch(ChromeEffect::OpenApp {
            app: "so.tilo.notes".into(),
            beside: Some(beside.tab),
            background: Some(background),
            float: None,
        })
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
        .dispatch(ChromeEffect::OpenApp {
            app: "org.example.other".into(),
            beside: Some(before[0].route.tab.clone()),
            background: None,
            float: None,
        })
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

/// A window an app opens floating stands over the desk beside the window it opened next to, where
/// the strip gives it room: the card the desk draws covers nothing the person was reading, and
/// every member's record keeps its place.
#[test]
fn a_window_an_app_opens_floating_stands_beside_the_window_it_opened_next_to() {
    let (mut frame, handle) = mount();
    floats_beside(&mut frame, &handle);
}

/// The window in use is the last of a strip whose windows fill the display: there is no room after
/// it, so the card stands before it, over the window the person was not using.
#[test]
fn a_float_opened_beside_the_last_window_of_a_full_strip_stands_before_it() {
    let (mut frame, handle) = mount();
    let last = handle.snapshot("g", vec![]).entries[1].route.clone();
    handle
        .dispatch(crate::navigation::DeskEffect::Focus { tab: last.tab })
        .unwrap();
    frame.relayout();
    floats_beside(&mut frame, &handle);
}

/// Open a window floating beside the one in front, and check it stands beside it, never over it.
fn floats_beside(frame: &mut ui::headless::FrameHarness, handle: &DesktopHandle) {
    let beside = handle
        .snapshot("g", vec![])
        .active
        .expect("a window in front");
    handle
        .dispatch(ChromeEffect::OpenApp {
            app: "so.tilo.notes".into(),
            beside: None,
            background: None,
            float: Some(true),
        })
        .unwrap();
    frame.relayout();
    frame.relayout();
    let area = frame
        .widget_mut::<ui::widget::GroupRow>()
        .map(|row| floats::area_of(row.get_rect()))
        .expect("the shown strip stands on the desk");
    let (group, float) = handle
        .read(|tree| {
            let workspace = tree.workspaces.first()?;
            let group = workspace
                .strips
                .first()?
                .window_groups
                .iter()
                .find(|group| workspace.arrangement.float_of(group.id).is_some())?;
            Some((group.id, workspace.arrangement.float_of(group.id)?))
        })
        .expect("the window the app opened floats");
    assert_eq!(
        handle.snapshot("g", vec![]).active.unwrap().group,
        group.to_string(),
        "the conversation a send opened is the window the person works in"
    );
    assert_ne!(
        group.to_string(),
        beside.group,
        "it stands in a window of its own, not the one it opened next to"
    );
    let opener = beside
        .group
        .parse::<u64>()
        .expect("a window group's id is a number");
    let (card, sender) = {
        let row = frame
            .widget_mut::<ui::widget::GroupRow>()
            .expect("the shown strip");
        let card = *row
            .floats()
            .get(&group)
            .expect("the row draws the card where the record keeps it");
        let sender = row
            .groups
            .iter()
            .find(|held| held.id == opener)
            .map(Widget::get_rect)
            .expect("the window it opened next to stands in the strip");
        (card, sender)
    };
    assert_eq!(
        card.size(),
        ui::kurbo::Size::new(ui::widget::FLOAT_SIZE.0, ui::widget::FLOAT_SIZE.1),
        "it stands at the size a floating window takes"
    );
    assert_eq!(
        floats::rect_of(float.resolved(area)),
        card,
        "the place every member's record keeps is the place this desk draws it at"
    );
    assert!(
        !card.overlaps(sender),
        "it stands beside the window it opened next to, never over what the person was reading: \
         card {card:?}, that window {sender:?}"
    );
}
