# tilo-resource

The resource layer of the Tilo app SDK. A Tilo app's state is a resource: a typed value the
system stores, versions and shares. This crate turns a Rust struct into one, and its methods
into typed actions.

A type derives `Model`, and an `#[resource]` impl declares its actions. From those declarations
the app gets, without writing it: CRDT storage and sync between devices and people, replayable
actions with a recorded author and time, history and per-change undo, permissions, search over
its text fields, and the action vocabulary that the command palette, agents and scripts call.
The crate has no rendering or window dependency; `tilo-app` binds resources to views.

Apps built on `tilo-app` normally use this crate through the SDK, which re-exports the resource
layer in its prelude. Depend on `tilo-resource` directly when declaring resources in a library,
or when testing one without a window.

## Use it

```toml
[dependencies]
tilo-resource = "0.1"
```

```rust
use tilo_resource::{ActionCx, Collection, Id, Model, resource};

#[derive(Model, Default)]
pub struct Todo {
    pub items: Collection<Item>,
}

#[derive(Model, Default)]
pub struct Item {
    pub title: String,
    pub done: bool,
}

#[resource(kind = "example.todo")]
impl Todo {
    /// Add an item.
    #[action]
    pub fn add(&mut self, cx: &mut ActionCx, title: String) {
        self.items.push(cx.id(), Item { title, done: false });
    }

    /// Mark an item done or not done.
    #[action]
    pub fn set_done(&mut self, item: Id, done: bool) -> Result<(), &'static str> {
        self.items.get_mut(&item).ok_or("no such item")?.done = done;
        Ok(())
    }
}

fn main() {
    let mut replica = tilo_resource::Replica::<Todo>::new();
    replica.apply(Todo::ADD.with("Buy milk")).unwrap();
    let id = replica.state().unwrap().items.ids().next().unwrap().clone();
    replica.apply(Todo::SET_DONE.with((id, true))).unwrap();
    assert!(replica.state().unwrap().items.values().all(|item| item.done));
}
```

`Replica` is the harness a test uses: it applies actions the way a control, the palette, an
agent or a script does, through the published action API, and reads back the resulting state.
An action method takes `&mut self` and the typed arguments it declares; `ActionCx` supplies the
ids and the time the action records. The same action, replayed from its recorded form on another
replica, produces the same state.

## Learn more

- Rust app documentation: https://tilolang.dev/docs/rust-apps
- Repository: https://code.tilo.so/tilo/tilo

## License

MIT
