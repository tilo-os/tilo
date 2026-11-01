# Draw a part of the shell

Parts of the Tilo shell are **slots**: the window menu, the bar, the
switcher, the overview, quick settings, the launcher, notifications, the desk
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

## What you cannot bind

The lock screen, a workspace's consent, a portal's request, a sign-in and a
display change being tried are drawn by the system whatever is bound
elsewhere: a view someone else wrote must not be able to ask for a password
and look like the machine asking. The settings resource refuses those slots.

More: [Build an app](../app-development.md), the
[window menu's own view](../../crates/session/window-menu), and the
[resource that binds a slot](../../crates/system/settings-sections/src/shell_views.rs).
