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
| Own state | scroll, selection, caret, expansion | becomes view state, so presence and resuming work for every component |
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
- A view's fit follows it into a slot: `fill`, `min:`, `max:` and `aspect:` written on the
  call hold in a component's body slot as in a `stack`'s, so `ui.Card { body: { mail.inbox(m,
  fill) } }` is the card's height, not one line (`layout.md` §2).
- Props are the language's values: `List<T>` and `Set<T>` take a list or a set of what they
  hold; `Map<Text, V>` takes a map field read whole (`ui.Reactions(reactions: m.reactions)`).
  An action list is the host's, filled from the type's declarations, and a view never reads one
  back (`actions(r)` is an error there, E047). The wire can splice a list prop in place
  (`SpliceProp`). A collection is not a component's prop yet: it goes to a view's
  `Collection<T>` parameter, live, until the host follows a collection prop item by item.
- A new ui-core widget is usable from tilolang as soon as it lands.
- One generic adapter turns descriptions into widgets and patches. There's no per-widget
  mapping, and Rust apps can use the same descriptions.
- Bindings are the description's, so `bind:` works on any component that declares one
  (`ui.TextInput(bind: t.title)`, `ui.Checkbox(bind: t.done)`, `ui.DatePicker(bind: t.due)`,
  `ui.RichInput(bind: t.notes)`), with the type and the merge it declares; no widget has binding
  code of its own. A component may declare several bindings, and `bind:` takes the one whose
  type the bound place has. A binding a description declares to take a case (`ui.EnumField`'s
  `value`) takes a field of any enum the app declares, whose cases are what the field offers;
  the app names the cases it offers in the description's slot, and a pick is the case's name.
  A primitive that is a component (`text`, `input`, `checkbox`, ...)
  is its shorter name and nothing else.
- A box bound to a value writes that field when it is pressed, unless the view names what a
  press means: `ui.Checkbox(bind: t.done) { on check: check(t) }` and
  `row(t.title, checkbox: t.done, check: check(t))` show the field and run the app's action,
  so completing a task completes what is under it in the same step, one undo, and history
  records the flag the app's action set.
- A document's running text is placed like any other component:
  `ui.BlockEditor(bind: doc.body, placeholder: "Write")`. It declares a binding to the field as
  a `RichText` (`rich`, its paragraphs) and to a Markdown `Text` field (`markdown`, the same
  text round-tripped), and edits the one the view bound; a change from elsewhere shows when the
  person is not typing there. The view places the editor; no marker on a field says which
  control draws it.

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
parameters are events, and it declares its own view `state`. Other packages' views are
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

- the host's surfaces on what the type declares: the window's bar and its one menu, and an
  item's context menu and swipe, each action with its label, icon and role;
- the viewer's and others' lenses, for presence;
- `open`, `close` and strips;
- resource metadata: title field, type name, icon.

An app that needs something `std` doesn't do composes `ui.*` directly and loses nothing native.

A window that is only a list is `std.browse`: it takes the resource its window is of, the items,
the places and the sort, and writes that window the standard way — the place a person chose as its
state, the list that place finds, the title and count naming that place, the empty state, the entry
that adds an item, and the pane beside the list when the rows open there. It draws what the caller
named and never reads a shell off the type; `opens` is the one fact it takes from the type, whether
its rows have a window of their own, and a window whose rows open in front of it states
`opens: false`. An app whose window is more than this — a sidebar of its own, rows that group or
nest — composes `std.list`, `std.places` and `std.header` itself.

A list groups without a second widget: the row a list draws takes `group:`, naming the item's
key, and the list draws a section band for each run of items that names the same key and holds
it at the top while its items scroll past. The heading is the key itself, presented as a value of its kind is
(a day as Today, Tomorrow, a weekday; a word as it is written), never a string an app formats.
Items that name no key stand under `ungrouped:`, in the order the caller gave them; a place that
groups nothing says so per item (`group: x -> if by_day { x.due } else { .none }`), from what the
place says and never a walk over the list. Only the rows in view are built and asked for their
group, so a heading costs nothing on a long list. `tail:` names the items done with: they stand
last under `tail_title`, folded away until the person presses the band, and a list that folds
nothing away answers `tail: x -> false`, giving no heading and no clause. The block names the
item and who else is at it; the row names the item it stands under (`parent:`) and the one just
over it (`above:`), so a child leaves out what its parent already says:

```
std.list(shown, tree: true, ungrouped: .some("No day")) { (t, at) ->
    ui.ListItem(item: t, presence: at, group: t.due, tail: t.folded, parent: t.parent) {
        task_row(t, t.parent)
    }
}
```

A key the platform has no words for — a case of an enum, a record reference — is grouped by the
text the caller reads off it (`group: x -> .some(x.status.label())`): the heading is the text the
platform draws, so the runs are the runs of that text.

The same bands serve mail and chat grouped by day. `ui.VirtualList` and `ui.ListItem` carry it
(`group:` and `tail:` on a row, `tail_title:` on the list), so a view composed from `ui.*`
groups as `std.list` does: the app names its own grouping, nesting and fold, and `std` adds no
parameters of its own for them.

A row that carries an item's own document says so with `detail:`, and opens where it stands: the
press grows the row into the document under it, the list keeps the scroll and lays out around it,
and the row stays a row of the list it is in. One row stands open at a time, Escape and a press
outside close it, and a row that leaves the list — a place switched, a filter, a sync — closes
with it, so the open item is always one of the items on screen. A window re-renders its list as
its place, its filter or its rows change, and the item stays open across that: `open:` carries
the item standing open, `opened:` reports the item the person opened, `closed:` that it closed
(pressing the open row again, Escape, a press beside it, or its item leaving the list), and the
view keeps it as its own state — `std.list` does that for its person.

A list that opens its rows elsewhere instead — in a pane beside it, a window of its own — leaves
`detail:` out and answers its `on activate`. Which rows carry one is the list's: `std.list` sets
it where the view gives one, and `std.browse` puts the pane beside the list when the caller says
its rows have a window of their own.

## 4. Customizing a component: a ladder

How deeply a view may make its UI its own, from saying nothing to shipping Rust. Each rung is a
superset of the one before it: a view that composes `ui.*` directly, or draws a shape itself,
keeps everything the rungs above give it — native behaviour, accessibility, theming, motion and
reduced motion. There is no second rendering path to fall behind. Most needs stop at rung 1 or 2,
and hot paths stay in Rust so Tilo runs well on slow hardware.

| Rung | The view says | Code it writes |
|---|---|---|
| 0 | nothing | none |
| 1 | options on a component | one argument |
| 2 | how the parts are laid out | its own `view` |
| 3 | how a change plays | a `motion:` argument |
| 4 | a shape of its own | specified, not in the language yet |
| 5 | a widget of its own | Rust, shipped with the package |

### 0. Defaults: nothing said, everything right

`std` and `ui.*` already look, behave and move the way the platform does. An app that writes its
list the obvious way gets motion with no code at all:

```
row view task_row for Task

view task_row(t: Task) {
    row(t.title, checkbox: t.done, secondary: t.project?.name, meta: t.due)
}

default view home for Tasks

view home(tasks: Tasks) {
    ui.NavShell(content_kind: .document) {
        overview: {
            std.list(tasks.items) { (t, at) ->
                ui.ListItem(item: t, presence: at) {
                    task_row(t)
                }
            }
        }
    }
}
```

What that already does, with no option set:

- **Lists move.** A row a person adds by hand arrives at its place and the rows below it make
  room; a row they remove by hand fades where it stood and those below close the gap; a row they
  move glides to where it lands. A change a key or an action makes (Enter, a menu item, an
  assistive technology) lands on the next frame instead, because the eye is already where it
  went: `motion::MotionSource::lands_at_once`. A list the host re-reads whole — its order
  replaced, not edited — lands as a page's worth of rows rather than one change to follow.
- **Marks fill.** A checkbox's mark comes in and goes out over the theme's base motion, and the
  box the next frame builds goes on from where the last one was (`motion::HandOn`), so a host
  that rebuilds rows on every change still shows one continuous mark.
- **Things open and close.** A disclosure row and a tree branch unfold their content, a
  collapsible section's content arrives and leaves, a badge's count cross-fades, a menu, sheet
  and window take the theme's named motion for opening and closing.
- **The person is obeyed.** A theme that holds still, and reduced motion, land every one of these
  on the frame it happens. No component, and no option below, can animate for someone who asked
  it not to.

All of it is ui-core's motion system: `motion::HandOn` for a widget a host rebuilds,
`motion::CheckMotion` for marks, `motion::RowMotions` and `collection_transition` for lists,
`Disclosure` and `Appear` for reveals and arrivals, `MotionSpec` for what a named motion becomes
at a given moment and source. tilolang has no animation machinery of its own, and `std` writes no
motion code: a rung is a way to describe a component, never a second engine.

### 1. Options: the component's own props

The first thing a view can say is an option on the component it already uses: emphasis, density,
which motion, how many lines. Options are typed, checked against the real widget, and bound to
the theme and its tokens — never to a colour, a duration or a curve.

```
ui.ListRow(title: t.title, checkbox: t.done, secondary: t.project?.name, motion: .brisk)
```

`motion:` takes `.standard` (the theme's pace, and what a component does anyway), `.brisk`
(0.7× the theme's durations: dense, repetitive work where the eye is already where the change
will be), `.calm` (1.5×: a surface a person reads or watches) and `.still` (every change on the
next frame). It changes *how long* the design system's named motions take, never what they do:
the curve stays the theme's spring, reduced motion and a still theme pass through untouched.

The components that own motion take it: `ui.VirtualList`, `ui.ListRow`, `ui.Checkbox`,
`ui.Badge`, `ui.Section`, `ui.DisclosureRow`, `ui.TreeView`. A list's own pace is the pace of the
rows it builds (rung 1 example above), so one option covers a whole list; a row says its own when
it needs to differ from the list's. `std`'s shapes take the platform's pace — a view that wants
its own either names the row it builds or composes the list itself (rung 2).

### 2. Compose `ui.*` into views

When an option is not enough, the view says how the parts are laid out, still from components:

```
view task_list(tasks: Tasks) {
    ui.VirtualList(selection_follows_focus: true, in_column: true, motion: .brisk) {
        on activate(x: Task): open(x)
        for x in tasks.items {
            ui.ListItem(item: x) {
                ui.ListRow(title: x.title, checkbox: x.done, secondary: x.project?.name) {
                    trailing: { ui.Badge(x.project?.name ?? "Inbox", tone: .neutral) }
                }
            }
        }
    }
}
```

Nothing is lost by leaving `std`: the list still virtualizes, rows still carry their menus,
swipes and presence, selection and scrolling stay the list's, and everything from rung 0 keeps
moving. `std` is a library of such views; a package's own views are the same kind of thing and
compose with them.

### 3. Motion and state: how a change plays

Rung 1's `motion:` is also the smallest way to say how a *state* change plays, because the
component that owns the change is the one that knows what it means:

- **check-off** — `ui.Checkbox`, `ui.ListRow`'s leading box;
- **reveal** — `ui.Section(collapsible: true)`, `ui.DisclosureRow`, a `ui.TreeView` branch;
- **reorder** — `ui.VirtualList`, including a move the view state drives (`items.move(x, before: y)`);
- **emphasis** — `ui.Badge`'s changed count.

So the whole rung costs an argument:

```
view inbox(tasks: Tasks) {
    state open: Option<Task> = .none
    page {
        ui.Section("Inbox", collapsible: true, motion: .calm) {
            for x in tasks.items {
                ui.ListRow(title: x.title, checkbox: x.done, selected: open == .some(x), motion: .brisk) {
                    on activate: set(open, .some(x))
                }
            }
        }
    }
}
```

What it does *not* let a view say, and this is deliberate: no duration, no cubic-bezier, no
per-property easing, no keyframes. An app's motion comes from which named motion plays and at
which pace, so every app accelerates, settles and respects reduced motion the same way, and a
theme change reaches all of them. Two things are specified and not in the language yet, and both
would be a `motion:` on the view rather than a new syntax:

- a transition on a value a component does not own (a figure that changes when a filter does):
  today that value lands on the next frame, and the smallest form it will take is the same
  `motion:` on the `ui.Text` that shows it;
- a view that wants to say what moved as a whole, rather than which component animated.

### 4. Draw with the design system

A view may draw shapes of its own: paths, arcs, gradients, strokes. Specified, not in the
language yet, and it will be one described component, so it stays on the same paint path, in the
same widget tree, with the same damage accounting.

`ui.Drawing` draws a list of shapes held as values. The shapes are values, so a
drawing is a pure function of what the view reads, it is retained like any other node, and the
compiler checks it like any other prop. There is no paint callback and no new syntax.

```
/// How full a disk is, as a ring.
view usage_ring(used: Float) {
    ui.Drawing([
        .arc(cx: 0.5, cy: 0.5, radius: 0.42, from: 0.0, to: 1.0, stroke: .track),
        .arc(cx: 0.5, cy: 0.5, radius: 0.42, from: 0.0, to: used, stroke: .accent, weight: .strong),
    ], meaning: .progress, value: .some(used), label: .some("Disk"), height: .some(64.0)) {
        on press: open_details()
    }
}
```

- **Shapes** are the toolkit's enum `ui.Shape`: `.rect(x, y, w, h, round)`, `.ellipse(cx, cy,
  rx, ry)`, `.arc(cx, cy, radius, from, to)` (fractions of a turn from the top), `.line(x1, y1,
  x2, y2)` and `.path(points, smooth, closed, wash)`. Coordinates are fractions of the
  drawing's box, so a drawing scales with its layout and never names pixels. A field with a
  default (`round`, `fill`, `weight`, the path's flags) may be left out, as a record's may.
  Text in a drawing is not built yet.
- **Looks come from the theme only.** A fill or stroke is an ink, `ui.Ink`: a role of the
  theme (`.accent`, `.ink`, `.muted`, `.track`, `.success`, `.warning`, `.danger`, `.info`).
  `wash` fades a path's fill out downward. A stroke's `weight` is a token (`.hairline`,
  `.regular`, `.strong`) multiplied from the theme's border width, and strokes end round.
  There are no raw colours, sizes or fonts, so a drawing reads right in every theme.
- **What it means is required:** `meaning: .progress | .chart | .picture | .decoration`, with
  `label` and, for progress, `value`. Screen readers and agents read that, not the shapes; a
  decoration is not in the accessibility tree.
- **Motion** (not built yet): when a shape's numbers change between two readings, the widget
  is to move them with the platform's motion curve, in Rust, at native refresh. A view never
  animates.
- **Retained and idle-free:** the shapes are props of a retained node, so a change of them is a
  patch of that node, and the widget paints them when its frame is drawn, like any widget. It
  asks for no frame of its own, so an idle drawing costs nothing.
- **Events are data:** `press` carries the index of the topmost shape under the pointer, so a
  chart's bar or a map's region is pressable without a closure. `hover` is not built yet.

Underneath, `ui.Drawing` is a ui-core widget (`#[derive(Component)]`) that lowers shapes to the
same `kurbo` paths, fills, strokes and gradients the toolkit's own `Chart` and `Sparkline`
paint with, resolving each role against the current theme. The component description gains
the one thing it lacks for this: a closed set of cases *with fields* (`Type::Cases`, as a
tilolang `enum` with payloads), which the compiler already understands from `enum`.

Until it lands, a view that needs a picture uses the drawn components that exist
(`ui.Sparkline`, `ui.ProgressRing`, `ui.ProgressBar`, `ui.Icon`), and a view that needs more is at
rung 5.

### 5. Native: a widget of its own

The last rung is Rust: a ui-core widget shipped with the package, or the platform's heavy
surfaces.

- **A widget written in Rust** is a component like any other once it is described (`#[derive(Component)]`,
  catalogue in `crates/dev/ui-graph/components.json`): it appears in `ui` for every tilolang
  package as soon as it lands, with its props, slots, events and defaults, and needs no adapter.
  That is also how `std` and the shell are built.
- **A render graph** — declared shader and compositing nodes over textures, run by the platform
  — is for video previews and image layers.
- **A WebGPU-shaped API** — validated WGSL, rendering into a texture slot inside the app's
  window — is for 3D and heavy work: never raw Vulkan, never outside the slot.

The last two produce a texture placed in the normal widget tree, with layout, focus and
accessibility.

### Climbing down

Dropping a rung never loses what the rungs above gave: a view that composes `ui.*` keeps
virtualization, selection, presence and motion; a view that draws a shape keeps theming,
reduced motion and accessibility through its role; a native widget is a component like any
other. The ladder is about how much a view says, never about a faster but poorer path.

Motion costs no frame. A mark, a row's arrival and a reveal are drawn as part of the frame that
shows them, so a frame while something moves costs what a still frame costs; `apps/tilolang`'s
motion suite measures it, and the same suite pins the behaviour of every rung above (a row's
mark filling over frames, a row arriving, rows leaving, a still theme and reduced motion landing
at once).

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
raw colours, sizes, fonts, durations, curves and easing, and widgets that are only internals of
other widgets. A view picks a named motion and its pace; the platform owns what they are.

## 7. Order

1. Description format, derive, generic adapter, the compiler reading `ui` (done for the first
   set).
2. Action reflection and view state access in the language.
3. `std` in tilolang instead of hard-coded frames.
4. The remaining widgets one by one, each with its closures turned into data events. This is
   real redesign per widget, and it improves the Rust API too.
5. The first app composed straight from `ui.*`: an ebook reader.
