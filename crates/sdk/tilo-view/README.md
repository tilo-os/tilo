# tilo-view

The view layer for Rust apps and Rust-written views on Tilo. A view is a function of a resource
that returns described components: a `Node` tree built from the `ui` builders, where every
component and every prop is checked against the toolkit's catalogue at the call site.

The tree is the same tree a tilolang view builds, keyed the same way, so the host draws both
with one theme, one set of motion primitives and one keyboard and accessibility model. Nothing
in a view names a window, a place or a screen; the tree travels on the component-patch stream
(`patch`, re-exported here), and the host draws it wherever the resource is shown.

A crate that declares views in Rust depends on this crate and on `tilo-view-macros`, the
companion crate that provides the `#[view]` attribute. The tilolang runtime draws the views a
linked crate registered through `ViewRegistry`, which finds them by resource kind and view
name. An app using the `tilo-app` facade usually composes views through the framework's context
over the same component catalogue; depend on `tilo-view` directly when you declare views for a
tilolang package or for a host of your own.

## Use it

```toml
[dependencies]
tilo-view = "0.1"
```

```rust
use tilo_view::ui::{self, Gap};
use tilo_view::{Node, ViewCx};

fn page(cx: &ViewCx, title: &str) -> Node {
    ui::stack(
        &cx.key("page"),
        Gap::Sibling,
        vec![
            ui::TextBuilder::new(&cx.key("title"), title)
                .role(ui::TextRole::Title)
                .into(),
            ui::TextBuilder::new(&cx.key("hint"), "Nothing here yet").into(),
        ],
    )
}
```

`ViewCx` carries what the view is handed besides the resource: `key` names a node to the host,
and events, opened resources and the view's own state attach through the context. In an app the
function is marked `#[view]` (or `#[view(default)]`) from `tilo-view-macros`, and its resource
implements `ViewResource`, whose `KIND` and `APP` constants say which resource type and app the
view belongs to. `ViewRegistry::get` and `ViewRegistry::resolve` look a view up by those.

`ui` holds a builder for every component in the catalogue, generated from the same description
the toolkit uses. A component's required props are the builder's constructor arguments, its
optional props are methods, and the builder converts into a `Node`. Layouts (`stack`, `row`,
`column`, `page`) and their `Gap` rhythms are plain functions.

## Learn more

- Rust app documentation: https://tilolang.dev/docs/rust-apps
- Repository: https://code.tilo.so/tilo/tilo

## License

MIT
