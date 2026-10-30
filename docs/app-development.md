# Build a Tilo app

A Tilo app is a Rust crate on the `tilo-app` SDK. It declares its state as a
typed resource, the changes it allows as actions, and views bound to both.
From that the framework provides storage, sync between devices and people,
history and per-author undo, permissions, search, the command palette, and the
interface scripts and agents use to drive the app without the screen. The same
source runs on Tilo, other Linux desktops, macOS, Windows, Android and the
web.

Tilo is in alpha: the SDK changes often. An app builds against the exact SDK
revision its `Cargo.lock` pins, so it keeps building while the SDK moves on.

## Install the tool

You need Rust through [rustup](https://rustup.rs) and Python 3.11 or newer as
`python3` (building the SDK stages its pinned fonts with a Python script; on
macOS, `brew install python`). Then:

```sh
cargo install --locked --git https://code.tilo.so/tilo/tilo.git tilo-app-cli
```

This installs `tilo-app`. `tilo-app --help` lists its commands; every command
accepts `--json` and then writes exactly one JSON document to stdout, so
scripts and agents can drive it. `tilo-app guide` has short offline workflows
(`start`, `actions`, `collaborate`, `permissions`, `network`, `verify`,
`publish`).

In a Tilo checkout, `cargo run --locked -p tilo-app-cli -- <command>` runs the
tool from source instead.

## Create an app

```sh
tilo-app new reading-list --id org.example.reading-list
cd reading-list
tilo-app dev
```

The id is the app's permanent name; use a namespace you control (a domain you
own, reversed), because publishing checks that you own it. `tilo-app new
--list-templates` lists the templates (`starter`, `document`, `collection`,
`graph`).

To learn one idea at a time, start from an example instead. Each app in
[`examples/apps`](../examples/README.md) is complete and tested:

```sh
tilo-app examples
tilo-app new my-list --from-example shared-checklist
```

`tilo-app dev` builds the app, opens it and rebuilds on every save; the open
document, screen and caret carry over, and a failed build leaves the last good
version running. The other everyday commands:

```sh
tilo-app run      # build once and open it
tilo-app test     # the Rust tests, plus the accessibility and action audit
tilo-app check    # the full gate: structure, format, Clippy, tests, scenarios
tilo-app shot     # render the app to an image without a window
```

A new app is a Cargo project: `src/` holds the app, `tilo-app.toml` is the
package manifest (id, entrypoint, sandbox), `surface.toml` records the public
surface, `locales/` holds the text, `tests/` the tests and `AGENTS.md` a short
guide for coding agents.

## The model: resources, actions, bound views

This is the whole of [`examples/apps/counter`](../examples/apps/counter),
slightly shortened. The state is a model:

```rust
use ui::prelude::*;

#[derive(Model, Default, Debug, Clone, PartialEq)]
#[model(check = Counter::within_limit)]
pub struct Counter {
    pub count: i64,
}

impl Counter {
    pub fn within_limit(&self) -> Result<(), Refusal> {
        if self.count.abs() > 1_000_000 {
            return Err(Refusal::new("the count is limited to a million either way"));
        }
        Ok(())
    }
}
```

Every change is an action: a method marked `#[action]` on the resource. Its
doc comment is its label, its parameters are typed, and returning `Err`
refuses it without changing anything. Setting a field directly
(`Counter::paths().count.set(42)`) is an edit at a typed path and goes through
the same checks.

```rust
#[resource(kind = "example.counter", label = "Counter")]
impl Counter {
    /// Add an amount to the count.
    #[action]
    pub fn add(&mut self, #[param("Amount to add")] amount: i64) -> Result<(), &'static str> {
        self.count = self.count.checked_add(amount).ok_or("the count would pass its limit")?;
        Ok(())
    }

    /// Reset the count to zero.
    #[action(destructive)]
    pub fn reset(&mut self) {
        self.count = 0;
    }
}
```

The view binds controls to fields and actions. A bound button is disabled
while its action is not allowed, and pressing it invokes the action, exactly
as the palette, an agent or a script would:

```rust
pub struct App;

impl ResourceApp for App {
    type Resource = Counter;
    const ID: &'static str = "so.tilo.example.counter";
    const TITLE: &'static str = "Counter";

    fn view(cx: &mut ViewCx<Counter>) -> Element {
        let count = cx.text(|counter| counter.count.to_string()).with_role(TextRole::Display);
        let actions = cx.actions();
        let add_one = actions.bind_invocation(Button::new("add-one", "counter-add-one"), Counter::ADD.with(1));
        let reset = cx.button("counter-reset-button", Counter::RESET.invoke());
        Box::new(ui::column![count, add_one, reset])
    }
}
```

`src/main.rs` is one line, `ui::main!(example_counter::App);`, which gives the
same app a window, the headless test modes and registration in the web page.
Button labels are keys into `locales/en-US.ftl`, which also says how each
action reads in history.

What this gives you without more code:

- **Collaboration and sync.** Actions become CRDT changes. Two people (or two
  devices) editing at once merge; see the `shared-checklist` example's tests.
- **History and undo.** Each action is a history entry with its author; undo
  takes back only your own actions. A destructive action that undo can take
  back runs at once and offers Undo; one it cannot take back is confirmed by
  the system, never by an app dialog.
- **Permissions.** Roles decide who may invoke which action; a viewer sees the
  state and cannot change it.
- **Search, palette, scripts and agents.** The resource and its actions are
  published (`tilo-app surface` writes them to `surface.toml`), so system
  search, the command palette and agents find and invoke them.

Apps start empty. Sample content belongs in tests, screenshots
(`ResourceApp::scenario`) or explicit demos. Apps contain no platform code:
files, network, processes and other system services come from the crates in
`crates/platform/`, which pick a backend per target.

## Test it

A `Replica` applies actions with no window, which is the fastest way to test
what the model allows:

```rust
#[test]
fn a_refused_action_changes_nothing() {
    let mut counter = Replica::<Counter>::new();
    counter.apply(Counter::ADD.with(10)).expect("add ten");
    assert!(counter.apply(Counter::ADD.with(i64::MAX)).is_err());
    assert_eq!(counter.state().expect("counter").count, 10);
}
```

`Replica::new().as_author("ada")` gives each replica an author for merge and
undo tests, and `TestApp::<App>::new()` runs the real app headlessly so a test
can press controls by id and read the published actions. Prefer semantic
assertions (roles, names, state) over pixels. In the Tilo checkout:

```sh
cargo test --locked -p example-counter
```

`tilo-app check` also fails on actions that read a clock, randomness, files or
the network, on missing translations, on controls bound to something the
resource does not publish, and on a public surface that differs from
`surface.toml`.

## Other platforms

`run`, `dev`, `build`, `check` and `package` take `--platform` (`linux`,
`macos`, `windows`, `android`, `web`) and `--arch` (`x86_64`, `aarch64`):

```sh
tilo-app run --platform web          # build once and serve the page
tilo-app check --platform all        # lint every other target too
tilo-app package --platform macos --arch aarch64
```

On other systems an app ships as one executable with the toolkit linked in.
[Android](android.md) covers the Android build.

## Publish

```sh
tilo-app keys new
tilo-app package
tilo-app publish
```

[Publishing](publishing.md) covers the publisher key, proving a namespace and
building releases from CI; [the registry](registry.md) describes how packages
are stored, verified and served.

## Reference

- The SDK source: [`crates/sdk/tilo-app`](../crates/sdk/tilo-app); browse the
  API with `cargo doc --locked -p tilo-app --no-deps --open`.
- The [interface guidelines](../crates/sdk/ui-core/HIG.md): layout,
  navigation, text and the shared controls.
- [Architecture](architecture/README.md): how apps fit the system.
