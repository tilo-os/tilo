# App package size

"Apps are ~2 MB" is measured as the bytes of the signed `.tapp` a person
downloads, per architecture. The KPI targets are a median of at most 2 MB, a
p90 of at most 4 MB and a max of at most 6 MB (decimal). Each app has its own
budget in `tools/perf/tapp-size-budget/budget.toml`, set at its measured size
plus 1 % headroom, rounded up to 64 KiB. `tools/perf/tapp_size_ratchet.py`
enforces the budgets in these places:

- **`app-sizes-{arch}`** (`Shipped tapps within their size budget`, and the
  runtime budget beside it) packages the default collection with throwaway
  identities and measures every tapp and the shared runtime. It fails when an
  app is over its budget or has none. `app-reproducible-{arch}` packages
  Calculator twice (`tools/apps/check-reproducible-tapp.sh`, the second time
  compiled again past the compiler cache) and fails on any byte that differs:
  an app whose package changes without its source re-sends itself in every OS
  update.
- **`tools/ci/perf-scheduled.sh footprint`** holds every published app to
  its budget. It also holds the apps' median, p90 and max to the fleet
  ceilings: the KPI targets, or an interim ceiling where the fleet does not
  meet a target yet. The budgets are those of the commit the newest release
  was built from, since the registry serves that release until the next one
  publishes.
- **t's `changes` check** runs `check-budget`, a text check. A budget or ceiling
  may only grow by what a raise file new in the same change adds. The budgeted
  apps must be exactly the default collection, so an app added to `apps/`
  brings its budget. A new app may start at the 6 MB max target until it is
  measured.

The OS image's app store holds each app to the same budgets when it packs it:
`os/scripts/pack-apps.sh` reads `budget.toml` and the raise files and refuses
a package over its budget for the build's architecture, naming the app, its
size and its budget, before it applies the format's 8 MB ceiling
(`crates/core/tilo-app-package/max-app-bytes`). Both build the app the same way. The
release package the size checks measure also carries the app's search entries and its
instruction-set note, which makes it the larger by about 1 KB (at e82aadce5c on aarch64: Code
Editor 2625536 bytes against 2625024, Messages 1332224 against 1331712, Store 2721792 against
2720768), so an app within its budget in a size check is within it when the image packs it.
A build whose binaries are
not the release profile the budgets measure (`TILO_RUST_LTO=off`) reports the
app instead of refusing it (`TAPP_BUDGET=report`).

A developer session (`dist/dev/nested-session.sh`, `tilolang dev`) installs the apps it
built itself, signed with its own development key, and never publishes them; the `thin-dev`
profile builds them for build speed rather than size. Its store, a build with
`dev-overrides`, installs a package whose signature verifies under a key on the session's
publisher list (`TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE`) under the 64 MiB desktop-build
ceiling instead of the 8 MB one (`package_store::size_limit`). A shipped store reads no such
list, and the registry holds every application to the 8 MB ceiling.

The population is the default collection's apps. The runtime package is the
runtime's own KPI. The runtime and provider stubs are not apps, and apps the
registry still lists but this repository no longer builds are not counted. The
footprint run names every package it leaves out.

To raise a budget for an intended growth, add one file per change. Never edit
the base numbers upward:

```toml
# tools/perf/tapp-size-budget/raises/<change>.toml
reason = "the toolkit gained guarded OS dictation"  # at least 20 characters
run = 90210                  # the run that measured the growth

[apps."so.tilo.settings"]    # bytes added to each app's budget
x86_64 = 65536
aarch64 = 65536

[fleet.x86_64]               # only if a fleet ceiling is crossed
p50 = 65536
```

A failing the app release job prints this file with each over-budget app's
numbers. Each architecture's job prints its own lines, so merge them. A raise
may at most double a budget. When an app shrinks, both checks print the lower
budget to commit (`[apps."<id>"] <arch> = <bytes>`), with the app removed from
its raise files. They also print when an interim fleet ceiling can be lowered,
or removed once its target holds. To re-seed the budgets from the registry,
run `python3 tools/perf/tapp_size_ratchet.py seed entries.json`, where
`entries.json` is `https://apps.tilo.so/entries`.

To read what an app carries, build as a release does with the symbol tables kept
(`TILO_THIN_KEEP_SYMBOLS=1 dist/apps/package-apps.sh`) and sum them by crate:

```sh
tools/perf/tapp_symbol_sizes.py measure target/thin-app \
    --plan "$TILO_APP_RELEASE_OUT/plan.json" --out head.json
tools/perf/tapp_symbol_sizes.py diff base.json head.json --app so.tilo.browser
```

The sum is named code only; the shipped tapp also holds unwind tables, read-only data and the
package format. A crate's bytes in one app are what that app instantiates of it: the toolkit
type's generic code there (a widget, a dialog, a serde instance) is code every app using it
compiles again, and is usually cheapest to fix in the toolkit. A warm build after a toolkit
change takes about five minutes on sixteen cores.

The symbol table undercounts such code: a small generic function is also inlined at each call,
so a fix often saves several times what `nm` shows for it. What has paid most so far:

- A generic entry point hands its work to a function that is not generic: a constructor taking
  `impl Into<String>` builds from a `String`, a builder taking a closure boxes it first.
  Thirteen of the toolkit's (`ListRow::new`, `EmptyState::new`, `Button::on_effect`, ...)
  took 968,806 loadable bytes out of the 64 x86_64 apps (cdb9027712).
- An impl that would be `#[inline]` is written out: a derived `PartialEq` or `Clone` on a
  large type (`Theme`) is copied into every caller.
- A dependency's inline or generic code that every app reaches goes out of line where the
  runtime holds it once: `vendor/serde_json` (31d03ee94e) took 966,311 bytes out of the same
  apps. Together the two changes made the tapps 1,932,800 bytes smaller on x86_64 (median
  27 KB each) and 1,739,776 on aarch64.
- A generic type whose bulk does not read its parameter keeps the bulk in a part that is not
  generic: a chain reads its parts' resource through one table of functions per part type
  (`tilo-app`'s `chain/part.rs`), an effect vocabulary's specs are built by one function handed
  the vocabulary's argument check, and a popover's panel and option menu are one struct beside
  its content. The effect specs and the popover took 1,622,528 bytes out of the 66 x86_64 tapps
  (median 20,992; Tasks 113,152, Feeds 77,312, Camera 80,384), the chain 41 to 49 KB out of each
  of the six apps that keep one (267,776 in all).
- A virtual list's code reads its model through `AnyModel` and keeps its keys as `ListKey`
  (`VirtualRows`); `VirtualList<M>` only converts between them and `M`. Each model type had
  cost an app about 200 KB: Tasks, Notes, Messages, Passwords, Snippets, Backups, Code Editor and
  Canvas came out 200 to 440 KB smaller each.

Not every erasure pays. Routing `Widget`'s default methods through a trait object made the apps
larger: the per-type copies of a leaf widget's defaults were identical, so the linker already
folded them into one, and each widget's vtable gained two entries.

Every crate in the runtime's graph is provided to an app, re-exported or not: giving
`runtime-dylib` a `pub use serde_json`, `pub use serde` and `pub use tilo_resource` between two
symbols builds changed 84 bytes across the 65 tapps. What an app's symbol table shows under a
shared crate's name is what the app instantiates of it, not a copy of it: of the 1,635
`tilo_resource` symbols in Calendar's binary six are non-generic, and none of its 974 `tilo_ui`,
220 `tilo_app` and 527 `serde_json` ones are.

`tools/binary-budget.toml` is different: it is a weekly guard against static
linking in release binaries, not this KPI.

## What the apps gained, 2026-11-09 to 11-15

The budgets were last raised at 9ee2d3e1439 (run ra2f0869c96). Four days later 25 tapps were
over them. Symbols builds at a4106fe8b0c (the first commit after that raise that compiles:
code-editor's own build was broken in between) and at 0c8ae209a88, summed by crate with
`tools/perf/tapp_symbol_sizes.py`, attribute the growth. Named code is what `nm` sees; a tapp is
larger by its unwind tables, read-only data and the package format.

| app | budget | shipped | over | named code 11-09 | named code 11-15 | own crate | tilo_resource | serde_json | core + alloc |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| browser | 3,276,800 | 3,901,440 | +624,640 | 1,841,759 | 2,262,552 | +184,689 | +36,206 | +16,698 | +96,289 |
| mail | 3,866,624 | 4,194,816 | +328,192 | 2,046,296 | 2,314,242 | +30,829 | +22,435 | +102,422 | +28,288 |
| tasks | 2,686,976 | 2,969,088 | +282,112 | 1,609,550 | 1,810,409 | +9,498 | +26,756 | +21,976 | +50,982 |
| calendar | 2,752,512 | 2,981,376 | +228,864 | 1,588,919 | 1,770,317 | +2,601 | +10,572 | +100,236 | +12,757 |
| ebook | 2,490,368 | 2,698,240 | +207,872 | 1,417,038 | 1,587,559 | +8,339 | +5,683 | +97,994 | +10,942 |
| notes | 2,293,760 | 2,453,504 | +159,744 | 1,264,180 | 1,405,165 | +13,023 | +12,653 | +11,851 | +45,381 |
| chat | 4,390,912 | 4,515,328 | +124,416 | 2,562,583 | 2,698,611 | +10,362 | +3,456 | +17,342 | +22,048 |
| discord | 2,162,688 | 2,270,720 | +108,032 | 1,176,930 | 1,229,467 | -1,867 | +16,030 | +4,625 | +28,047 |
| messages | 2,162,688 | 2,240,000 | +77,312 | 1,274,188 | 1,347,562 | +2,756 | +7,373 | +908 | +20,524 |
| video-editor | 2,293,760 | 2,378,240 | +84,480 | 1,331,499 | 1,369,113 | +24,294 | +384 | +5 | +10,777 |
| snippets | 1,310,720 | 1,356,800 | +46,080 | 675,593 | 703,583 | -1,124 | +621 | -9 | +10,899 |
| clocks | 1,507,328 | 1,545,728 | +38,400 | 770,347 | 799,621 | +2,588 | +1,402 | +161 | +12,457 |
| shortcuts | 2,228,224 | 2,263,040 | +34,816 | 1,190,967 | 1,217,913 | +5,498 | +1,045 | -37 | +10,268 |
| code-editor | 3,604,480 | 3,640,320 | +35,840 | 2,100,555 | 2,146,906 | +3,922 | +5,106 | +177 | +12,552 |
| passwords | 1,835,008 | 1,866,752 | +31,744 | 1,053,147 | 1,091,444 | +9,400 | +810 | +82 | +11,891 |
| db-client | 4,390,912 | 4,422,144 | +31,232 | 3,126,259 | 3,189,925 | +1,778 | +1,352 | -25 | +4,471 |
| totp | 1,376,256 | 1,405,952 | +29,696 | 747,597 | 770,440 | -637 | +928 | -14 | +5,356 |
| terminal | 1,900,544 | 1,924,608 | +24,064 | 1,068,174 | 1,087,292 | +2,632 | +2,621 | +264 | +3,800 |
| contacts | 1,900,544 | 1,914,880 | +14,336 | 995,005 | 1,012,003 | +859 | +1,724 | +47 | +6,529 |
| remote | 2,949,120 | 2,962,432 | +13,312 | 2,001,831 | 2,020,867 | +616 | +1,171 | +1 | +8,591 |
| maps | 1,572,864 | 1,583,104 | +10,240 | 882,102 | 892,608 | -1,796 | +1,460 | +189 | +3,974 |
| feeds | 1,703,936 | 1,712,640 | +8,704 | 921,643 | 930,135 | +417 | +720 | -52 | +5,772 |
| backups | 1,441,792 | 1,445,888 | +4,096 | 705,677 | 739,299 | +4,054 | +876 | +43 | +11,948 |
| pdf-viewer | 2,359,296 | 2,362,368 | +3,072 | 1,346,419 | 1,368,317 | +13,266 | +1,752 | +3,521 | +13,058 |
| weather | 1,048,576 | 1,049,600 | +1,024 | 548,648 | 550,421 | -383 | +696 | -50 | +3,127 |

What the columns say:

- **Own crate**: Browser's page forms, page record and page review (+184,689), Video Editor's
  inspector and timeline drawing (+24,294), Pdf Viewer's page rendering (+13,266), Notes'
  metadata and notebook (+13,023), Mail's own mailbox per member and page sends (+30,829),
  Tasks' import inbox (+9,498) and its model crate (+55,098 in `tasks_model`).
- **`serde_json`** is where the shared growth sits: Calendar's own crate grew 2,601 bytes and
  its serde instances 100,236; Ebook 8,339 and 97,994; Mail 30,829 and 102,422. Each record and
  action type an app reads out of a `Value` compiles
  `<Map<String, Value> as Deserializer>::deserialize_any` for its own visitor again, 1.5 to
  6 KB a type, and nested shared types (`tilo_document::action::RichTextAction` in Calendar: a
  dozen instances of 1.5 to 1.9 KB in its largest twenty) are instantiated in the app that reads
  them. The types are the apps'; the machinery is one set of code compiled again in each of the
  65 tapps.
- **`core` and `alloc`** are the fallout of those instances: drop glue, iterators and
  formatting for the same types.
- **Two tapps carry a dependency of their own.** Db Client links the bundled SQLite itself (a C
  library an app calls, so the runtime cannot hold it) and its rusqlite bump 0.37 to 0.41
  brought a newer SQLite: 30 KB of `sqlite3_*` code in the ten largest new symbols. Remote
  links its SSH client (russh, with the curve crate its new key type needs) for 9 KB. Neither
  is shared with a second tapp, so an engine library would only move them.
- **None of this is a dependency carried whole.** Every crate an app names that the runtime
  links is provided by it, so the only way to cut these bytes is a non-generic entry point for
  the shared types the apps read.

## Shared types read and written in the runtime, 2026-11-15

Most of the `serde_json` growth above was one type: `tilo_document::action::RichTextAction`,
whose 53 struct variants each compiled a visitor in every app that edits rich text (through
`tilo_resource`'s generic vocabulary and child-resource code, instantiated for `RichText`).
`RichTextAction` now derives its serde code with `#[serde(remote = "Self")]` and
`tilo_actions::read_through_json_value!(RichTextAction, write)` reads and writes it through
one `Value`, in functions compiled once in the runtime. The vendored `serde_json` also moved
the non-generic steps of reading a `Value` (the next element, key and value, the end check,
an enum's single-key map) and of writing one (inserting a field, element or map value) out of
every visitor and serializer (`vendor/serde_json/TILO-PATCH.md`).

Named code from symbols builds before and after, with `tools/perf/tapp_symbol_sizes.py`:

| app | before | after | saved | of it `serde_json` |
| --- | --- | --- | --- | --- |
| tasks | 1,810,080 | 1,399,904 | 410,176 | 297,728 |
| mail | 2,314,506 | 2,149,129 | 165,377 | 114,052 |
| calendar | 1,770,394 | 1,605,098 | 165,296 | 113,476 |
| ebook | 1,587,402 | 1,422,638 | 164,764 | 113,044 |
| browser | 2,262,647 | 2,253,515 | 9,132 | 7,896 |

t's app-sizes-x86_64 run ra8e3b25f8a measured the tapps the x86_64 budgets were lowered to
(Slides' tapp, for one, is 171,008 bytes smaller). The aarch64 budgets wait for a measurement on
that architecture.

Eight of the twenty-five apps that raise covered — Browser, Calendar, Chat, Code Editor, Ebook,
Mail, Notes and Tasks — fell below their raised budgets with that change, so their budgets were
lowered instead. The other seventeen keep their raise, and the comment above each app in
`tools/perf/tapp-size-budget/raises/apps-features-2026-11-09-to-11-15.toml` names what grew in it:
its own feature work, its records answering a read and naming their labels from its catalogue, the
framework code those instantiate for its own types, and, for Db Client and Remote, the one
dependency its tapp carries alone (the bundled SQLite, and the SSH client with its curve crate).
t's app-sizes-x86_64 run rf7bc66b852 re-measured the x86_64 tapps at 1b82c14d6b8: all of them are
within their budgets, and each budget is already the smallest 64 KiB step above its measurement
plus the 1 % headroom, so none falls.

## What the packages' model modules added, 2026-11-18

Every app's package now carries the app's model module, the WebAssembly a device
holding the workspace's keys applies its actions with (docs/architecture/apps.md). `t test
app-models` cuts each app's module out of one combined build, and both packs write it into the
package as `model.wasm`, so the module's own bytes count against the app's budget. A module is
the app's own model code: 21,846 bytes for Assistant, 138,691 for Api Tester, 509,894 for Chat.

Nothing was linked twice to pay for it. Calendar's named code is 1,594,816 bytes at ce691a3c2e
against 1,605,089 at 1b82c14d6b8, measured with `tools/perf/tapp_symbol_sizes.py`: its resource
moved from `calendar` into `calendar_model`, and `jiff` with `tilo_recurrence` replaced `rrule`
and `chrono`. Its binary grew about 40 KB of `.text`, unwind tables and `.data.rel.ro`; the rest
of its 536,880-byte payload is its module, its surface and its locales (`apps/calendar/surface.toml`,
`apps/calendar/locales`).

| app | module | tapp | budget before |
| --- | --- | --- | --- |
| chat | 509,894 | 5,211,648 | 4,587,520 |
| calendar | 499,371 | 3,290,112 | 2,686,976 |
| api-tester | 138,691 | 1,706,496 | 1,572,864 |
| maps | 129,067 | 1,730,048 | 1,572,864 |
| poker | 107,158 | 885,248 | 786,432 |
| browser | 106,793 | 4,066,304 | 3,932,160 |
| clocks | 101,405 | 1,656,320 | 1,572,864 |
| firewall | 98,535 | 1,433,600 | 1,376,256 |
| diffmerge | 97,556 | 1,272,320 | 1,179,648 |
| tube | 81,047 | 2,155,008 | 2,097,152 |
| git-client | 79,982 | 4,099,584 | 4,063,232 |
| db-client | 65,022 | 4,529,152 | 4,456,448 |
| calculator | 64,835 | 966,656 | 917,504 |
| hex-editor | 63,863 | 914,432 | 851,968 |
| remote | 59,781 | 3,022,848 | 3,014,656 |
| camera | 51,336 | 1,780,736 | 1,703,936 |
| disk-usage | 41,105 | 951,808 | 917,504 |
| containers | 31,632 | 1,488,384 | 1,441,792 |
| publisher | 30,333 | 867,328 | 851,968 |
| logs | 28,822 | 932,864 | 917,504 |
| backups | 26,779 | 1,246,720 | 1,245,184 |
| print-centre | 25,401 | 1,277,440 | 1,245,184 |
| assistant | 21,846 | 998,912 | 983,040 |
| passwords | 21,837 | 1,654,784 | 1,638,400 |

Apps that keep no model grew 2 to 52 KB of their own code: Mail, Agent, Imager, Launcher, Store,
Terminal, Typst Editor, Video Player and Windows. `raises/models-in-packages.toml` carries each
overage plus the headroom, and `raises/app-code-since-model-budgets.toml` the four that crossed
their budgets later with code of their own: Mail, Agent, Contacts and Disks.
The runtime the modules link carries what several of them reach. The date library left it when
Calendar's model became its only user: the model runtime fell to 2,431,613 bytes, under the
budget it got at its introduction, and Calendar's own module took its 124,888 bytes
(`raises/calendar-carries-the-date-library.toml`). The whole collection is still inside
the fleet's targets: p50 1,706,496 bytes, p90 3,822,592, max 5,211,648.
