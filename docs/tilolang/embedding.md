# Run tilolang in your own program

tilolang compiles to WebAssembly, and the runtime that runs it is an ordinary Rust library. A
program of your own can compile a tilolang package, keep its resources, run its actions and
build its views without Tilo around it: the compiler is the `tilolang` crate and the runtime is
`tilolang-runtime`, both on crates.io under the MIT license.

This page builds one small program from start to end. `tools/sdk/published_crates_proof.py` builds
and runs exactly the code shown here, from the packages as crates.io would serve them, and
checks that it prints the output shown under [Output](#output).

## Dependencies

A new Cargo project needs the compiler, the runtime and, to print the view, `serde_json`:

<!-- file: Cargo.toml -->
```toml
[package]
name = "counter-host"
version = "0.1.0"
edition = "2024"

[dependencies]
tilolang = "0.5"
tilolang-runtime = "0.5"
serde_json = "1"
```

The runtime runs modules on wasmtime, compiling them to machine code in the background while
they start on the wasmi interpreter. Without its default `wasmtime` feature
(`default-features = false`) it runs on wasmi alone, which also builds for
`wasm32-unknown-unknown` and compiles much faster.

## The package

A package declares resource types, the actions that change them and the views that show them.
This one is a counter: a type with one field, an action that adds to it and a view with the
count and a button. The [guide](../../crates/lang/tilolang/README.md) covers the language.

<!-- file: counter.tl -->
```tilolang
app example.counter "Counter"

/// A count several people can add to at once.
type Counter {
    value: Int merge counter
}

/// Add an amount to the count.
action add(c: Counter, amount: Int) {
    c.value += amount
}

view counter(c: Counter) {
    stack {
        text("Count: {c.value}")
        button("+1", on: add(c, 1)) as plus
    }
}
```

## Compiling it

`tilolang::compile_package` turns the source into a WebAssembly module and the metadata that
describes it: its types, actions and views, each under a stable id. The ids come from the
package's ids lock, which a package keeps beside its source. What was stored under an id stays
readable while the declaration keeps it, so a field can be renamed without losing its data.
A declaration the lock does not name gets a new id, and the compiled package's lock
(`compiled.ids.to_text()`) holds it: save that, as the `tilolang` command saves `app.ids`.
`IdsLock::new()` is an empty lock, for a package whose ids may change on every run.

<!-- file: counter.ids -->
```text
Counter = ctype222
Counter.value = cvalue22
add@Counter = addact22
counter = cview222
counter#plus = cplus222
```

A compile error is a list of diagnostics, each with a code, a position and a message.

<!-- file: src/main.rs -->
```rust
use tilolang::IdsLock;
use tilolang_runtime::ids::{Actor, MemberId};
use tilolang_runtime::{AmStore, Runtime, SimpleAuthority, Value};

const SOURCE: &str = include_str!("../counter.tl");
const IDS: &str = include_str!("../counter.ids");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ids = IdsLock::parse(IDS)?;
    let compiled = tilolang::compile_package(SOURCE, &ids, &[]).map_err(|errors| {
        let shown: Vec<String> = errors.iter().map(|e| e.render("counter.tl")).collect();
        shown.join("\n")
    })?;
```

## Running it

A runtime keeps resources in a store and asks an authority whether each call may run.
`AmStore` keeps every resource as an Automerge document in memory; implementing the `Store`
trait keeps them anywhere else, and two `AmStore`s sync with `sync_with`. `SimpleAuthority`
decides by who wrote the code: a person's own call runs, and so does code calling an action of
its own package, while an action that reaches outside the workspace, or code of a package it was
not told to trust, asks first. Your program implements the `Authority` trait to decide what each
person may do. Loading the compiled package makes its types, actions and views available.

<!-- file: src/main.rs -->
```rust
    let runtime = Runtime::new(AmStore::new(), SimpleAuthority::new())?;
    runtime.load_compiled(&compiled)?;
```

## Actions and resources

Everything is done by someone: an actor is a member of the workspace, a person or an agent,
and every change records who made it. `create` makes a resource of a type, by its name.
An action is called by its id, with its arguments as values; the package's metadata finds the
id of the action its source calls `add`. `read` reads a field back.

<!-- file: src/main.rs -->
```rust
    let me = Actor::from(MemberId::person("me"));
    let counter = runtime.create("Counter")?;
    let add = &compiled.meta.action_named("add").ok_or("no action add")?.id;
    runtime.invoke_action(&me, (add.as_str(), vec![Value::resource(&counter), Value::Int(41)]))?;
    println!("value: {:?}", runtime.read(&counter, "value")?);
```

## Views

`open_view` runs a view for a person with its arguments and keeps it open: `tree()` is the
tree of nodes it built, with the shared components' kinds (`ui.Text`, `ui.Button`), their
properties and the events they answer. A part written `as plus` has a stable key, which the
view's metadata names. `press` runs what the button's `on:` says, as the person the view is
open for; the view then follows the change, and `poll_patches` says which properties of which
nodes changed, for a renderer that updates what it drew. `to_json` gives the whole tree as
JSON, for a host that draws it itself.

<!-- file: src/main.rs -->
```rust
    let view = compiled.meta.view_named("counter").ok_or("no view counter")?;
    let mut window = runtime.open_view(&me, &view.id, vec![Value::resource(&counter)])?;
    window.press(view.anchor_named("plus").ok_or("no button plus")?)?;
    println!("patches: {}", window.poll_patches()?.len());
    println!("value: {:?}", runtime.read(&counter, "value")?);
    println!("{}", serde_json::to_string_pretty(&window.tree().to_json())?);
    Ok(())
}
```

## Output

`cargo run` prints the count after the action, the number of patches the press made (one: the
text's value), the new count and the view's tree, whose keys come from the ids in
`counter.ids`.

<!-- output -->
```text
value: Int(41)
patches: 1
value: Int(42)
{
  "anchor": null,
  "bound": false,
  "children": [
    {
      "anchor": null,
      "bound": false,
      "children": [],
      "events": [],
      "key": "cview222.8",
      "kind": "ui.Text",
      "props": {
        "value": "Count: 42"
      }
    },
    {
      "anchor": "cplus222",
      "bound": false,
      "children": [],
      "events": [
        "press"
      ],
      "key": "cplus222",
      "kind": "ui.Button",
      "props": {
        "label": "+1"
      }
    }
  ],
  "events": [],
  "key": "cview222.7",
  "kind": "stack",
  "props": {}
}
```

## Drawing the tree

The tree is data: what to draw is up to the host. Tilo's own windows lay it out and draw it with
the shared component library, `tilo-ui`, which is being split out to be published beside these
crates.

## In the browser

The `tilolang` package on npm is the compiler and runtime built for a web page (the
`tilolang-web` crate, through wasm-bindgen). It compiles a package from its files
(`compile_package`), runs its app and answers its window as semantic HTML and CSS (`open_app`),
and takes what a person does on the page (`app_act`); every function takes and answers JSON
text. Its [README](../../crates/lang/tilolang-web/npm/README.md) shows a page that does this.
The crate is not on crates.io, and its functions are still being reworked, so they may change
before a release.
