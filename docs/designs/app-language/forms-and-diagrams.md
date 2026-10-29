# Forms, dialogs and diagrams

What views need beyond rows, pages and inputs. Both build on what exists: the one action system
(action-system.md), components described from ui-core (components.md) and the layout model
(layout.md). ui-core already has the native pieces: `ActionForm`, `ModalSheet`, `ModalDialog`,
`BottomSheet`, `Popover` and `PopoverPrompt`.

## 1. A dialog is an action waiting for its arguments

Nearly every modal in a real app asks for the rest of an action's arguments: share with whom,
move to which project, rename to what, snooze until when. So tilolang has no free-floating
`modal`. Running an action that still lacks arguments opens **its form**.

```
/// Shares the note with someone in the workspace.
action share(n: Note, with: Principal, can: Access = .read) (label: "Share…", icon: .share) {
    grant(n, to: with, can: can)
}
```

`share` in the note's menu, the command surface or a shortcut opens a form: a people picker for
`with`, and `can` as a choice starting at `.read`. The submit button is labelled "Share". Nothing
in the app describes that form.

- **Fields come from the one schema.** Each parameter's `FieldType` picks its control:

  | Type | Control |
  |---|---|
  | `Text` | text input; one line unless the field is a body |
  | `Bool` | toggle |
  | `enum` | segmented control up to four values, otherwise a menu |
  | `Date`, `Time` | date and time pickers that take natural text ("tomorrow 9") |
  | `Number` with a unit | number field with steps and the unit |
  | `Principal` | people picker over `workspace.members` |
  | a resource type | a picker over `find(T)`, with the palette's search |

  Labels come from parameter names and doc comments, through the app's i18n catalogue.
- **Defaults start filled in.** Parameters the caller already supplied aren't shown: the
  note's menu supplies `n`.
- **Validation is the action's.** Submit stays disabled while an argument doesn't parse for its
  type. A rejection from the action's node is shown at the field it names.
- **Typed but unsent values are lens state.** They're personal and follow the person across
  devices. Closing the form and opening it again keeps them. Nothing reaches the resource until
  submit, which is one invocation, with undo and history like any other.
- **Preview uses the same sheet.** When the invocation needs confirmation (destructive, bulk,
  reaching outside), the sheet shows the provisional run's changes and the confirm button.
  There is no separate "are you sure" dialog. Undoable changes don't ask; they offer undo.

### Each shell presents it its own way

The app declares the form; the shell decides how it appears:

| Shell | Presentation |
|---|---|
| Strips, windowed desktop, tiling | `ModalSheet` attached to the window. Only that window waits; the rest of the workspace doesn't. One or two short fields, invoked from a control, appear as a `PopoverPrompt` anchored to that control. |
| Canvas | Like the desktop, at the window. It's drawn at screen scale whatever the zoom. |
| Phone | `BottomSheet`. A full-screen sheet when there are more than about four fields. |
| Web | a `<dialog>` element, with the browser's focus and escape handling |
| Watch | One field at a time, or "continue on phone" for more than two |
| Agents, REPL | The same call. A missing argument is a teaching error that lists what the call needs. |

### A form the app draws itself

When the generated form isn't right, the action names a view for it:

```
action share(n: Note, with: Principal, can: Access = .read) (label: "Share…", form: share_form) { ... }

view share_form(f: Form<share>) {
    stack(gap: related) {
        ui.PeoplePicker(bind: f.with, suggest: members(f.n))
        row { choice(bind: f.can); spacer; ui.Text(access_hint(f.can), role: .caption) }
    }
}
```

- `Form<A>` is the action's arguments as bindable fields. It is a type derived from `A`, not a
  new kind of declaration.
- The view lays out only the fields. The shell keeps the title, the submit and cancel buttons
  and the presentation, so every shell can still show it.

### What isn't a dialog

- Showing something (an image at full size, a message's details) is opening it: a window, or
  a view inside one (workspace-model.md §4).
- Choosing among things in place is a popover menu (`ui.MenuButton`) or a picker control.
- Several steps with work between them are a flow, whose asks appear as forms (convergence.md).

## 2. Inputs

`std` and generated forms need these ui-core widgets described as components (components.md
§1), in this order: number field, segmented control, a searchable select, `ui.PeoplePicker`, a
resource picker, slider, colour (theme roles only), and file. Each lands with its closures turned
into data events, the same as `TextInput`'s `submit`. Buttons get their role (`.primary`,
`.destructive`, `.quiet`) from the action they run, not from the view.

## 3. Diagrams

components.md §4 has the drawing ladder; step 2, drawing with the design system, stays the
escape hatch. Most diagrams aren't drawn by hand, though. Like lists, they are ui-core
components: layout, routing, axes, hit testing, motion and accessibility are written once in
Rust, and apps describe data.

| Component | Draws | Layout |
|---|---|---|
| `ui.Chart` | line, area, bar, scatter; several series | axes, ticks, units and locale formatting picked from the data |
| `ui.Graph` | nodes and edges | `.layered` (dependencies, flows), `.tree`, `.force`, or `.free` (positions stored on the resource) |
| `ui.Timeline` | items with spans in lanes | time axis that zooms from hours to years |
| `ui.Sparkline` | one small series inside text or a row | fills its slot |

### Data in, actions out

Items come in the way lists take them: a `for` loop inside the component, which follows the
collection's inserts and removals. A node's look is an ordinary view. Gestures are events that
run actions, so a drag or a connection is an attributed, undoable change that permissions,
presence and agents all understand.

```
view dependencies(p: Project, tasks: Collection<Task>) {
    ui.Graph(layout: .layered) {
        on activate(t: Task): open(t)
        on connect(a: Task, b: Task): block(a, by: b)
        on disconnect(a: Task, b: Task): unblock(a, by: b)
        for t in tasks.filter(t -> t.project == p) {
            ui.GraphNode(item: t) { task_card(t) }
            for b in t.blocked_by { ui.GraphEdge(from: b, to: t) }
        }
    }
}

view done_per_week(p: Project, weeks: Collection<WeekCount>) {
    ui.Chart(kind: .bar, x: .week, y: .count) {
        for w in weeks { ui.ChartPoint(item: w, x: w.week, y: w.done) }
    }
}
```

- **Free layout** stores positions on the resource. A `Point` field gets
  `ui.GraphNode(item: n, at: n.at)` plus `on moved(n: Card, to: Point): n.at = to`. While a node
  is dragged, the UI host moves it locally at native refresh. The release commits one edit.
- **Selection, hover and focus** are lens state, so others' selections show as presence. A
  hover shows the item's title and its type's filter fields by default.
- **Accessibility:** each node is focusable with its view's semantics, and arrow keys follow
  edges. A chart exposes its data as a table to screen readers and to agents (`read` and
  `describe` see series, not pixels).
- **Agents:** a chart is a view of data. An agent's answer can embed one (agent-ux.md), and the
  door checks it like any other view.

### Good looking by default

- Colours come from the theme's categorical and sequential roles. An app can't set a colour; it
  can emphasise a series (`emphasis: .primary`).
- Labels avoid collisions automatically: ticks thin out, edge labels move, and long names
  truncate with the full name on hover.
- Changes animate on the platform's springs, keyed by item id, so a new point slides in rather
  than redrawing the chart.
- Sizes follow layout.md: a chart hugs its slot with an aspect (`aspect: 16/9` by default) and
  switches to a compact form (fewer ticks, no legend) below `space.compact`.

### Cost

Each component draws only what changed and only what's visible. A graph past a few hundred
nodes clusters out of view, and a chart with more points than pixels draws the min and max per
pixel column. The perf matrix (components.md §5) gains 10k-point chart and 1k-node graph cases,
gated like lists.

## 4. Order

1. Action forms: generated forms, the presentation per shell, then `form:` views. Mail's move
   to folder and Tasks' assign and snooze move to them.
2. The input widgets in §2.
3. `ui.Chart`, first used by Tasks' project page (done per week).
4. `ui.Graph`, first used by Tasks' dependencies (`blocked_by`).
5. `ui.Timeline`, first used by Tasks' project schedule.
6. `tilolang layout-check` (layout.md §6) gates all of these from its first landing.
