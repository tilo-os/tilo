# One app model for Rust and tilolang

A Tilo app is the same thing whichever language it is written in. It declares:

- types, which are its resources;
- actions on them;
- named views of a type, each returning described components that the host draws.

Its model and actions run in the session's document host. The shell owns windows and places.
A Rust app and a tilolang app are interchangeable to every shell, every search, every agent and
the REPL, and neither does anything to be. A Rust app looks and feels exactly as good as a
tilolang app: the same components, theme roles, motion, chrome, keyboard and touch behaviour,
accessibility and i18n, because it is drawn by the same host from the same described components.
Nothing a Rust app can do makes it look like a second-class app, or better than one.

This replaces the view-mount bridge first drafted in shell.md §8.4. Storage, where actions run,
identity and permissions are already decided in [apps.md](../../architecture/apps.md) §2–§8, and
this design does not repeat them. Below, each part maps what a Rust app does today to what it
does in the one model, by the names in the code.

## 1. Types are resources

| Today (Rust) | One model |
|---|---|
| One `#[resource(kind, label)]` model per app (`ResourceApp::Resource`) | Every `#[resource]` type is a resource type. Items in a collection are resources too, each with its own actions, as a tilolang type's items are. |
| `LiveRow` rows (`tilo-app` `framework/rows.rs`): untyped, with app-internal effects (`TubeEffect::OpenCreator`) | Gone. A row is an item resource of a declared type. What a press does is one of the type's actions, or `open`. |
| `ResourceSpec::resource_type` set only by tilolang's publisher | Set for every published resource, from its declaration: noun = the type's name in words (refined in the catalogue, i18n.md), app = the app. |
| The app's search pushed as indexed rows (`IndexedContent`) | Search matches published types and their fields (search.md). The app pushes nothing. |

Example: Tube declares `Channel`, `Video`, `Playlist` and `Subscriptions` as `#[resource]` types.
"youtube videos of mkbhd" then names a `Channel` by its type words, exactly as "my mails" names
Mail's root.

## 2. Views are named functions of a resource

| Today (Rust) | One model |
|---|---|
| `ResourceApp::view(cx) -> Element`: one imperative widget tree for the whole app, in its own window | `#[view] fn page(c: &Channel, cx: &mut ViewCx) -> Node`. Many named views per type, and `#[view(default)]` for the one a place shows unasked. |
| Widgets built and laid out in the app process, then drawn as scenes | A view returns described components (`ui::component` builders over the same catalogue tilolang's `ui.X` checks against). The app sends them as component patches (`tilo-render-protocol` `patch`), exactly as a tilolang app does. The UI host draws them in whatever place shows the resource. |
| Bindings and events wired to the app's widgets | The same binding and event protocol tilolang views use (`patch`: bindings, events, focus). An event fires an action of the type, or sets a view's state. |

So the shell places a Rust app's view exactly as it places a tilolang view: by resource and view
name. Only who computes the tree differs: the tilolang runtime for a package, the app's process
for a Rust app. Both send it over the same patch stream. A theme change re-styles both, because
both are described components.

A Rust view may still draw something the toolkit does not describe (a video surface, a browser
engine): a described component hosts it, such as `ui.VideoPlayer` or `ui.WebView`. Raw custom
drawing goes through `ui.Drawing` (components.md §4.1).

## 3. Windows belong to the shell

| Today (Rust) | One model |
|---|---|
| The app opens its window and draws its own chrome (`NavShell`, sidebar, app bar) | The app has no window and no chrome. Windows, places, a window's bar and its actions, navigation between an app's resources, and back/forward belong to the system. "Opening Tube" places its root resource with its default view. |
| Deep links and open-at-resource handled inside the app | `open(r)` places `r` with its default view, beside where it was asked, for every app. |

**Who the system is, per platform.** On Tilo, the shell draws windows and places, and the
session's document host keeps the records. On another platform (macOS, Windows, the web), the
same runtime is compiled into the app: it opens the platform's window, draws the same chrome
the shell would draw (each view's bar with the resource's actions, and navigation between
resources), and hosts the records itself. The app's code is the same on both. It never draws
chrome and never opens a window.

## 4. Storage, actions, identity, permissions

As apps.md §8 decided:
- A Rust app's model and actions run as an instance kind of the workspace's tilolang host.
- Its views are a client of the one launcher-made connection, and hold no identity, keys or transport.
- Permission checks leave the app.

This design adds nothing there.

### The runner's one source

What a Rust app's runner needs of storage is two things: **the state of its
resources**, as its model types read them, and **a way to submit actions** on
them, with the arguments a control supplies. Both are the record the host
opened for the window (`TypedRoot::sync` reads it, `Edits::apply` changes it),
for every launch: a window keeps its own record today, whichever host started
it.

**Where an app's model will run.** Not in the session beside the window, and
not on a second code path in the app: the model and verbs run as an instance
kind of the per-workspace host, and every window of the app, the person's and
an agent's alike, reaches that instance the way a window reaches any other
host. Until that instance kind exists, the window's own record is the only
path, and there is no launch an app can be told to take that keeps no record
of its own.

## 5. What a Rust developer writes

```rust
#[derive(Model)]
pub struct Channel { pub name: String, pub handle: String, pub videos: Collection<Video> }

#[resource(kind = "tube.channel")]
impl Channel {
    /// Follow this channel in your subscriptions.
    #[action] pub fn subscribe(&mut self, subs: &mut Subscriptions) { subs.add(self) }
}

/// The channel's page: who it is and its videos, newest first.
#[view(default)]
fn page(c: &Channel, cx: &mut ViewCx) -> Node {
    ui::page(c, |p| {
        p.header(ui::avatar(&c.name), ui::text(&c.name).role(Heading), cx.action(Channel::SUBSCRIBE));
        p.list(c.videos.newest(), |v| ui::list_row(&v.title).presents(v));
    })
}
```

The developer writes nothing for interop, search, places, windows or sync.

## 6. Migration

Order, each step keeping every app working, and each ending by deleting the code it replaced:
1. **Types:** every `#[resource]` publishes its `ResourceType`. `LiveRow` becomes typed item resources.
2. **Views:** `#[view]` functions producing described components over the patch stream; the shell places them by resource and view.
3. **Windows:** the SDK's window chrome becomes the app's root view. `ResourceApp::view` is removed.
4. **Storage and actions:** apps.md §8, steps 1–6.

Each step migrates every app at once, or with a ratchet that fails for an app still on the old way.
Tube goes first in each step, and proves it with the canvas query "youtube videos of <creator>",
which places the creator's page.
