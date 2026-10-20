# Components: every Tilo component, composable from tilolang

**Goal.** Every component in Tilo's toolkit is directly usable from tilolang. App authors and
agents compose them freely into new components, and those compose further. The native look
(`native-apps.md`) is not compiler magic. It is a standard library written in tilolang on the same
components, which anyone can read, copy or replace.

This replaces the closed vocabulary in `native-apps.md` §3. Its goals (native chrome by default,
actions offered automatically, platform headers) stay, but the vocabulary that delivers them
changes.

## 1. Where we are

There are three closed vocabularies today:

| Where | What | Size |
|---|---|---|
| tilolang | `prims.rs` primitive table; host mapping in `apps/tilolang` | about 30, each wired by hand twice |
| tilolang "native" frames | `list`, `row`, `document`, automatic actions, platform headers | hard-coded in compiler and host |
| `tilo-ui-graph` | `UiNode`: Column, Row, Container, Text, Button, TextInput, Checkbox, Toggle, Spacer, plus `Component { name, props: JSON }` resolved by app-owned factories | 9 + an untyped escape |

None of them reaches the roughly 220 ui-core widgets, and every new widget means more hand
wiring. An author cannot build a component of the same kind as `list`, because `list`'s parts are
not exposed.

## 2. The model

### 2.1 A component is a described widget

Each ui-core widget gets a **component description**. It is declarative and generated from the
widget's Rust definition (a derive plus attributes), so it cannot drift from the widget:

| Part | Is | Example (`ListRow`) |
|---|---|---|
| **Props** | Typed values | `title: Text`, `secondary: Option<Text>`, `meta: Option<Text>`, `icon: Option<Icon>`, `selected: Bool` |
| **Slots** | Child content, typed by what may go in it | `trailing: View`, `avatar: View<Avatar>` |
| **Events** | Data, never closures. Each takes an action reference or a view-state update | `activate`, `select`, `swipe(action)` |
| **Bindings** | Two-way to a resource field, with the right merge (text splices, never rewrites) | `TextInput.value <-> Text`, `Checkbox.checked <-> Bool` |
| **Own state** | What the widget keeps between frames: scroll, selection, caret, expansion, the open pane | Becomes **lens** state automatically, so presence and resuming on another device work for every component |
| **Semantics** | Accessibility role, test id, the resource it presents | Carried through, never re-declared |

The description is data, versioned with ui-core. Types in it are the language's types: `Text`,
`Int`, `Option`, enums for roles and icons, `View` for slots, and `Action` for events.

### 2.2 tilolang reads descriptions like a package

The compiler already reads another package's metadata to type-check `use so.tilo.notes`. It reads
the toolkit's component descriptions the same way, as the package `ui`:

```
view contact_row(c: Contact) {
  ui.ListRow(title: c.name, secondary: c.email, avatar: { ui.Avatar.person(c.person) }) {
    trailing: { ui.Badge(c.unread) }
    on activate: open(c)
  }
}
```

- Arguments, slots and events are checked against the real widget API. Wrong types and missing
  required props are compile errors with the widget's own doc text.
- A widget added to ui-core is usable from tilolang as soon as it lands. No compiler or host change
  is needed.
- Formatter, completion and docs come from the description.

### 2.3 One generic host adapter

The host has **one** adapter: description → widget instance → patches. It does not have a mapping
per widget.

- A prop patch sets the prop and damages the widget.
- A slot patch inserts, removes or moves children with stable keys (`StructureChange::Described`).
- An event dispatches the action reference through the door, or applies the view-state update.
- A binding goes through the widget's existing model-binding path.

`tilo-ui-graph` becomes the serialized form of the same thing. A `UiNode` is a component instance
(`name`, typed props, slots). Its fixed variants and the untyped `Component` escape are replaced
by described components, so the graph format, `tilo-app dev` and tilolang share one model. Rust
apps can construct the same descriptions, so there is one component model, not two.

### 2.4 Composing new components

A tilolang `view` **is** a component. It has typed parameters (props), view parameters (slots),
action parameters (events), and its own `window` and `lens` state. Views from another package are
used exactly like `ui.*` ones. Authors therefore build components the same way the toolkit's own
widgets compose:

```
/// A tile on a dashboard: a titled card with a figure and a trend.
view stat_tile(title: Text, value: Number, trend: List<Number>, on_open: Action) {
  ui.Card(title: title) {
    body: { ui.Text(format(value), role: .title); ui.Sparkline(trend) }
    on activate: on_open
  }
}
```

There's no separate component system: it is views all the way down, with the leaves being
described toolkit widgets.

## 3. Native by default, as a library

The "native by default" behaviour moves into **`std`**, a package written in tilolang and shipped
with the runtime:

- `std.list(items) { x -> ... }` composes `ui.NavShell` and `ui.VirtualList` of `ui.ListRow`,
  with the header filled from the resource's actions, the automatic context menu and swipe,
  undo after destructive actions, and the empty state.
- `std.document`, `std.page`, `std.form`, sort and filter controls from enum window state, and
  presence on rows.

For `std` to be plain tilolang, the platform exposes what it needs as language features. Each one
is generally useful, not special to `std`:

1. **Action reflection.** `actions(r)` lists the actions available on `r` to *this viewer*,
   under authority, with label, icon, role and whether each is the primary one. This is how
   `std` builds headers and context menus, and any author can do the same.
2. **Lens access.** The viewer's lens and other people's lenses on a resource, for presence on
   any component.
3. **`open`, `close`, lanes:** already platform actions.
4. **Resource metadata:** title field, type name, icon.

An app that wants something `std` doesn't do (a reader, a canvas, a timeline) composes `ui.*`
and its own views directly. It loses nothing native, because the widgets are the native ones.
Authors who use `std` get the native look with no effort.

## 4. What stays out

- **Closures and arbitrary Rust callbacks.** Every event is an action reference or a view-state
  update. This keeps views pure, attributed, replayable and agent-visible.
- **Direct painting.** A later `canvas` component takes pure draw commands. Until then, custom
  visuals come from composing components.
- **Colours, sizes and fonts as raw values.** Props use the design system's roles and tokens, as
  ui-core apps already do.
- **Widgets that are internals,** such as layout helpers that exist only to implement other
  widgets. A description is opt-in per widget. Every widget the design system names as a
  primitive (design-language.md "Primitives", HIG content-widget table) is described.

## 5. What it costs

The work is in ui-core, not the compiler:

- **An audit of every public widget.** Many have Rust builder APIs with closures (`on_select`,
  `with_selection_delegate`, `on_context_menu`) and Rust-typed delegates. Each needs its events
  expressed as data and its delegates expressed as slots or row views. This is real redesign per
  widget, not a mechanical wrapper, and it improves the Rust API too: the same declarative events
  serve automation and accessibility (AGENTS.md: "a capability reachable only by clicking is not
  finished").
- **The derive** and the description format, versioned.
- **The generic host adapter.** It replaces both tilolang's per-primitive mapping and
  `tilo-ui-graph`'s builder.
- **`std` in tilolang,** replacing the hard-coded frames.

### Order

1. **Description format and derive,** on a first set: `Text`, `Button`, `TextInput`, `ListRow`,
   `VirtualList`, `NavShell`, `NavList`, `Section`, `EmptyState`, `Card`, `PropertyList`, `Avatar`.
   Then the generic adapter, and the compiler reading `ui`.
2. **Action reflection and lens access** in the language.
3. **`std` rewritten in tilolang;** the hard-coded frames removed; the six apps unchanged in
   behaviour, which is the regression check.
4. **The remaining primitives,** widget by widget, each with its closures turned into data.
5. **First app composed from `ui.*`: the ebook reader.** It is a library in `std`, plus a reader
   composed from the paginated text view, `FindBar` and a lens position. This also needs blob
   and file resources, declared format importers and content anchors (separate design).
