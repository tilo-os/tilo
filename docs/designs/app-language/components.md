# Components

There's one set of UI components for every app and the shell (AGENTS.md, principle 5).
tilolang uses exactly those, and anything built from them composes further. The native look
isn't compiler magic: it's `std`, a library written in tilolang on the same components.

## 1. A component is a described widget

Each ui-core widget has a description generated from its Rust definition
(`#[derive(Component)]`, catalogue in `crates/dev/ui-graph/components.json`), so it can't drift:

| Part | Is | Example (`ListRow`) |
|---|---|---|
| Props | typed values | `title: Text`, `secondary: Option<Text>`, `selected: Bool` |
| Slots | typed child content | `trailing: View`, `avatar: View<Avatar>` |
| Events | data, never closures: an action reference or a view-state update | `activate`, `swipe(action)` |
| Bindings | two-way to a field, with the right merge (text is spliced, never replaced) | `TextInput.value <-> Text` |
| Own state | scroll, selection, caret, expansion | becomes lens state, so presence and resuming work for every component |
| Semantics | accessibility role, test id, the resource shown | carried through |

The compiler reads the catalogue as the package `ui`:

```
view contact_row(c: Contact) {
    ui.ListRow(title: c.name, secondary: c.email, avatar: { ui.Avatar.person(c.person) }) {
        trailing: { ui.Badge(c.unread) }
        on activate: open(c)
    }
}
```

- Props, slots and events are checked against the real widget, with its doc text in errors.
- Props are the language's values: `List<T>` and `Set<T>` take a list or a set of what they
  hold; `Map<Text, V>` takes a map field read whole (`ui.Reactions(reactions: m.reactions)`);
  `List<Action>` takes `actions(r)`, each action shown by the component and run by an event of
  its node (`actions.0`, ...). The wire can splice a list prop in place (`SpliceProp`). A
  collection is not a component's prop yet: it goes to a view's `Collection<T>` parameter, live,
  until the host follows a collection prop item by item.
- A new ui-core widget is usable from tilolang as soon as it lands.
- One generic adapter turns descriptions into widgets and patches. There's no per-widget
  mapping, and Rust apps can use the same descriptions.
- Bindings are the description's, so `bind:` works on any component that declares one
  (`ui.TextInput(bind: t.title)`, `ui.Checkbox(bind: t.done)`, `ui.DatePicker(bind: t.due)`,
  `ui.RichInput(bind: t.notes)`), with the type and the merge it declares; no widget has binding
  code of its own. A primitive that is a component (`text`, `input`, `checkbox`, ...) is its
  shorter name and nothing else.

Some components *attach*: `ui.ContextMenu`, `ui.HoverToolbar` and `ui.Swipe` draw nothing where
a view puts them and give the view they stand in a menu, a toolbar shown on hover and focus, or
a swipe. A list item holds those of its row wherever the row's layout puts them — through its
stacks, rows, grids and calls — while what a component of its own meaning holds (a card, a
section) stays that component's. Its menu ends with each of the item's actions the row's menu
did not place, so every action stays reachable. They all run in the UI host.

## 2. Views are components

A tilolang `view` is a component: typed parameters are props (a `Collection<T>`, `Set<T>` or
`Map<V>` parameter is handed the field live, never copied, and a loop over it follows its
inserts and removals), view parameters are slots, action
parameters are events, and it has its own `window` and `lens` state. Other packages' views are
used like `ui.*` ones.

```
/// A dashboard tile: a titled card with a figure and a trend.
view stat_tile(title: Text, value: Number, trend: List<Number>, on_open: Action) {
    ui.Card(title: title) {
        body: { ui.Text(format(value), role: .title); ui.Sparkline(trend) }
        on activate: on_open
    }
}
```

## 3. Native by default, as a library

`std` builds the standard shapes from `ui.*`: lists with headers filled from the resource's
actions, context menus and swipes, undo after destructive actions, empty states, documents,
pages, forms. It relies on general language features any author can use:

- `actions(r)`: the actions on `r` this viewer may use, with label, icon and role;
- the viewer's and others' lenses, for presence;
- `open`, `close` and strips;
- resource metadata: title field, type name, icon.

An app that needs something `std` doesn't do composes `ui.*` directly and loses nothing native.

A list groups without a second widget: `std.rows` takes `group:`, naming each item's heading,
and the list draws a section band for each run and holds it at the top while its items scroll
past (dates arrive formatted: Today, Tomorrow, a weekday, a day). Only the rows in view are
built and asked for their group, so a heading costs nothing on a long list. `closed:` names the
items done with: they stand last under `closed_title`, folded away until the person presses the
band. The heading is a date the platform writes, never a string an app formats:

```
std.rows(shown, "No tasks here", .none, .some("list-checks"),
         group: .due, closed: .done) { t -> task_row(t) }
```

The same bands serve mail and chat grouped by day. `ui.VirtualList` and `ui.ListItem` carry it
(`group:` and `tail:` on a row, `tail_title:` on the list), so a view composed from `ui.*`
groups as `std.rows` does.

## 4. Custom widgets: a ladder

Most needs stop at step 1 or 2. Every step stays native under any theme, and hot paths stay in
Rust so Tilo runs well on slow hardware.

1. **Compose `ui.*`.**
2. **Draw with the design system:** shapes, paths, arcs and gradients, with colours only by
   theme role, sizes from tokens, text by role, motion from the platform's curves. Retained and
   damage-limited. A semantic role is required (`role: .progress(0.6)`), so screen readers and
   agents understand it. Theming belongs to the person; an app's identity comes from shape,
   layout, icons and motion, never its own colours.
3. **A render graph:** declared shader and compositing nodes over textures, run by the platform.
   For video previews and image layers.
4. **A WebGPU-shaped API** for 3D and heavy work: validated WGSL, rendering into a texture slot
   inside the app's window. Never raw Vulkan, never outside the slot.

Steps 3 and 4 produce a texture placed in the normal widget tree, with layout, focus and
accessibility.

### 4.1 Drawing with the design system (step 2)

One described component, `ui.Drawing`, draws a list of shapes. The shapes are values, so a
drawing is a pure function of what the view reads, it is retained like any other node, and the
compiler checks it like any other prop. There is no paint callback and no new syntax.

```
/// How full a disk is, as a ring.
view usage_ring(used: Float) {
    ui.Drawing(role: .progress(used), size: 4rem) {
        shapes: [
            .arc(center: (0.5, 0.5), radius: 0.42, from: 0.0, to: 1.0,
                 stroke: .role(.track), width: .regular),
            .arc(center: (0.5, 0.5), radius: 0.42, from: 0.0, to: used,
                 stroke: .role(.accent), width: .regular, cap: .round),
        ]
        on press(shape): open_details()
    }
}
```

- **Shapes:** `.rect` (with a corner role), `.ellipse`, `.arc`, `.line`, `.path(points:,
  curve: .straight | .smooth, closed:)` and `.text(role:)`. Coordinates are fractions of the
  drawing's box, so a drawing scales with its layout and never names pixels.
- **Looks come from the theme only.** A fill or stroke is `.role(...)` (accent, track, positive,
  danger, ink, and the rest of the theme's roles), optionally with `.wash` (a vertical gradient
  of the role into transparency). A stroke width is a token (`.hairline`, `.regular`,
  `.strong`), text is a text role, and a corner is a radius role. There are no raw colours,
  sizes or fonts, so a drawing reads right in every theme.
- **A semantic role is required:** `.progress(f)`, `.chart(summary)`, `.picture(label)`,
  `.decoration` for purely visual ones. Screen readers and agents read the role, not the shapes.
- **Motion:** when a shape's numbers change between two readings, the widget moves them with the
  platform's motion curve (`motion` theme role), in Rust, at native refresh. A view never
  animates.
- **Retained and damage-limited:** the widget builds its scene from the shapes once per
  change of its props, caches it like any part, and damages only its own box. An idle drawing
  costs nothing.
- **Events are data:** `press(shape)` gives the index of the topmost shape under the
  pointer, and `hover(shape)` likewise, so a chart's bar or a map's region is pressable without
  a closure.

Underneath, `ui.Drawing` is a ui-core widget (`#[derive(Component)]`) that lowers shapes to the
same `kurbo` paths, fills, strokes and gradients the toolkit's own `Chart` and `Sparkline`
paint with, resolving each role against the current theme. The component description gains
the one thing it lacks for this: a closed set of cases *with fields* (`Type::Cases`, as a
tilolang `enum` with payloads), which the compiler already understands from `enum`.

## 5. Efficiency is enforced

- Counts, filters and queries come from the index, never a loop in a view. The compiler warns
  when a view iterates a whole collection outside a virtual list.
- Every app gets a performance matrix from its scenarios (open, scroll, keystroke, peer edit,
  filter at 1k, 10k and 100k items), gated by counted work: boundaries rendered, items built,
  pages read, repainted area.
- Widgets report exactly what changed and damage only their own area; the test harness handles
  changes exactly as the live host does.

## 6. What stays out

Closures and Rust callbacks (events are data), raw painting or GPU access outside the ladder,
raw colours, sizes and fonts, and widgets that are only internals of other widgets.

## 7. Order

1. Description format, derive, generic adapter, the compiler reading `ui` (done for the first
   set).
2. Action reflection and lens access in the language.
3. `std` in tilolang instead of hard-coded frames.
4. The remaining widgets one by one, each with its closures turned into data events. This is
   real redesign per widget, and it improves the Rust API too.
5. The first app composed straight from `ui.*`: an ebook reader.
