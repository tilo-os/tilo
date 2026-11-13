# Draw a part of the shell

Parts of the Tilo shell are **slots**: the window menu, the bar, the
switcher, the overview, quick settings, the launcher, notifications, the layout
widgets, the people list, the emoji picker, the on-screen displays, the
workspace menu, the window decoration and the window manager, plus every
sheet that is not asking the person to trust something (`sheet:power`,
`sheet:display`, `sheet:shortcuts`, `sheet:directory`). Each slot is drawn by
a view, and the person can bind a slot to a view of their own — one their app
publishes — in place of the one the shell ships with.

## What a slot's view reads

The session publishes what a part of the shell is as a resource. The window
menu's resource is `desktop:window-menu`: the window it is for, where it
stands, and the rows that window offers. Its verbs are `open`, `close` and
`choose{row}`.

Read it the way any app reads a resource it does not own — through the door,
with the reading kept live:

```rust
let menu = ui::collaboration::SessionResource::open("desktop:window-menu")?;
let Some(Ok(reading)) = menu.latest() else {
    return Ok(()); // no reading yet
};
for row in reading["rows"].as_array().into_iter().flatten() {
    // draw `row["label"]`, and keep `row["id"]` for the press
}
```

The reading the slot's *first* frame draws is the shell's own resource: a slot
is presentation, and a view that keeps state of its own has nowhere to keep
it.

## What a slot's view may change

Only through the resource's own verbs, and each one is a child invocation of
your app, so it carries your app's attribution and grants like any other
action:

```rust
menu.invoke("choose", &json!({ "row": "window-close-window" }))?;
```

A view never places itself. Where the slot stands, whether it holds the
keyboard and what a press outside it closes come from the shell's surface
stack (`desktop:overlays`). What your view draws is the content.

## Bind the slot

The binding is a setting of the person's, `settings:shell-views`, saved with
what made it and who, and it follows them to their other devices:

```json
{ "bindings": { "window-menu": { "app": "so.tilo.example.menu", "view": "menu" } } }
```

Bind it through the session's door, as an app, an agent or a script does:

```rust
let views = ui::collaboration::SessionResource::open("settings:shell-views")?;
views.invoke(
    "bind",
    &json!({ "slot": "window-menu", "app": "so.tilo.example.menu", "view": "menu" }),
)?;
```

`"on": "native"` or `"on": "web"` binds the slot on one kind of device only,
over the binding for every device. `reset` with a slot shows the shell's own
view again:

```rust
views.invoke("reset", &json!({ "slot": "window-menu" }))?;
```

The session starts the app you bound the way it starts its own panels (the
program `tilo-` and the app id's name after `so.tilo.`), puts its window in
the slot while the reading says the part is up, and swaps it on the frame
after a rebind: the part is up before and after, and the reading behind it
never moves. The app draws the slot with the view it publishes for that slot;
the binding names the view too, so the person and their tools can tell which
one was meant. If your app is not installed on the machine, the slot stays
with the shell's own view rather than going blank.

## Draw it in tilolang

A view can be a tilolang package, which the session runs in its own process
rather than starting as a program. A binding whose app this machine installed
as a package — its payload holds `app.wasm` — is run here, and so is one whose
program path is a directory holding `app.wasm`, which is how an app you are
still building is pointed at. Anything else is an app of its own, started by
its program name as above.

The package is compiled against the surface the session publishes for the slot
— for the window menu, the resource `desktop:window-menu` of kind
`desktop.window-menu`, which a package reads as `desktop.WindowMenu`:

```toml
[[resources]]
kind = "desktop.window-menu"
label = "Window menu"
fields = [
  { path = "tab", type = { option = "text" } },
  { path = "anchor", type = { option = { named = "Anchor" } } },
  { path = "rows", type = { collection = { named = "Row" } } },
]

[[resources.verbs]]
id = "open"
kind = "outside"
params = [
  { name = "tab", required = false, type = { option = "text" } },
  { name = "anchor", required = false, type = { option = "text" } },
]

[[resources.verbs]]
id = "close"
kind = "outside"

[[resources.verbs]]
id = "choose"
kind = "outside"
params = [{ name = "row", required = true, type = "text" }]
```

A row is what the menu shows: its id, its label, whether the window can choose
it, and the reason beside it when it cannot. The view takes the slot's type as
its reading, and every field of it is what the session publishes:

```tilo
app so.tilo.mymenu "A menu of mine"

use desktop as desktop

view menu(m: desktop.WindowMenu) {
  column {
    for row in m.rows {
      button(row.label, on: desktop.choose(m, row: row.id))
    }
    button("Close", on: desktop.close(m))
  }
}
```

Reading the slot changes nothing; when the session publishes a new reading the
view runs again over it, so a menu left open while its window changes offers
what that window offers now. What the view invokes names an app and an action
of its own, but the view itself changes nothing: the session hands the call to
the same door every other caller goes through — the address is the slot's
resource and the verb, the arguments are converted to the shapes the verb's
parameters declare — so the checks the door makes of any caller (grants,
consent, destructive actions) are the checks it makes of the view's press.
Every parameter the verb declares is passed at the call site, `.none` where the
view means none. A verb that takes a place takes it as the reading gives it
back (`desktop.open(m, tab: .none, anchor: .some("40,50"))`).

A view of a slot opens nothing and places nothing: the session puts it where
the reading says the part stands, the overlay stack says when it holds the
pointer and the keys, and a press away from what it drew closes the slot, as a
press outside a menu does. A package that does not load, that declares no view
by the name the binding names, or whose view does not run is no view for the
slot: the session draws the view it ships with again rather than leaving the
slot blank.

The window menu is the slot with a tilolang host today
([`slots::surface`](../../crates/session/session/src/slots.rs)).

## What you cannot bind

The lock screen, a workspace's consent, a portal's request, a sign-in and a
display change being tried are drawn by the system whatever is bound
elsewhere: a view someone else wrote must not be able to ask for a password
and look like the machine asking. The settings resource refuses those slots.

More: [Build an app](../app-development.md), the
[window menu's own view](../../crates/session/window-menu), and the
[resource that binds a slot](../../crates/system/settings-sections/src/shell_views.rs).
