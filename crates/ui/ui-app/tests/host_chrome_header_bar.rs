//! The header bar a window outside the Tilo shell carries: its History menu and the commands it
//! hands back, its controls' sizes, and what refreshes it.
use tilo_ui_app::App;
use tilo_ui_app::headless::Driver;
use tilo_ui_app::host_chrome_testing::{
    CommandGroup, HeaderBar, HeaderRequest, HostCommand, resize_edge,
};
use tilo_ui_base::widget::Widget;

fn app_with_history() -> App {
    tilo_ui_app::app::testing::native_chrome_app()
}

fn driver(commands: &[HostCommand], theme: tilo_ui_base::Theme) -> Driver {
    let mut driver = Driver::for_root(
        tilo_ui_app::State::new(),
        HeaderBar::new("Notes", commands),
        (900, 120),
        theme,
    );
    driver
        .root_mut::<HeaderBar>()
        .set_viewport(vello::kurbo::Rect::new(0.0, 0.0, 900.0, 600.0));
    driver.relayout();
    driver
}

#[test]
fn history_menu_lists_the_command_model_and_hands_back_the_chosen_command() {
    let mut app = app_with_history();
    let commands = app.host_commands_for_test(true);
    let mut driver = driver(&commands, tilo_ui_base::Theme::dark());
    let below = vello::kurbo::Point::new(600.0, 110.0);
    assert!(
        !driver.root_mut::<HeaderBar>().claims(below),
        "the app keeps its area"
    );
    driver.click_named("host-header-history").unwrap();
    let bar = driver.root_mut::<HeaderBar>();
    assert!(bar.menu.is_open());
    assert!(bar.claims(below), "an open menu takes every pointer event");
    let labels: Vec<_> = bar.menu.items().iter().map(|item| item.label()).collect();
    let history: Vec<_> = commands
        .iter()
        .filter(|command| {
            matches!(
                command.group,
                CommandGroup::History | CommandGroup::RecentChanges
            )
        })
        .collect();
    assert_eq!(labels[0], Some("Show History"));
    assert_eq!(labels[1], None, "recent changes follow a separator");
    assert_eq!(labels.len(), history.len() + 1);
    let show = bar.menu.item_rect(0).unwrap().center();
    drop(bar);
    driver.click_at(show.x, show.y);
    let requests = driver.root_mut::<HeaderBar>().take_requests();
    let [HeaderRequest::Command(invocation)] = requests.as_slice() else {
        panic!("choosing Show History hands back one command: {requests:?}");
    };
    assert_eq!(Some(invocation.clone()), history[0].invocation());
    assert!(
        app.invoke_host_command_for_test(invocation),
        "the palette's API opens it"
    );
    tilo_ui_app::app::testing::layout(&mut app);
    assert!(
        app.host_commands_for_test(false)
            .iter()
            .any(|command| command.binding.address().action == "hide"),
        "History opened"
    );
}

#[test]
fn a_live_window_sizes_every_control_to_its_content() {
    let mut app = app_with_history();
    let mut bar = HeaderBar::new("Notes", &app.host_commands_for_test(true));
    let (mut tree, mut nodes) = (taffy::TaffyTree::new(), std::collections::HashMap::new());
    let theme = tilo_ui_base::Theme::light();
    let mut text = tilo_ui_base::TextRenderer::new();
    bar.solve_layout(&mut tree, &mut nodes, &theme, &mut text, 800.0)
        .expect("the bar settles");
    let controls: Vec<_> = Widget::children(&bar.row)
        .into_iter()
        .filter_map(|child| Some((child.test_id()?.to_owned(), child.get_rect())))
        .collect();
    assert_eq!(controls.len(), 5, "{controls:?}");
    let height = controls[0].1.height();
    for (id, rect) in &controls {
        assert!(rect.width() >= height * 0.75, "{id} is squeezed: {rect:?}");
    }
    assert!(
        (controls[0].1.width() - controls[1].1.width()).abs() < 0.5,
        "History is a glyph like the others: {controls:?}"
    );
    assert!(
        controls.windows(2).all(|pair| pair[0].1.x1 <= pair[1].1.x0),
        "controls never overlap: {controls:?}"
    );
}

#[test]
fn a_bar_is_kept_until_what_it_presents_changes() {
    let mut app = app_with_history();
    let commands = app.host_commands_for_test(true);
    let bar = HeaderBar::new("Notes", &commands);
    assert!(
        bar.presents("Notes", &app.host_commands_for_test(true)),
        "a refresh keeps it"
    );
    assert!(!bar.presents("Other", &commands), "a new title replaces it");
    let search = commands
        .iter()
        .find(|command| command.group == CommandGroup::Search)
        .and_then(HostCommand::invocation)
        .unwrap();
    assert!(app.invoke_host_command_for_test(&search));
    assert!(
        !bar.presents("Notes", &app.host_commands_for_test(true)),
        "Search stepping aside while the palette is open replaces it"
    );
}

#[test]
fn framework_history_apps_search_from_the_header_bar() {
    let mut app = tilo_ui_app::app::testing::native_chrome_app();
    let commands = app.host_commands_for_test(false);
    let mut driver = driver(&commands, tilo_ui_base::Theme::light());
    driver.click_named("host-header-search").unwrap();
    let requests = driver.root_mut::<HeaderBar>().take_requests();
    let [HeaderRequest::Command(invocation)] = requests.as_slice() else {
        panic!("the search toggle hands back its command: {requests:?}");
    };
    assert!(
        app.invoke_host_command_for_test(invocation),
        "the palette's API opens it"
    );
    let search = app
        .host_commands_for_test(false)
        .into_iter()
        .find(|command| command.group == CommandGroup::Search)
        .unwrap();
    assert!(!search.enabled, "the open palette answers its own shortcut");
}

#[test]
fn framework_history_menu_opens_at_a_recent_change() {
    let mut app = tilo_ui_app::app::testing::native_chrome_app();
    let commands = app.host_commands_for_test(true);
    let mut driver = driver(&commands, tilo_ui_base::Theme::light());
    driver.click_named("host-header-history").unwrap();
    let bar = driver.root_mut::<HeaderBar>();
    let labels: Vec<_> = bar.menu.items().iter().map(|item| item.label()).collect();
    assert_eq!(labels.len(), 4, "Show History, a separator and two changes");
    assert!(
        labels[2..]
            .iter()
            .all(|label| label.is_some_and(|label| label.contains(" by "))),
        "a change names who made it: {labels:?}"
    );
    let newest = bar.menu.item_rect(2).unwrap().center();
    drop(bar);
    driver.click_at(newest.x, newest.y);
    let requests = driver.root_mut::<HeaderBar>().take_requests();
    let [HeaderRequest::Command(invocation)] = requests.as_slice() else {
        panic!("choosing a change hands back one command: {requests:?}");
    };
    assert!(app.invoke_host_command_for_test(invocation));
    tilo_ui_app::app::testing::layout(&mut app);
    let show = app
        .host_commands_for_test(false)
        .into_iter()
        .find(|command| command.group == CommandGroup::History)
        .unwrap();
    assert_eq!(show.label, "Hide History", "History opened");
}

#[test]
fn search_toggle_and_window_controls_ask_the_app_and_the_runner() {
    let mut app = app_with_history();
    let commands = app.host_commands_for_test(false);
    let mut driver = driver(&commands, tilo_ui_base::Theme::light());
    for id in [
        "host-header-search",
        "host-header-minimize",
        "host-header-maximize",
        "host-header-close",
    ] {
        driver.click_named(id).unwrap();
    }
    let search = commands
        .iter()
        .find(|command| command.group == CommandGroup::Search)
        .and_then(HostCommand::invocation)
        .unwrap();
    assert_eq!(
        driver.root_mut::<HeaderBar>().take_requests(),
        [
            HeaderRequest::Command(search),
            HeaderRequest::Minimize,
            HeaderRequest::ToggleMaximize,
            HeaderRequest::Close,
        ]
    );
    let bar = driver.targets()["host-header-bar"];
    let history = driver.targets()["host-header-history"];
    let empty = vello::kurbo::Point::new(history.x1 + 16.0, bar.center().y);
    driver.click_at(empty.x, empty.y);
    driver.click_at(empty.x, empty.y);
    assert_eq!(
        driver.root_mut::<HeaderBar>().take_requests(),
        [HeaderRequest::Move, HeaderRequest::ToggleMaximize]
    );
}

#[test]
fn frame_edges_resize_and_the_inside_does_not() {
    use vello::kurbo::{Point, Rect};
    use winit::window::ResizeDirection as Edge;
    let theme = tilo_ui_base::Theme::dark();
    let window = Rect::new(0.0, 0.0, 800.0, 600.0);
    let at = |x, y| resize_edge(Point::new(x, y), window, &theme);
    assert_eq!(at(2.0, 2.0), Some(Edge::NorthWest));
    assert_eq!(at(798.0, 598.0), Some(Edge::SouthEast));
    assert_eq!(at(400.0, 1.0), Some(Edge::North));
    assert_eq!(at(1.0, 300.0), Some(Edge::West));
    assert_eq!(at(400.0, 300.0), None);
}

#[test]
fn header_bar_shots_in_every_theme() {
    let Ok(directory) = std::env::var("TILO_HOST_CHROME_SHOTS") else {
        return;
    };
    let mut app = app_with_history();
    let commands = app.host_commands_for_test(true);
    for (name, theme) in [
        ("light", tilo_ui_base::Theme::light()),
        ("dark", tilo_ui_base::Theme::dark()),
        ("high-contrast", tilo_ui_base::Theme::high_contrast()),
    ] {
        let mut driver = Driver::for_root(
            tilo_ui_app::State::new(),
            HeaderBar::new("Notes", &commands),
            (900, 260),
            theme,
        );
        driver
            .root_mut::<HeaderBar>()
            .set_viewport(vello::kurbo::Rect::new(0.0, 0.0, 900.0, 260.0));
        driver.relayout();
        driver.click_named("host-header-history").unwrap();
        driver.render_png(&std::path::Path::new(&directory).join(format!("header-bar-{name}.png")));
    }
}
