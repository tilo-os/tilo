# Runtime size

Every thin app maps one shared Rust dylib, `libruntime_dylib.so`. A device
downloads it when an app's runtime is missing, and every app process applies
its dynamic relocations before `main`. `tools/perf/runtime_size_ratchet.py`
reads four numbers from the ELF file, per architecture: `bytes`,
`relocation_bytes` (the relocation tables), `relocated_bytes` (the 4 KiB pages
those relocations dirty in each app process) and `symbol_relocations` (the
relocations that need a symbol lookup). The budgets are in
`tools/perf/runtime-size-budget/budget.toml`, with the same headroom, rounding
and raise files as the [app budgets](app-package-size.md).

GLib and GStreamer are part of the runtime but not of every app's start:
`tools/ci/rustc-thin-app-wrapper.sh` links `libruntime_dylib.so` through
`tools/ci/runtime-dylib-lazy-link.py`, which keeps them out of `DT_NEEDED`,
serves the runtime's calls into them through hidden trampolines that load them
on first use, and records their SONAMEs in the `.tilo.lazy_needed` section. The
runtime bundle tools (`os/scripts/collect-runtime-dependencies.py`,
`verify-runtime-bundle-closure.sh`) ship and verify those libraries like
`DT_NEEDED` ones. A library of `[targets] needed_absent` in the runtime's
`DT_NEEDED`, in what every app loads with it, or in what a built app of
`apps_without_media` loads at exec fails the build gate; if the runtime starts
reading a GLib or GStreamer *variable*, the link itself fails, since nothing can
provide one before the library loads.

- **Release / apps** (`Shared runtime within its size budget`) measures the
  dylib it has just built, and the closures above. Its next steps report the
  loader's cost for an app without media (statistics, the objects it loads,
  its private dirty memory) and, when Video Player is built, decode its H.264
  fixture through the thin runtime.
- **Perf / scheduled** (footprint group) fetches the newest published
  `so.tilo.runtime` of each architecture, holds it to the budgets and records
  `footprint.runtime.<metric>.<arch>`.
- **CI / fast quality** fails when a budget grows without a raise file new in
  the same change.

To raise a budget for an intended growth, add one file per change:

```toml
# tools/perf/runtime-size-budget/raises/<change>.toml
reason = "the toolkit gained a PDF engine"  # at least 20 characters
run = 90210                  # the Release / apps run that measured the growth

[runtime.x86_64]             # added to each metric's budget
bytes = 4194304
```

When the runtime shrinks, both gates print the lower budget to commit.

What the runtime may carry is a rule of its own. It holds the SDK and crates
several apps share; never an engine only one or two apps use, an OS daemon or other OS-internal crate (the filesystem, the
launcher, the app host, the install CLI, the peer transport) and nothing no
app reaches. An app that calls into such a crate links it itself and gets only
the code it calls; the Store, for one, depends on `app-host` without its
default `service` feature, and Settings, Agent and Canvas depend on
`tilo-model-gateway` without its: the daemon's provider clients, on-device
model service and subject-model runtime (rten) stay in the gateway, and an
app gets only the client that asks it. `tools/policy/check_runtime_dylib_surface.py` (CI /
fast quality) resolves the runtime with the default collection the way the
release does and fails on either, and on a crate of an engine that has moved
out.

An engine a few apps use moves out of the runtime into an engine library
of its own: a crate under `crates/runtime/engines/<name>/` that is a Rust
`dylib` chained on the runtime (Typst: `libtilo_typst_engine.so` for Typst
Editor; Spotify playback: `libtilo_spotify_engine.so` for Music; the wet-media
simulation and subject mattes: `libtilo_paint_engine.so` for Canvas; the
YouTube client, rustypipe with its QuickJS: `libtilo_youtube_engine.so` for
Music and Tube; account custody with its OAuth client and cloud-storage
providers, the IMAP client, and the WebDAV/CalDAV client whose calendar sign-in
reaches custody: `libtilo_accounts_engine.so` for Mail, Settings, Calendar,
Imager and the Spotify engine; the time-zone database, chrono-tz:
`libtilo_timezone_engine.so` for Calendar and Clocks; the contact directory:
`libtilo_contact_directory_engine.so` for Contacts, Mail and Messages; Tilo's
PDF engine, tilo-pdf with the hayro rasteriser, the pdfium helper's client and
PDF export: `libtilo_pdf_engine.so` for PDF Viewer, Files, Writer and Print
Centre, which hand its first-page rasteriser to the thumbnailer every app
shares; the workspace filesystem's FUSE presentation, runtime-fs-fuse with
fuser: `libtilo_fuse_engine.so` for Imager's projection worker). It is
signed into the runtime bundle beside `libruntime_dylib.so`, so the runtime's
id, signature and verity cover it, but only its apps name it `DT_NEEDED`;
every other app no longer maps or relocates it. Apps share one engine library
rather than each carrying a copy in its tapp. The conventions the build
relies on:

- the package is `tilo-<name>-engine`, its library `tilo_<name>_engine`; every
  dependency sits behind a `thin` feature, so builds other than the thin one
  (workspace checks and tests, `tilo-app dev`) see an empty library;
- a `[[bin]] tilo-<name>-engine-check` with `required-features = ["thin"]`:
  `os/scripts/build-thin-apps.sh` builds and runs it in every runtime build,
  whichever apps are selected, and writes the libraries it built to
  `target/thin-app/runtime-engines`, which packaging signs into the bundle;
- `tools/ci/rustc-thin-app-wrapper.sh` gives `tilo_*_engine` crates the
  runtime's link flags (`-C prefer-dynamic` is what lets one Rust dylib link
  another);
- the app links the engine under a feature of its own (`runtime-engine` in
  Typst Editor, Music, Canvas, Tube, Mail, Settings, Calendar, Clocks,
  Contacts, Messages, PDF Viewer, Files, Writer, Print Centre and Imager), named in `dist/apps/app-collections.toml`, the
  Containerfile's `APP_FEATURES` and `os/components.toml`; without it the app
  links the engine's crates statically, as `tilo-app dev` does;
- one app may not load two engine libraries that both carry a crate (rustc
  refuses a crate that shows up twice in a link); an engine whose crates
  overlap another's that its app always loads with it chains on that one
  instead (`tilo-spotify-engine` takes rand 0.9 and sha1 0.10, which librespot
  shares with rustypipe, from the YouTube engine Music loads beside it, and
  account custody, which Spotify's sign-in uses, from the accounts engine);
- the engine's crates go into the surface check's `ENGINES`. The
  workspace-hack listing one does not put it into the runtime: the hack's
  library names none of its dependencies, and rustc links only the crates a
  crate names, so a crate only the hack reaches is on no link line of
  `libruntime_dylib.so` (the release's runtime link names none of
  russh, wasmi or the other crates only the hack lists). The check follows
  real dependencies and the features apps enable on them, which is how an
  engine's crate comes back (Calendar's `live` asking tilo-dav for the
  account custody its sign-in reaches, for one).

Release / apps reports each engine's bytes, relocations and dirtied pages
beside the runtime's, fails when an app of `apps_without_media` loads one it
does not use (`[engine_users]` in the budget names the ones those apps use:
Clocks the time-zone database), renders Typst Editor through its engine, runs
Music's Spotify engine (`music --probe-spotify-engine`, librespot's code
without an account, network or audio device), and runs the other engines
from each app that loads them (`canvas --probe-paint-engine`, `music` and
`tube --probe-youtube-engine`, `mail --probe-accounts-engine`, `calendar` and
`clocks --probe-timezone-engine`, `contacts`, `mail` and
`messages --probe-contact-directory-engine`), checking each app loads its
engine itself. Settings, whose tapp stays as it is, has no probe; the trace of
its start shows it loads the accounts engine itself.

### Engines the toolkit opens on first use

Some code the toolkit reaches on behalf of every app, but only once a person
does something particular: the syntax
grammars (syntect, fancy-regex and two-face's packed grammars) for a line of
code, the ad-block compiler (adblock-rust) for the browser's filter lists. No
app names those engines: the toolkit opens them by name the first time
(`tilo-engine-loader`: `dlopen` with local symbols, never closed) and calls
one entry function, whose type the toolkit defines and the engine crate is
checked against at compile time (`tilo_highlight::GrammarsEntry`,
`ui::webview::adblock::EngineEntry`):

| engine library | opened by | the first time |
|---|---|---|
| `libtilo_highlight_engine.so` | the code editor's highlighter (`lazy-highlight`) | a line of code is highlighted |
| `libtilo_adblock_engine.so` | `ui::webview::adblock` (`lazy-adblock`) | an app compiles filter lists |

The runtime-side crate names the engine's code only while its `lazy-*`
feature is off, and each engine's `thin` feature turns it on, so in the
release graph rustc never puts the engine on the runtime's link line, while
every other build (`tilo-app dev`, workspace tests, the shell) links it as
before. The surface check knows these edges (`LAZY_EDGES`). The engine's
check binary goes through the toolkit's own path, so a release proves the
runtime finds and runs it. A library that does not open is logged once:
code highlights as plain text, filter lists
do not compile. The Typst engine chains on the highlight engine (Typst's raw
blocks use the same syntect and grammars), so Typst Editor maps one copy and
the toolkit's `dlopen` finds it already loaded.

The search engine (tantivy) is in the runtime again. Splitting it into a
library of its own left tilo-search naming it, so the runtime kept tantivy
and, through tantivy-sstable, zstd, and the engine library then called the
runtime's copy of zstd's C library, whose symbols a Rust dylib does not
export. It stays one copy in the runtime until tilo-search can leave it out
entirely.

### Tables without relocations

A table of `&str` or slices is a table of pointers, which the loader writes
in every process before `main`. Where the runtime needs such a table, it is
laid out as offsets into one string literal instead, which is plain
`.rodata`: MIME types by extension (`tilo-mime`, generated from mime_guess by
`tools/mime-table.py`), the BIP39 wordlist of recovery phrases
(`threshold-identity`'s `bip39.rs`), pulldown-cmark's HTML entities
(`vendor/pulldown-cmark`) and regex-syntax's Unicode name and case-folding
tables (`vendor/regex-syntax`, written by `tools/regex-syntax-tables.py`).
The surface check fails on mime_guess or bip39 back in the runtime
(`TABLES`).

### X11

Every app runs as a Wayland client in its sandbox, so the runtime leaves
winit's X11 backend (x11rb, x11-dl) and the X11 clipboard (arboard on Linux)
out. A development build on an X11 host turns them on with `tilo-app`'s
`x11` feature (`--features ui/x11`); the shell keeps X11 for its hosted
backend. hakari does not unify winit or xkbcommon-dl (`.config/hakari.toml`),
or the shell's X11 would return through the workspace-hack.

### Expected effect of the engines on first use, the tables and X11

Not measured: no build ran for these changes. From the previous round's link
map of the runtime at main (aarch64, `.data.rel.ro` 4.0 MB before the PDF
engine left, about 3.5 MB after), these leave the relocated data every app
process copies and relocates before `main`:

| change | relocated data | code out of the runtime |
|---|---|---|
| search engine on first use (tantivy, rust-stemmers; reverted, see above) | ~120 KB stemmers + tantivy's vtables and strings, ~0.2 MB | ~2-3 MB |
| syntax grammars on first use (syntect, fancy-regex, two-face) | tens of KB | ~2 MB code, 0.9 MB grammar data |
| ad-block compiler on first use | tens of KB | ~1-2 MB |
| mime types from `tilo-mime` | ~122 KB | — |
| regex-syntax's tables | ~121 KB | — |
| pulldown-cmark's entities | ~68 KB | — |
| BIP39 wordlist (and bitcoin_hashes) | ~33 KB | small |
| X11 (x11rb, x11-dl, arboard) | tens of KB | ~1 MB |
| FUSE presentation (fuser) | small | ~0.3 MB |

About 0.6-0.7 MB in all: `.data.rel.ro` from ~3.5 MB to ~2.8-2.9 MB, about
160 fewer pages copied and relocated, ~0.3-0.4 ms of the loader-only time at
the measured ~2 µs a page (5.97 ms before the PDF engine moved; expected
~4.2 ms now, still above the 3 ms target). What is left is mostly the
toolkit's own (tilo-ui about 570 KB), ~39k panic locations (a file-name
pointer each, ~0.9 MB) and ~89k vtable entries: see app-start-phases.md for
what reaches 3 ms. Cold, the runtime file shrinks by roughly 8-10 MB of code
that no first frame touches.

Apps get smaller too: an app that encodes a PNG or JPEG no longer
instantiates the `image` crate's encoders in its own binary (`ui::png`,
`ui::jpeg`, compiled once in the runtime; about 50 KB per app that encoded),
and Imager loads account custody and the FUSE presentation from their
engines instead of carrying them.

## What an app start pays for the runtime

Warm, the loader's work before `main` is almost all the runtime's: it copies
and relocates every page of `.data.rel.ro` (vtables, `&str` tables, panic
locations: about 5 MB, one copy-on-write fault per 4 KiB page, about 2 µs
each on the Yoga) and looks up the symbols the runtime, libstd and the app
bind to each other. Each crate in the runtime costs every app its share of
that table, whether the app calls it or not, which is why the budget counts
`relocated_bytes` and `symbol_relocations`.

The runtime and its engines link with `-Bsymbolic`: references to their own
functions *and data* bind at link time. Rust never interposes a symbol, and
apps are PIE that reach the runtime's data through the GOT, so nothing else
can define one; `os/scripts/build-thin-apps.sh` fails an app or engine with a
copy relocation, the one construct that would split a static in two. Binding
functions only (`-Bsymbolic-functions`) left about 3.8k lookups of the
runtime's own vtables, statics and Wayland interface tables in every app
process.

Cold, the same pages come from disk. `tilo-runtime-resident.service`
(`tilo-launcher runtime-resident`, `crates/packaging/launcher/src/runtime_resident.rs`)
maps the runtime library and the libraries of its bundle it needs, for every
runtime the system registry names, read-only, and locks them once per boot,
at idle I/O priority. The page cache then holds one copy that every app maps,
and no app start waits on the disk for it. It runs no app code; engines and
the media libraries loaded on first use stay out. Its memory is the runtime's
size, which is one more reason the runtime stays small.

A runtime a person installed into their own store is kept by their session
(`tilo-launcher runtime-resident --user`, the user units
`tilo-runtime-resident.service` and `.path`), by what the bytes are, never by
where they are: an ABI-bound version names its payload's digest, so when the
system holds a bundle of that version the launcher runs the app on the
system's copy (the same bytes, already resident); the person's other
runtimes are verified as a launch verifies them, and each library is kept
only when the kernel measures the publisher-signed fs-verity digest on the
descriptor that is mapped. Unsealed files are left to the page cache. The
session's `RLIMIT_MEMLOCK` bounds what is locked; past it the files stay
mapped and read in.
