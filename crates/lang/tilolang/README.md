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

action increment(c: Counter) {
    c.value += 1
}

view main(c: Counter) {
    page {
        heading("{c.value}")
        button("+1", on: increment(c))
    }
}
```

`view main(root: T)` is what the host opens; `T` is the package's root resource, one per room.

`use other.app as other` imports another package. Imports are found among the package's sibling
directories (for `apps/tl/board`, the other directories of `apps/tl`); `--path <dir>` adds a
package directory, or a directory of packages, to look in.

## Commands

The command line is the `tilolang` binary. The compiler's own (`cargo run -p tilolang --`)
runs everything but `run`. The host app's binary, also called `tilolang`
(`cargo run -p tilolang-app --`), runs every command, `run` included. Both write
`target/<profile>/tilolang`, so whichever was built last is the one there; build the host to get
`run`.

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
