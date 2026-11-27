# Tilo UI

Tilo UI is the Rust toolkit that the Tilo shell and every Tilo app draw with. It has the
components a window is built from (headings, buttons, fields, lists, tables, sheets, navigation and
inspectors), layout for any window size, text shaping and editing, theming, accessibility through
AccessKit, and animation through the shared motion primitives. It draws with the vello renderer on
wgpu, and on the CPU where there is no GPU.

Depend on `tilo-app` to build an app. It re-exports this toolkit and turns on the model an app is
made of: resources kept in a synced document, typed actions that change them, and views
bound to both, with history and undo, the command palette, permissions and search. Depend on
`tilo-ui-app` for a program that only draws, such as a custom surface or a tool whose state
does not need to be a shared resource: it is the toolkit, `tilo-ui`, with the runner that opens
its window and the headless host that drives it, and it compiles none of Tilo's records, sync,
settings store or identity. [Build an app](../app-development.md) covers `tilo-app`;
this page builds a window on the toolkit alone.

## A window with the shared components

A new Cargo project depends on the toolkit and its runner:

<!-- file: Cargo.toml -->
```toml
[package]
name = "greeter"
version = "0.1.0"
edition = "2024"

[dependencies]
tilo-ui-app = "0.5"
```

The default `native` feature brings the winit event loop that opens the window, the AccessKit
adapter that exports what it shows to screen readers, and the process services through which a
program reaches the clipboard, file choosers and notifications. Two parts are features of their
own: `webview` adds the `WebView` widget, which shows a page through the Chromium helper, and
`content` adds the device's content store that a Tilo session hands its apps. A program that
shows a page turns `webview` on. The `host` feature is what Tilo's own apps run under (the
launcher, the session's daemons and its settings file); a program on the toolkit alone leaves it
off. With that in place,
`App::from_factory` takes the app's document (`State::new()` is an empty one) and a closure that
builds the window's root widget, and `run` opens the window on the platform event loop.

Widgets are values you build and nest, and they are the components [a tilolang view](../designs/app-language/README.md) draws too.
`Column` stacks them, `Text` shows words, `TextInput` edits them, `Button` is a press, and
`ListRow` is one row of a list.

The example keeps its state in two `LocalState` handles. A `LocalState` is typed state that lives
in your process rather than in the app's document. A widget bound to one reads it again when it
changes: `TextInput::from_state` and `Text::from_state` take the handle, and `set` publishes a new
value. The button reads the name and sets the greeting, so the label changes without a resource or
an action.

`--headless` runs the same program through `Driver`, the toolkit's host with no window, GPU
or process services. It lays the tree out at the given viewport, dispatches input as the window
would, and lets the program read what was laid out. The output below is what that path prints;
without the flag the program opens the window.

<!-- file: src/main.rs -->
```rust
use tilo_ui_app::headless::{Driver, InspectionSnapshot};
use tilo_ui_app::widget::{Button, Column, ListRow, Text, TextInput};
use tilo_ui_app::{App, LocalState, State, TextRole};

/// The window's tree: a heading, a name field, the button that greets with it, the greeting, and
/// a list.
fn tree(name: &LocalState<String>, greeting: &LocalState<String>) -> Box<dyn tilo_ui_app::Widget> {
    let greet = {
        let name = name.clone();
        let greeting = greeting.clone();
        // The first argument is the id a press on the button is addressed by.
        Button::new("greet", "Greet").on_click(move || {
            let who = name.with(|text| text.trim().to_owned());
            let who = if who.is_empty() { "world".to_owned() } else { who };
            greeting.set(format!("Hello, {who}!"));
        })
    };
    let people = Column::new()
        .push(Box::new(ListRow::new("ada", "Ada Lovelace").subtitle("Programming")))
        .push(Box::new(ListRow::new("alan", "Alan Turing").subtitle("Computing")));
    Box::new(
        Column::new()
            .push(Box::new(
                // The role sets the type size; the heading level is what assistive technology
                // announces.
                Text::new("Tilo UI")
                    .with_role(TextRole::Heading)
                    .with_heading_level(1),
            ))
            .push(Box::new(Text::new("A window drawn by the shared components.")))
            .push(Box::new(
                TextInput::from_state(name)
                    .with_id("name")
                    .with_accessible_name("Name")
                    .with_placeholder("Your name"),
            ))
            .push(Box::new(greet))
            // The closure turns the state's value into the words the label shows.
            .push(Box::new(Text::from_state(greeting, |words| words.clone())))
            .push(Box::new(people)),
    )
}

fn main() {
    let name = LocalState::new(String::new());
    let greeting = LocalState::new("No greeting yet.".to_owned());

    if std::env::args().any(|argument| argument == "--headless") {
        // The viewport is in logical pixels.
        let mut driver = Driver::new(State::new(), tree(&name, &greeting), (720, 480));
        name.set("Ada".to_owned());
        driver.relayout();
        println!("{}", describe(&driver.inspection()));
        driver.click_named("greet").expect("the Greet button is on screen");
        driver.relayout();
        println!("{}", describe(&driver.inspection()));
        return;
    }

    let (name, greeting) = (name.clone(), greeting.clone());
    App::from_factory(State::new(), move || tree(&name, &greeting))
        .with_window_title("Tilo UI")
        .run();
}

/// Every laid-out widget that reports accessibility semantics, as its type, its role, its name
/// and, for a field, its value.
fn describe(snapshot: &InspectionSnapshot) -> String {
    let mut lines = Vec::new();
    for node in snapshot.root.nodes() {
        if let Some(semantics) = &node.semantics {
            let mut line = vec![format!("{:<10}", node.widget), format!("{:?}", semantics.role)];
            if let Some(name) = &semantics.name {
                line.push(name.clone());
            }
            if let Some(value) = &semantics.value {
                line.push(format!("= {value}"));
            }
            lines.push(line.join("  "));
        }
    }
    lines.join("\n")
}
```

## Output

`describe` walks the laid-out tree and prints each widget that reports semantics: its type, its
accessibility role, the name a screen reader announces, and a field's value after `=`. The
headless path sets the name to Ada before the frame it prints, so the field shows it. The button is
found by its id, `greet`; pressing it greets with the name, and the second snapshot shows the new
label.

<!-- output -->
```text
Text        Heading { level: 1 }  Tilo UI
Text        StaticText  A window drawn by the shared components.
TextInput   TextInput  Name  = Ada
Button      Button  Greet
Text        StaticText  No greeting yet.
ListRow     Group  Ada Lovelace  = Programming
ListRow     Group  Alan Turing  = Computing
Text        Heading { level: 1 }  Tilo UI
Text        StaticText  A window drawn by the shared components.
TextInput   TextInput  Name  = Ada
Button      Button  Greet
Text        StaticText  Hello, Ada!
ListRow     Group  Ada Lovelace  = Programming
ListRow     Group  Alan Turing  = Computing
```

## Theming and appearance

Components paint from a `Theme`: one set of tokens for colour, type, spacing, shape, material and
motion. `Theme::dark()` and `Theme::light()` are the two stock themes, and a theme also carries a
text scale and a reduced-motion flag.

A window running outside Tilo takes its appearance from the operating system: light or dark,
higher contrast, reduced motion, and the accent colour, which the window uses for focus rings,
carets and selections. The window follows a change while it is open.
Inside Tilo the shell supplies the appearance through the workspace's settings. For a test or a
capture, `Driver::new_with_theme` takes a theme explicitly.

The [interface guidelines](../../crates/ui/ui-core/HIG.md) describe the tokens and their use, and
[layout](../designs/app-language/layout.md) covers how the parts size and place themselves at any
window size.

## Accessibility

Every widget reports an accessibility node: a role, a name, a value and an accessibility state
(focused, checked, disabled), along with the actions a person can take on it. The native runner builds the
window's AccessKit tree from those nodes, so a screen reader reads the words the window shows, and
keyboard focus, selection and the input method go through the same widgets that draw.

The same nodes are available without a window. `Driver::inspection` returns the laid-out
widget tree and its accessibility nodes as one snapshot, and `Driver::accessibility_audit`
reports the nodes that break the toolkit's accessibility contract. Both read the tree the live
window exports, so a test asserts on what a screen reader would get.

`Text::with_role` sizes the words, and `with_heading_level` sets the level a screen reader
announces. `TextInput::with_accessible_name` gives a field the name it is spoken as, which is not
its placeholder. The [interface guidelines](../../crates/ui/ui-core/HIG.md) say what each control
should announce.

## Building for the web

With `default-features = false` the toolkit builds without the native runner and the native
process services. That is the build for `wasm32-unknown-unknown`:

```toml
[dependencies]
tilo-ui-app = { version = "0.5", default-features = false }
```

A browser host drives the widget tree itself. `Driver` lays it out and dispatches the events
your page delivers, `Driver::frame_output` gives the frame as a vello `Scene` with the same
accessibility nodes, and `Driver::composition` with `tilo_ui_app::composition::page::Page` writes it as
semantic HTML styled from the theme's tokens. Clipboard, file choosers and notifications go
through `tilo_ui_app::process`: install browser-backed `ProcessServices` with
`process::install_default`, and without one those effects do nothing outside your program.

The components lay out and behave the same on the web, and your page gets the same accessibility
tree. [Components](../designs/app-language/components.md) lists them, and
the [interface guidelines](../../crates/ui/ui-core/HIG.md) apply to any host that draws them.
