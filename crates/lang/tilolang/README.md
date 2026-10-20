# tilolang

The compiler of tilolang, the Tilo app language, and its command line. The language is
specified in `docs/designs/app-language/` (spec, implementation, layout); the choices this crate
made where those are silent are in [DECISIONS.md](DECISIONS.md).

## A package

A package is a directory. Every `.tl` file directly in it is part of one module, and exactly
one of the files starts with the app header:

```
apps/tl/counter/
  app.tl        app so.tilo.counter "Counter"   (the header, then any declarations)
  views.tl      more declarations; no header
  app.ids       the ids lock: generated, committed
```

How the declarations are spread over the files does not matter: a file may use what another
declares, in any order. Diagnostics name the file and the line within it.

The ids lock gives every declaration its stable id. The compiler adds new declarations to it and
retires removed ones; commit it with the sources. Rename a declaration with
`tilolang ids --rename old new`, so it keeps its id and with it the data stored under it.

To start a package, make a directory next to the others with one file:

```
app my.counter "My counter"

type Counter {
    value: Int merge counter
}

root Counter

default view counter for Counter

action increment(c: Counter) {
    c.value += 1
}

view counter(c: Counter) {
    page {
        heading("{c.value}")
        button("+1", on: increment(c), style: .primary) as plus
    }
}
```

`root Counter` makes `Counter` a root type: one resource of it per room, which the launcher
opens through the type's default view (`default view counter for Counter`).

`use other.app as other` imports another package. Imports are found among the package's sibling
directories (for `apps/tl/board`, the other directories of `apps/tl`); `--path <dir>` adds a
package directory, or a directory of packages, to look in.

## Syntax reference

The grammar is implementation.md section 2 with the changes in [DECISIONS.md](DECISIONS.md)
("Syntax revision"). In short:

**Declarations.** A package is `type`s, `enum`s, `action`s, `flow`s, `view`s and `fn`s, plus two
package-level declarations (workspace-model.md sections 2 and 7):

```
type Note { title: Text, pinned: Bool = false, tags: Set<Text> }

enum NoteOrder {
    created
    title
}

enum Shape { dot, line(len: Int), box(w: Float, label: Text) }

root Notebook                          // one per room; the launcher opens it
default view note_editor for Note     // what a window of a Note shows
```

**Enums.** An `enum` is a sum type; a variant may carry named payload fields. A value is a
dot literal resolved from the type the context expects, or written with its type where none
is expected:

```
let s: Shape = .line(len: 3)
let d = Shape.dot
p.shape = .box(w: 1.5, label: "big")   // a field of an enum type is a register
```

`Option<T>` is the built-in enum `.none | .some(value)` and `Result<T, E>` the built-in
`.ok(value) | .err(error)`. A `T` is accepted where an `Option<T>` is expected. Enums, options
and results compare with `==` by value (`picked == .some(n)`); they are taken apart only with
`match`, which is an expression and must name every variant (`_` skips a payload field):

```
fn area(s: Shape) -> Float {
    match s {
        .dot => 0.0,
        .line(n) => n.to_float(),
        .box(w, _) => {
            let half = w / 2.0
            half * half
        }
    }
}
```

**`if` and blocks.** There is one `if`: `if cond { ... } else { ... }`. Used as a value its branches
agree on a type and the value is each block's last expression; as a statement it needs no
`else`. Any block is an expression (`let n = { let a = 2; a * 2 }`), and a body that ends with an
expression returns it:

```
fn title_of(list: TodoList) -> Text {
    if list.name == "" { "Todo" } else { list.name }
}
```

**Flows** invoke actions by calling them. `try act(args)` gives a `Result<T, Text>` (`Result<(),
Text>` for an action without a result) instead of failing:

```
flow cleanup(list: todo.TodoList) -> Text {
    for item in list.items {
        if item.done {
            todo.remove(list, item)
        }
    }
    match try todo.rename(list, "Done") {
        .ok => "renamed",
        .err(why) => why,
    }
}
```

**Views.** A view body is UI statements: primitive and view calls (with child blocks), `for`,
`if`/`else if`/`else`, `match` (each arm one UI call or a block) and `let`. Options of UI
primitives are the primitives' own enums, written as dot literals; lengths keep their units and
flags stay bare words. An anchor follows the call as `as name`:

```
view notebook(nb: Notebook) {
    window order: NoteOrder = .created     // shared by everyone looking at this window
    lens expanded: Option<Note> = .none    // one per person per window
    page {
        stack(gap: .section) {
            row(distribute: .between) {
                heading("Notes")
                button("New note", on: new_note(nb), style: .primary) as new
            }
            button("Sort by title", on: set(order, .title))
            for n in listed(nb, order) {
                button(n.title, on: open(n), style: .plain)
                text(n.body, style: .caption, truncate, min: 11rem)
            }
        }
    }
}
```

The options and their values: `gap:`/`padding:` `.tight .related .sibling .section .group`;
`align:` `.start .center .end .stretch`; `distribute:` `.start .center .end .between`;
`collapse:` `.stack .drawer .navigate`; `side:` `.leading .trailing`; `at:` the nine positions
`.top_leading` ... `.bottom_trailing`; `axis:` `.vertical .horizontal`; button `style:`
`.secondary .primary .destructive .plain`; text `style:` `.body .title .heading .caption .label
.code`. `space.width_class` (`.compact .medium .expanded .wide`) and `space.orientation`
(`.portrait .landscape .ultrawide`) compare with the same literals.

**View state** (workspace-model.md sections 1, 4 and 7) is declared at the top of a view:
`window name: T = default` is the window's and shared, `lens name: T = default` is each
person's own in that window. The view reads them like variables; they change only through a
control bound to them (`input(bind: query)`, `checkbox("Open", bind: open)`) or the platform
action `set(name, value)` on a control. `open(r)` and `open(r, view: v)` are platform actions
too: they go to the window of `r`.

## Commands

`tilolang` is the full build tool: the host app's binary (`cargo run -p tilolang-app --`,
`target/<profile>/tilolang`) runs every command, `run` included. The compiler crate has the same
command line without `run` under two names, `tilolangc` and the short `tlc`
(`cargo run -p tilolang --bin tlc --`), for where only the compiler is built. The examples
below say `tilolang`; `tlc` takes the same arguments.

```
tilolang check [dir] [--json] [--watch]   every error of the package and its imports
tilolang build [dir]                      writes dir/app.wasm; updates the ids locks
tilolang fmt [dir] [--check]              rewrites every file in canonical form
tilolang ids [dir] [--rename old new]     prints or edits the ids lock
tilolang run [dir] [--peers N] [--fresh]  runs the package in a window
```

`dir` defaults to the current directory. Errors print as `path:line:col: E0xx message`;
`--json` prints them as one JSON array, each with its `file`. `check --watch` checks again
whenever a `.tl` file of the package or its imports changes. Exit status: 0 fine, 1 errors in
the sources, 2 anything else (a missing directory, a bad option).

`fmt` prints the one canonical text projection. It keeps `///` doc comments; plain `//`
comments are not part of the canonical form, so `fmt` leaves a file that has any alone and
names it, instead of deleting what someone wrote.

## The development loop

```
cargo build -p tilolang-app
target/debug/tilolang run apps/tl/counter --peers 2
```

`run` compiles the package and its imports from their directories and opens a window that runs
them, attached to no Tilo session: it uses no workspace, room or identity of the person running
it. Then:

- **Edit and save.** When any `.tl` file of the package or of an import changes, the packages
  are compiled again and the window loads the new modules. What the app stores lives in the
  room, keyed by declaration ids, so it stays; the open view is opened again over the new code.
  A build with errors keeps the last good one running: the terminal lists the errors, and the
  window says that there are some.
- **Collaborate.** `--peers N` (1 to 8) shows N replicas of the room side by side, each a
  different person (`peer-1`, `peer-2`, ...) with their own copy of the room's record. A change
  in one replica is merged into the others as soon as it is committed, as sync delivers it, so
  concurrent edits can be tried on one machine.
- **Keep the room.** The room is kept between runs in
  `$XDG_STATE_HOME/tilolang-dev/<package>-<hash>/room.automerge`. `--fresh` starts empty.

New declarations get their ids when they are first compiled, and `run` writes them to the
package's `app.ids` straight away, so every replica and the next build agree on them.

Inside Tilo, the host app (`so.tilo.lang`) runs the built-in packages it embeds
(`apps/tilolang/src/packages.rs`) in the room's record, synced with the room's members.
