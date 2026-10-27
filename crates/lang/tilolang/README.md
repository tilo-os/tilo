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

**Parameter defaults.** An action parameter may have a default written in terms of the
parameters before it. An invocation that leaves the argument out gets it, computed when it is
invoked on what the invoker sees, and records it as the argument; an explicit argument wins:

```
action check(item: Todo, done: Bool = not item.done) (label: if done { "Check" } else { "Uncheck" }, on: item.done) {
    item.done = done
}
```

A register written from itself (`item.done = not item.done`, `c.n = c.n + 1`) is an error
(E039): it is computed again wherever the change is applied. Take the outcome as a parameter
with a default, as above, or make the field a counter for a delta.

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

**Native vocabulary.** Some smaller pieces of syntax:

- An enum case takes an icon: `all (icon: .file_text)`.
- An action's `label:` and `icon:` may be an `if` over its subject, and `on:` is a Bool over
  the subject. The `else` branch is the static fallback:
  `action pin(n: Note) (label: if n.pinned { "Unpin" } else { "Pin" }, on: n.pinned)`.
- `Date("2026-10-24")` and `Time("2026-10-24T09:30:00Z")` are ISO text that compares with the
  ordering operators. `date(bind: t.due)` edits one. In an action, `now` is the invocation's
  Time.
- A scenario names people with `let mia = person("Mia")`. Inside `as mia { post(t, "Hi") }`,
  the actions called run with `invoker` = mia.
- `composer { ... }` is the entry bar of a frame and stands directly in `page`, `document` or
  `canvas`.
- `xs.first()` and `xs.last()` give an `Option` of the item.
- A row view may have `let`s before its `row`. `row(r)` with a resource shows that resource's
  row. `card(r, count: n)` shows a count.
- A card is a summary of its resource: a `list` inside a `card` shows its first five rows (the
  list node's `preview` property), the count says how many there are, and the card opens the
  resource for the whole list.

## Commands

`tilolang` is the language's tool (`crates/lang/tilolang-cli`, `cargo run -p tilolang-cli --`).
The compiler crate has the commands that need nothing but the compiler under two names,
`tilolangc` and the short `tlc` (`cargo run -p tilolang --bin tlc --`).

```
tilolang check [dir] [--json] [--watch]   every error of the package and its imports
tilolang build [dir] [--out DIR]          writes a signed Tilo app per package: OUT/<name>.tapp
tilolang fmt [dir] [--check]              rewrites every file in canonical form
tilolang ids [dir] [--rename old new]     prints or edits the ids lock
tilolang run [dir] [--peers N] [--headless]  runs the package as an app in a nested Tilo session
```

`dir` defaults to the current directory. Errors print as `path:line:col: E0xx message`;
`--json` prints them as one JSON array, each with its `file`. `check --watch` checks again
whenever a `.tl` file of the package or its imports changes. Exit status: 0 fine, 1 errors in
the sources, 2 anything else (a missing directory, a bad option).

`fmt` prints the one canonical text projection. It keeps `///` doc comments; plain `//`
comments are not part of the canonical form, so `fmt` leaves a file that has any alone and
names it, instead of deleting what someone wrote.

## Packages are apps

Every package is a Tilo app of its own. `tilolang build` compiles it and the packages it uses
and writes each as a signed `.tapp`:

- the manifest, generated from the `app` declaration: the package id is the app id, its name
  and `category:` are the app's, its `icon:` (a platform glyph, `.file_text`) is drawn into the
  payload's `icon.svg` and signed as `[package] icon`;
- the entrypoint: `[entrypoint] runtime = "tilolang/<version>"`, `bin = "bin/tilolang-app"`. The
  launcher runs the tilolang runtime bundle's binary (`apps/tilolang`) and points it at the
  package's payload with `TILO_APP_MODULE`;
- the payload: `app.wasm`, `app.ids`, the metadata of the packages it uses (`imports/`), the
  `surface.toml` generated from the metadata (declaring the kinds it reads from the packages it
  uses as `reads`), and the icon.

The runtime compiles nothing: it loads the one module it was started for, under the id the
package was signed as. Each app has its own window, record, custody and permissions. It reads
another app's records only as its surface declares (`reads`), read-only; it changes another
app's resources only by invoking that app's actions.

## The development loop

```
dist/dev/nested-session-build.sh target/nested-session   # once; TILO_NESTED_APPS="" builds no other app
cargo run -p tilolang-cli -- run apps/tl/counter --peers 2
```

`run` builds the package into `.tapp`s for a locally built nested Tilo session
(`dist/dev/nested-session.sh`, or `dist/dev/nested-pair.sh --peers N` for N people in one
community workspace), installs them there and opens the app through the session's own action,
so the launcher starts it as on a real system. The session bakes the tilolang runtime bundle from
the build. Then:

- **Edit and save.** When any `.tl` file of the package or of an import changes (inotify), the
  packages are built again, reinstalled with `tilo-install --force`, and the app's window is
  closed and opened again on the new build. No cargo build runs. A build with errors keeps the
  last good one installed; the terminal lists the errors.
- **Collaborate.** `--peers N` runs N sessions, each a different throwaway person, joined to one
  workspace.

New declarations get their ids when they are first compiled, and `build` writes them to the
package's `app.ids` straight away, so every build agrees on them.
