# tilo-app

The Rust SDK for [Tilo](https://code.tilo.so/tilo/tilo) apps. An app declares
its state as a typed resource, the changes it allows as actions, and views
bound to both. The SDK keeps the resource, records every change, and gives the
app history and undo, an action palette, and an interface that scripts, agents
and tests drive through the same actions a button invokes.

The same source runs on Linux, macOS and the web. Outside Tilo an app is an
ordinary window. Inside Tilo the same app also syncs between a person's devices and the people they
share it with, under the workspace's permissions; the app doesn't change for
that.

Most Tilo apps are written in [tilolang](https://tilolang.dev). Use `tilo-app`
when an app needs Rust: wrapping a native library, a custom renderer, or code
you already have.

## Use it

```toml
[dependencies]
ui = { package = "tilo-app", version = "0.1" }
```

Apps rename the package to `ui`. A counter, with its labels in a Fluent catalog
at `locales/en-US.ftl`:

```rust
use ui::prelude::*;

ui::embedded_catalog!(pub static CATALOG, ["en-US"]);

/// The state the SDK stores, versions and undoes.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Counter {
    pub count: i64,
}

/// Each `#[action]` is a verb people, scripts and agents can invoke; its doc
/// comment is its label, and returning `Err` refuses it without a change.
#[resource(kind = "example.counter")]
impl Counter {
    /// Add an amount to the count.
    #[action]
    pub fn add(&mut self, #[param("Amount to add")] amount: i64) -> Result<(), &'static str> {
        self.count = self.count.checked_add(amount).ok_or("the count is too large")?;
        Ok(())
    }

    /// Reset the count to zero.
    #[action(destructive)]
    pub fn reset(&mut self) {
        self.count = 0;
    }
}

pub struct App;

impl ResourceApp for App {
    type Resource = Counter;
    const ID: &'static str = "org.example.counter";
    const TITLE: &'static str = "Counter";

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn view(cx: &mut ViewCx<Counter>) -> Element {
        let count = cx.text(|counter| counter.count.to_string()).with_role(TextRole::Display);
        let add = cx.actions().bind_invocation(Button::new("add-one", "counter-add-one"), Counter::ADD.with(1));
        let reset = cx.button("counter-reset", Counter::RESET.invoke());
        Box::new(ui::column![count, ui::row![add, reset]])
    }
}

ui::main!(App);
```

The complete app, with its catalog, tests and screenshot scenarios, is
[examples/apps/counter](https://code.tilo.so/tilo/tilo/src/branch/main/examples/apps/counter).
`tilo-app` re-exports the [`tilo-ui`](https://crates.io/crates/tilo-ui) toolkit,
so an app reaches every shared component through `ui::`.

## Learn more

- [Rust apps guide](https://tilolang.dev/docs/rust-apps)
- [Tilo UI guidelines](https://tilolang.dev/docs/interface-guidelines)
- [Repository](https://code.tilo.so/tilo/tilo)

## License

MIT
