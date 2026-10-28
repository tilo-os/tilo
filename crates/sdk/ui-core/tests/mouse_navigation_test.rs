use tilo_ui::State;
use tilo_ui::headless::FrameHarness;
use tilo_ui::theme::Theme;
use tilo_ui::widget::Router;
use tilo_ui::widget::Text;
use winit::event::{ElementState, MouseButton};

fn side_button(driver: &mut FrameHarness, button: MouseButton, state: ElementState) {
    driver.send(&tilo_ui::synthetic_input::mouse_input(state, button));
    driver.relayout();
}

#[test]
fn back_and_forward_buttons_walk_a_router_history() {
    let theme = Theme::dark();
    let router = Router::new("home", ["home", "settings"], &theme, |route, _, _| {
        Ok(Box::new(Text::new(route).with_id(route)))
    })
    .unwrap();
    let handle = router.handle();
    let mut driver = FrameHarness::new(State::new(), Box::new(router), (400, 300));
    handle.push("settings").unwrap();
    driver.relayout();
    assert!(driver.targets().contains_key("settings"));

    side_button(&mut driver, MouseButton::Back, ElementState::Released);
    assert_eq!(handle.current(), "settings", "a release alone does nothing");

    side_button(&mut driver, MouseButton::Back, ElementState::Pressed);
    side_button(&mut driver, MouseButton::Back, ElementState::Released);
    assert_eq!(handle.current(), "home");
    assert!(driver.targets().contains_key("home"));

    side_button(&mut driver, MouseButton::Forward, ElementState::Pressed);
    side_button(&mut driver, MouseButton::Forward, ElementState::Released);
    assert_eq!(handle.current(), "settings");
    assert!(driver.targets().contains_key("settings"));
}
