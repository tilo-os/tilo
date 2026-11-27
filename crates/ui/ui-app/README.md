# tilo-ui-app

The app runner of Tilo UI, the toolkit every [Tilo](https://code.tilo.so/tilo/tilo)
app and the Tilo shell draw with: the window (`App`), the headless host that
drives the same widgets with no window or GPU (`headless::Driver`), the action
palette, launch and frame profiling. It re-exports the toolkit,
[`tilo-ui`](https://crates.io/crates/tilo-ui), whole.

## Which crate to use

To build an app, depend on [`tilo-app`](https://crates.io/crates/tilo-app).
Depend on `tilo-ui-app` for a program that only draws.

## A window

<!-- file: Cargo.toml -->
```toml
[package]
name = "counter"
version = "0.1.0"
edition = "2024"

[dependencies]
tilo-ui-app = "0.5"
```

`App::from_factory` opens a window whose root widget the closure builds. A
`LocalState` is a value the program owns; `Text::from_state` shows it and
redraws when `set` changes it. With `--headless` the program drives the same
widgets through `Driver`, the runner's host with no window or GPU: it presses
the button by its id and prints what a screen reader would read.

<!-- file: src/main.rs -->
```rust
use tilo_ui_app::headless::Driver;
use tilo_ui_app::widget::{Button, Column, Text};
use tilo_ui_app::{App, LocalState, State, TextRole};

fn window(presses: &LocalState<u32>) -> Box<dyn tilo_ui_app::Widget> {
    let press = {
        let presses = presses.clone();
        Button::new("press", "Press").on_click(move || presses.set(presses.with(|n| n + 1)))
    };
    Box::new(
        Column::new()
            .push(Box::new(Text::new("Counter").with_role(TextRole::Heading).with_heading_level(1)))
            .push(Box::new(Text::from_state(presses, |n| format!("Presses: {n}"))))
            .push(Box::new(press)),
    )
}

fn main() {
    let presses = LocalState::new(0);
    if std::env::args().any(|argument| argument == "--headless") {
        let mut driver = Driver::new(State::new(), window(&presses), (480, 320));
        driver.relayout();
        driver.click_named("press").expect("the button is on screen");
        driver.relayout();
        for node in driver.inspection().root.nodes() {
            if let Some(semantics) = &node.semantics
                && let Some(name) = &semantics.name
            {
                println!("{:?}: {name}", semantics.role);
            }
        }
        return;
    }
    App::from_factory(State::new(), move || window(&presses))
        .with_window_title("Counter")
        .run();
}
```

<!-- output -->
```text
Heading { level: 1 }: Counter
StaticText: Presses: 1
Button: Press
```

## License

MIT.
