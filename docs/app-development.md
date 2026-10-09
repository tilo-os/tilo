# Build a Tilo app

Install the `tilo-app` CLI once. It needs rustup and nothing else: no Tilo
checkout, no capnp, and no system development packages.

```bash
cargo install --locked --git https://code.tilo.so/tilo/tilo.git tilo-app-cli
```

From there, the shortest path from an idea to an app running in the live
development loop is one command:

```bash
tilo-app new reading-list --dev
```

Creation is atomic. Once the app exists, `--dev` runs its structural preflight,
opens it, and watches both app code and the local toolkit. A successful Rust or
asset edit replaces the running process. Before the old process exits it writes
the open document, route, and caret, and the new process restores them. Another
save while that compile is still running cancels it and builds the newest
source. Edits to a graph screen's `ui/*.json` still update that screen in place.
A failed build leaves the last good app open and waits for the next save. Press
Ctrl-C to stop.

The same stages remain available separately for automation and focused work:

```bash
tilo-app new reading-list
cd reading-list
tilo-app check
tilo-app certify
tilo-app test
tilo-app dev
tilo-app run
tilo-app shot
tilo-app inspect
tilo-app package
```

Inside a Tilo checkout, `new` places the app in `apps/`, and
`cargo run -p tilo-app-cli --bin tilo-app --` runs the CLI you are changing
in place of the installed one.
The app-aware commands find the app root from any file or directory inside it,
run the same structural checks as `doctor`, and invoke Cargo with the app's
manifest explicitly. They therefore behave the same for an app in this
workspace and for a standalone app. Arguments after `--` are sent to the app
or test harness:

```bash
tilo-app test -- --nocapture
```

For scripts and agents, every command accepts `--json`. Stdout then carries
exactly one JSON document: `{"ok": true, "command": …, "result": …}`, or
`{"ok": false, "command": …, "error": …, "causes": […], "result": …}`. A
command that ran but failed its verdict, such as `doctor`, `check` or
`scenario run`, still reports what it found in `result`. Progress text and the
output of Cargo, the app and other tools move to stderr, and nothing prompts:
`new --json` needs a name and `add --json` needs an artifact subcommand. Usage
errors are JSON too, with exit status 2.

```bash
tilo-app new reading-list --dry-run --json | jq -r '.result.files[]'
tilo-app check --json | jq '.result.stages[] | select(.passed == false)'
```

### Outside the Tilo repository

An app does not have to live in this repository. With the CLI installed as
above, create the app anywhere:

```bash
tilo-app new reading-list --id org.example.reading-list
```

Outside a checkout, the app depends on the SDK from
`https://code.tilo.so/tilo/tilo.git`, pinned to the commit the CLI was built
from. `--sdk-rev` selects another commit and `--sdk-git` another repository.
If a checkout is nearby, such as the one the CLI was built in, `--ui-path`, or
`TILO_UI_PATH`, the app builds against that checkout by relative path instead.

A standalone app builds the code this repository tests:

- `[patch.crates-io]` repeats each fork the SDK's dependency graph uses,
  because Cargo does not apply the SDK workspace's patches to an app that
  depends on it. The forks pinned in git come from their public repositories
  at the revision the workspace pins.
- `Cargo.lock` starts as this repository's lockfile, so the first build picks
  the dependency versions CI tested rather than the newest ones. Cargo drops
  the entries the app does not use.
- `rust-toolchain.toml` pins Tilo's compiler. A thin app links the toolkit
  library the OS ships, and Rust keeps that library's symbols stable only for
  the compiler that built it.
- `.cargo/config.toml` carries Tilo's target CPU floor and hardening flags,
  and the profiles in `Cargo.toml` are Tilo's `dev`, `test`, `release` and
  `thin-app` profiles.
- A `.gitignore` for `target/`, and a git repository of its own when the app
  is not created inside one already.

`--sdk-version` builds the app on the published SDK instead:
`ui = { package = "tilo-app", version = "0.1.0" }` from crates.io (or the
registry Cargo's source replacement names), with no `[patch]` and no seeded
lockfile. Without a value it requires the version the CLI was built with. The
toolchain, target flags and profiles are the same. Such an app always links
the toolkit statically (`--static`): the shared toolkit library comes from a
Tilo tree, not from crates.io. Three optional features need the workspace's
patched crates and do not build on the published SDK: `presentation-feedback`
and `dx12-shader-cache` (both on wgpu-hal) and `tilo-media`'s `pipewire`
feature on PipeWire headers newer than libspa-sys 0.10.1 handles (Fedora 44).
None is on by default.

Until the SDK is on crates.io, `tools/sdk/publish_check.py --verify` packages
and builds every SDK crate, and `tools/sdk/local_registry.py <dir>` turns the
result into a local registry an app builds against:

```bash
cargo build \
  --config 'source.crates-io.replace-with="tilo-sdk"' \
  --config 'source.tilo-sdk.local-registry="<dir>"'
```

Without `--id`, an app outside this repository is named `local.dev.<name>`.
That name cannot be published. Choose an id in a namespace you control before
you package the app.

### System libraries

`new`, `check`, `test`, `dev` and `package --platform linux` need no system
development packages. Wayland, X11 and xkbcommon are loaded when the app
starts, and fonts are found without fontconfig's C library. The first Cargo
command inside the app installs the compiler its `rust-toolchain.toml` pins,
through rustup. An app's tests lay out text, so `check` and `test` need one
installed font; every desktop has several, and on a bare machine or container
`check` names the package (`google-noto-sans-fonts` on Fedora,
`fonts-noto-core` on Debian and Ubuntu).

A thin Tilo OS package, which `tilo-app package` builds by default, links the
toolkit library the OS ships. That library has to be built with the OS's
feature set, which links GStreamer, so it needs GStreamer's development files.
`package` checks for them first and names the packages when they are missing:

```bash
sudo dnf install gstreamer1-devel gstreamer1-plugins-base-devel                # Fedora
sudo apt install libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev          # Debian, Ubuntu
```

Without them, `tilo-app package --platform linux` still builds a desktop
package, which links its own toolkit, and `tilo-app dev` restarts the app
after each build instead of reloading it in its windows. `--static` does not
help here: a static Tilo OS package lands far over the application size
limit. The CI that `tilo-app add ci` writes
builds in Tilo's builder image, which has them.

The CLI installs from git rather than crates.io. `cargo install --git` builds
it inside this workspace, so it gets the workspace's `[patch]` forks and, with
`--locked`, its tested lockfile. The CLI also embeds that lockfile and the
toolchain file for the apps it creates. A crates.io install would have none of
these, and the workspace crates it depends on are not published.

## The Tilo-native contract

Generated applications depend on `tilo-app` (normally under the concise Rust
name `ui`), never directly on the internal `ui-core` package. `tilo-app` is not
a reduced or declarative-only widget set: it re-exports the complete layout,
retained-widget, canvas, custom-painting, native-view and GPU surface API.
Applications control their content and composition, not the theme. Colours,
fonts, borders, shadows and motion come from theme tokens that the system and
the user own, so a user's theme applies to every app. The boundary exists so
tooling can prove that an application participates in Tilo actions,
collaboration, history, accessibility and lifecycle contracts. `ui-core` is
reserved for the shell and framework implementation.

### The quick path: a local effect or view

For a zero-argument control, keep ordinary closure ergonomics with
`Button::on_effect` or `Button::on_view`:

```rust
let refresh = ui::Button::new("refresh", "Refresh")
    .on_effect(move || refresh_from_service());

let inspector = ui::Button::new("toggle-inspector", "Toggle inspector")
    .on_view(move || inspector_state.toggle());
```

The button synthesizes and publishes a resource/action and invokes the closure
only through that action's operation handler. Search, scripts and agents can
discover and invoke it by stable ID.
Use `on_effect` for external work and `on_view` for transient presentation
state; use the domain path below for document mutations, parameters, explicit
contracts, or an action shared by several controls.

### The domain path: bind a declared action

An interactive control binds to the same action that search, scripts and
agents see:

```rust
let resource = document_resource(&model);
let save = ui::tilo_actions::ActionBinding::unit_for(&resource, "save")?;
let button = ui::Button::new("save", "Save").on_action(save);
```

`ActionBinding::unit_for` looks up the actual `ActionSpec` on the resource, so
the UI cannot silently drift to a parallel contract. Parameterized controls
use `ActionBinding::new` to map typed control values to JSON arguments, or
`static_json_for` for a fixed, strictly validated argument object.

The control emits only an inert typed intent. The app host re-resolves its
stable address against the fresh resource tree, applies current permissions
and confirmation policy, and enters the ordinary action door. It never
auto-confirms a destructive or phrase-confirmed operation; it opens the same
confirmation flow used by the command palette. Document actions use the shared
collaboration and undo-history executor. Effects and background jobs use the
same admitted/progress/terminal lifecycle as remote callers. Do not duplicate
the operation in `on_click`, a private command enum, or a second mutation path.

### Command palettes come from resources

`Widget::resources` is the source of truth for commands. Tilo recursively
flattens the live resource tree into native Ctrl+K rows, semantic actions and
the broker catalogue. An app supplies stable resource/action IDs, typed
parameters and contracts; it does not maintain a separate command-palette
list or callback table. If availability changes, return the updated resource
tree. The host resolves the selected address again immediately before
execution.

### Custom widgets draw with the system theme

An app can build its own widgets, down to fully custom GPU-accelerated drawing,
but ordinary tasks should use the toolkit widgets. The goal is a custom widget
that still looks native: build it from the system's primitives and draw
it with the theme's real colours, fonts, borders and shadows rather than
hard-coded values. The user stays in control of the theme.

Custom rendering follows the same contract without being forced into a stock
widget. A custom `Widget` can own `ActionEmitter<T>`, call `emit(value)` from
its pointer, keyboard or accessibility path, return queued intents from
`take_control_invocation`, and include `emitter.metadata(control_id)` in
`control_action_bindings`. It must also expose explicit retained or synthetic
accessibility semantics and route semantic activation to the same emitter.
Those semantics and declared resources are projected into the searchable,
agent-operable tree. Canvas geometry, native views, shaders and bespoke input
remain app-defined; the theme they draw with and the meaning and route of an
interaction do not.

### Certification requirements

Every scaffold includes a strict `Driver::native_app_audit()` assertion. It
checks the actual laid-out accessibility tree and resolves the actual control
inventory against the live resource tree. Legacy callbacks, direct button
document paths, stale bindings, unnamed/invalid semantics, and focusable
widgets hidden from accessibility make certification fail. The workspace
checker separately rejects direct core-UI dependencies and ratchets existing
legacy escape hatches downward, so new code cannot silently bypass the model.
Direct CRDT field controls remain a supported small-app primitive: the SDK
projects them as semantic actions and their writes automatically enter shared
history and collaboration.

Run the same gates explicitly from an app directory:

```bash
tilo-app check
tilo-app certify
tilo-app certify . --workspace --responsive-artifacts target/responsive-acceptance
```

`tilo-app check` runs structural checks, app-only formatting verification,
Clippy, Rust/generated accessibility tests, production-root native
certification, and audited saved scenarios. `tilo-app certify` is the focused
runtime proof against the production app root; from a checkout,
`tilo-app certify --workspace` certifies every graphical app. Certification is
an app boundary, not a warning budget: one legacy callback, stale binding,
missing semantic route, or invalid operation contract fails it.

The workspace responsive form builds the graphical fleet once, certifies each
production root, then captures every app at phone, tablet, laptop, and desktop
sizes in four modes: ordinary copy, expanded pseudo-localized copy, 200% text,
and high contrast. It also runs the native semantic/accessibility audit at each
of those sixteen app/mode/viewport combinations and saves the JSON beside the
images. A failure is attributed to the app, mode, and viewport; artifacts are
grouped by app instead of overwriting one global set. Each PNG must have the
requested dimensions and contain a materially painted frame; creating an empty
or effectively single-colour image is a failure, not a golden candidate. This
is the release-surface acceptance run—not a substitute for scenario-specific
interaction tests.

Every app using the shared `LaunchSpec` runner also gets a standard responsive
acceptance capture without app-specific argument parsing:

```bash
my-app --responsive-matrix target/responsive
my-app --responsive-matrix target/responsive-expanded --pseudo-locale
my-app --responsive-matrix target/responsive-large-type --text-scale 2
```

The matrix renders phone portrait (390×844), tablet (768×1024), laptop
(1366×768), and large desktop (1920×1080) images from the same scenario. The
pseudo locale lengthens translated strings and invalidates retained text layout,
so reviewers see clipped labels and fixed-width assumptions before catalogs are
complete. The text-scale pass exercises the supported 200% accessibility size.
Pseudo-localization is a layout audit mode, not a language presented to users.

`tilo-app check` is the copyable local/CI quality gate. It reports the complete
structural `doctor` result, verifies formatting for only the app package, runs
Clippy over every app target with warnings denied and dependencies excluded,
runs the app's Rust and generated accessibility tests, then executes every
saved scenario in isolated state with accessibility auditing enabled. Use
`--artifacts target/check` to retain each final semantic tree, target list,
document, and versioned audit report; `--no-scenarios` keeps the structural,
format, lint, and Rust-test phases for apps that deliberately have no saved
flows. Arguments after `--` are forwarded only to the Cargo test harness.
Pass `--fix` to format only the app package before the strict lint, test, and
scenario phases; dependency source and unrelated workspace packages are never
rewritten.

For serialized screens, that structural preflight crosses file boundaries: it
checks every component node against `src/components/mod.rs`, verifies each
registered module and graph factory still exists, and validates the generated
display/form factory's typed props with the screen path and node id in the
diagnostic. A customized router context is reported as app-owned and therefore
not statically verifiable; generated contexts fail early on missing factories.

With no name, `tilo-app new` runs as a small terminal wizard for the package
name, window title, application ID, and template. It presents the numbered
templates with their intended use; numbers and full names are accepted.
Invalid package names, IDs, and template choices are explained
and re-prompted without discarding earlier answers. Enter accepts a displayed
default. Before writing, the wizard shows the resolved app title, package,
application ID, template purpose, and destination for confirmation. Declining
or ending input cancels before any app files or workspace edits are published.

With a name and flags the command is deterministic and suitable for scripts;
omitted `--template` defaults to `starter` outside an interactive terminal:

```bash
tilo-app new --list-templates
tilo-app new errands \
  --id org.example.errands \
  --title "Errands" \
  --template collection
```

A template is a starting point you scaffold and then own, so there are only
four, each the least code that is a correct app of one shape:

| Template | Starts you with |
| --- | --- |
| `starter` | One resource, one action, one bound field and a derived label |
| `document` | One collaborative document: bound fields, a live preview, a destructive `clear` |
| `collection` | Items in a `Collection` the user adds, edits, checks off and removes, with item actions and an empty state |
| `graph` | Serialized, live-editable screens under `ui/` above controls bound to the resource |

Every template starts empty, reads every string from its own
`locales/en-US.ftl`, uses only shared widgets and theme values, and exposes each
capability as an action. Each ships with tests for its actions, its bound
controls, undo, permissions, persistence and the native-contract audit, plus a
saved headless scenario. Programs that teach one concept, such as virtualized
lists, background work or a responsive master-detail shell, are examples
rather than templates; see [Examples](examples.md).

After creation, the printed commands use shell-safe paths and include the
first test, development, run, saved-scenario, screenshot, inspection, and
package workflows.

Inside a Tilo checkout the tool finds the workspace `ui` crate and registers
the new package automatically. It writes the complete app into a private
sibling staging directory, then publishes that directory and the workspace
registration as one rollback-safe operation. A validation, write, or
registration failure therefore leaves neither a partial app nor a stray
workspace member; an explicitly supplied empty destination is restored with
its original permissions if publication fails. Outside a checkout, the tool
discovers `ui` from `TILO_UI_PATH`, the running `tilo-app` binary's source tree,
or the checkout it was built from. The interactive wizard shows that path as
an editable default, validates it before the review screen, and re-prompts on
a bad path. For a pinned or relocated toolkit, set `TILO_UI_PATH` or pass
`--ui-path /path/to/tilo/ui`; an explicit flag always wins.

Grow an existing app from any directory inside it with `add`:

```bash
tilo-app add
tilo-app add component profile-card --template form
tilo-app add component activity-card --mount main \
  --props '{"title":"Recent activity","detail":"Nothing pending"}'
tilo-app add component profile-form --template form --mount main \
  --props '{"value_path":"profile-name","enabled_path":"profile-enabled"}'
tilo-app add component project-tree --template tree
tilo-app add component activity-list --template list
tilo-app add component results-table --template table
tilo-app add component article-body --template text
tilo-app add screen account-settings --template settings --link-from main
tilo-app add screen new-project --template form --link-from account-settings
tilo-app add screen first-run --template empty-state
tilo-app add scenario settings-smoke --screen account-settings
tilo-app scenario run
tilo-app add locale de-DE
tilo-app locale sync
```

With no subcommand, `tilo-app add` opens a terminal wizard that discovers the
containing app and offers a screen, reusable Rust component, semantic scenario,
or locale. Screen setup includes its title, starter shape, and an optional
source route (defaulting to `main` when present). Component setup offers a
typed-props display, a typed-state form, a source-agnostic virtualized tree, a
sparse variable-height list, a model-backed table, or advanced text. The wizard
offers to place a display or form component directly into a generated graph
screen, including typed display props or form document-binding paths. It reviews
the resolved artifact before entering the same atomic publication path as the
explicit commands.
Scripts and CI should continue to use the fully specified subcommands.

`add component profile-card --template form` creates
`src/components/profile_card.rs`, maintains the generated
`src/components/mod.rs` registry, and adds one plain `pub mod components;` to
the app library. The generated state is explicit and cloneable, so app actions
or workers can retain it without reaching into the widget tree:

```rust
let profile = components::profile_card::ProfileCardState::default();
let view = components::profile_card::profile_card("settings-profile", &profile);
let value_for_save = profile.value.clone();
```

The instance ID prefixes every semantic control ID, allowing the same component
to appear more than once. Its text and checkbox use two-way `LocalState`
bindings, visible/accessibility labels share generated Fluent messages, and a
headless component test exercises both controls. In a generated graph app,
`--mount main` also creates a graph adapter whose `value_path` and
`enabled_path` bind both controls directly to the shared app document. Reuse a
path to keep multiple component instances or ordinary graph controls in sync;
use distinct paths for independent values. These edits therefore participate
in persistence, collaboration, and document undo without app-owned fan-out.
Without explicit `--props`, the paths default to `<component>-value` and
`<component>-enabled`; the wizard lets you choose both.
The `display` template instead generates a typed props struct plus a
heading/detail view.
Component source, registry, library wiring, and locale updates publish as one
rollback-safe batch; an app-owned registry or existing component is protected
unless replacement is explicit. Even with `--force`, changing a generated
component template is refused while an existing graph node has incompatible
props or the replacement has no graph factory; update or remove those nodes
first, so generation cannot leave the app unbuildable.

`add component project-tree --template tree` starts at the professional tree
boundary without choosing the app's data source. Its function accepts any
`TreeModel`—including a database, CRDT, remote-page cache, or `TreeNode`
adapter—and returns the concrete `TreeView<(), M>`. App code can therefore
attach its own retained row widgets and reorder policy instead of editing a
generated wrapper or giving up toolkit behavior:

```rust
let config = ProjectTreeConfig {
    expanded: vec![workspace_id],
    selection: restored_selection,
    ..ProjectTreeConfig::default()
};
let tree = project_tree("project-browser", database_model, &config)
    .with_row_delegate(project_row)
    .with_reorder(can_move_project, move_project);
```

The generated configuration also controls the localized accessible name,
stable-key multi-selection, viewport height, and the initial variable-row
estimate. `TreeView` continues to own sparse
virtualization, loading/error slots, keyboard navigation, selection across
collapsed branches, drag edge-scrolling and insertion previews, stable scroll
anchoring, model notifications, accessibility, and reduced-motion-aware
structural transitions. The embedded headless test proves that expansion,
stable-key selection, and an interactive button inside a custom row delegate
remain independent. The tree's accessible name is added to every Fluent
catalog in the same atomic generation batch.

`add component activity-list --template list` provides the equivalent reusable
boundary for flat collections. It accepts any `ListModel`, including models
that report hundreds of millions of logical rows while retaining only a page
cache. The required delegate receives `Ready`, `Loading`, and `Error` states,
and the required key encoder gives accessibility and headless tests a stable
row identity:

```rust
let list = activity_list(
    "activity-feed",
    paged_model,
    &ActivityListConfig::default(),
    activity_row,
    ActivityId::to_string,
)
.on_activate(open_activity)
.with_reorder(can_reorder_activity, reorder_activity);
```

The typed configuration restores `KeySelection`, including constant-space
select-all state, discarding keys no longer present in the model. It also
controls the localized accessible name, multi-selection, viewport height, and
initial row-height estimate. The returned `VirtualList`
keeps targeted insert/remove/move/data notifications, retained interactive row
widgets, measured variable heights, pixel and drag-to-scroll input, accelerated
momentum, edge auto-scroll during reorder, insertion previews, and stable
anchoring when rows change above the viewport. Its generated stress test uses a
100-million-row model without allocating its rows, verifies that fewer than 30
delegates are mounted, checks variable geometry and restored selection, and
activates a button inside a row independently of row selection.

`add component results-table --template table` completes the reusable
collection set. It accepts app-defined `ColumnDef<Row>` values and any
`TableModel<Row = Row>`, so ordinary text, custom-painted cells, and
interactive child widgets can be mixed without coupling the table to a data
source:

```rust
let table = results_table(
    "search-results",
    columns,
    remote_results,
    &ResultsTableConfig::default(),
)
.on_activate(open_result);
```

The generated configuration restores and validates stable-key selection and
controls the localized accessible name, selectable behavior, viewport height,
and initial row-height estimate. The returned `DataTable` retains column
sorting and resizing, loading/error row semantics, targeted asynchronous model
notifications, interactive cell widgets, variable row measurements, keyboard
and pointer selection, momentum and drag scrolling, and stable anchoring across
inserts, removals, moves, and remote sorts. Its generated test exposes 100
million logical rows from constant model storage, keeps the mounted semantic
window below 40 rows, restores selection, measures a non-default row height,
and independently activates a retained cell button.

`add component article-body --template text` generates a small reusable
boundary around the complete text-layout contract. `ArticleBodyProps` keeps
the Fluent message and `TextStyle` together, with a readable default that
enables kerning, tracking, line height, and automatic hyphenation. The ordinary
`article_body` function returns a concrete `Text`, so app code can continue to
use its normal state, accessibility, inspection, presence, and permission
APIs.

Two sibling constructors expose the less common paths without making ordinary
text complicated:

```rust
let article = article_body_in_region("article", &props, polygon);
let diagram_label = article_body_with_engine("label", &props, diagram_layout);
```

The first accepts any validated `TextFlowRegion` and uses the built-in
`PolygonTextLayout` for concave or convex flow and exact clipping. The second
accepts any `TextLayoutEngine`, replacing measurement and painting while
retaining the standard `Text` widget lifecycle. Replace the generated
`en-US` automatic hyphenation setting with the active content language, or use
`TextStyle::with_hyphenator` for an application dictionary. The generated
headless tests verify that both polygon and custom-engine layouts preserve
static-text accessibility and that the custom engine participates in layout.
The full style, hyphenation, flow-region, and engine contracts are documented
under [Text layout and typography](#text-layout-and-typography).

`screen` writes a validated object graph to
`ui/account-settings.json`, registers it in the generated `src/screens.rs`,
and prints the `tilo-app ui edit` command that opens it. The registry exposes a
typed Rust-name function for direct construction, a name-based lookup, and a
history-aware router over every registered screen:

```rust
let settings = screens::account_settings(&theme)?;
let selected = screens::get(route_name, &theme);
let app = screens::router("main", &theme)?;
```

The default `blank` template is a title and body. `form` starts with labeled
single- and multi-line inputs, an enabled checkbox, stable draft bindings, and
a primary save action. `settings` supplies labeled toggles, a checkbox, stable
preference bindings, and a save action. `empty-state` supplies explanatory
copy and a primary recovery action. All controls have stable IDs and accessible
names, so they are immediately addressable from headless tests and the UI
builder.

`--link-from main` also adds an `open-account-settings` button to the existing
source screen. It pushes the new route; generated non-main screens
include a Back button. The source screen edit, new graph, and regenerated Rust
registry use the same rollback-safe batch. When the app has an `en-US` catalog,
the screen's title, labels, placeholders, accessible names, and actions are
generated as namespaced Fluent messages; every existing translation receives
source-language placeholders without replacing translated values. A duplicate
button ID, invalid graph, catalog conflict, or malformed translation therefore
publishes none of the graph, routing, Rust, or localization edits.

The first screen added to an app also adds a parsed and validated
`pub mod screens;` declaration to `src/lib.rs`. A pre-existing app-owned
`src/screens.rs` is never overwritten. `add scenario settings-smoke --screen
account-settings` derives a semantic workflow from that graph: it follows the
direct navigation button on `main`, fills enabled text inputs, toggles enabled
checkboxes and switches, and activates app-action buttons. Screen-derived
scenarios require the live app root to use `screens::router("main", ...)`; the
Graph app template does this by default, so use these routed examples in an app
created with `--template graph`. The named screen must be directly
reachable from `main`, which keeps the generated setup deterministic. Omit
`--screen` when you want an empty action file to author by hand. In both cases
the artifact is written under `tests/scenarios/` and the printed next command
runs it with `tilo-app scenario run`.

`locale` writes a valid Fluent catalog under `locales/` whose complete set of
source messages, terms, attributes, variables, and starter values mirrors
`en-US`. Translate those
values in place; the catalog is atomically added to the generated
`src/locales.rs` registry, so system locale selection can use it on the next
build without a manual Rust edit.
Artifact names are lowercase kebab case and locale names use BCP-47-style tags.
Existing files are protected unless `--force` is explicit, and
`--dry-run` reports every file that would be created or updated and the next
command without writing. Multi-file screen generation snapshots existing
contents first and restores earlier publications if a later write fails.

After adding or changing messages in `en-US`, synchronize every translation—or
one named translation—without overwriting translated values:

```bash
tilo-app locale sync
tilo-app locale sync de-DE --dry-run
```

The command copies missing messages, terms, values, and attributes from the
source catalog. Existing translated patterns, stale units, and patterns with a
different variable set are preserved and reported. Use `--prune` to remove
stale structure. Use `--reset-variables` to replace only mismatched patterns
with their source-language versions; this deliberately loses the translation
for those patterns so the app cannot format them with an invalid argument
contract. When synchronizing multiple catalogs, every input is parsed and
validated before the atomic write batch starts.

Run a fast structural diagnosis without compiling the workspace when you want
the checks without starting Cargo:

```bash
tilo-app doctor reading-list
```

`doctor` finds the app from any descendant path, parses the Cargo manifest,
resolves its `ui` dependency, validates `tilo-app.toml` through the same package
schema used by installation, and checks the standard app-root and launch
lifecycle seams. The SDK's Cap'n Proto bindings are checked in, so building an
app never needs the `capnp` compiler. It validates every serialized screen with the renderer's
lightweight graph schema, parses every Fluent catalog, reconciles the generated
locale registry with files on disk, and checks saved headless scenarios with
the exact wire grammar used by the runtime driver. It reports every problem in
one pass. Translation catalogs are also compared with `en-US`: missing or stale
messages, terms, attributes, and changed variable sets are reported as contract
warnings while the source-locale fallback keeps the app runnable. Invalid
runtime artifacts and build/package wiring make the command fail; an
intentionally custom lifecycle or a broken optional scenario is a warning, so
it does not prevent an otherwise healthy app from running.

Use `dev` for the edit-build-run loop:

```bash
tilo-app dev
tilo-app dev apps/reading-list --app-only
```

It watches the app recursively and, when `ui` is a local path dependency, the
toolkit source as well. Editor save bursts are coalesced before Cargo builds a
candidate. The currently running app stays alive while that build runs. Only a
successful build replaces its complete process group with the reported app
executable; a compiler error or structural-preflight failure leaves the last
good window running and the next edit tries again. If no build has succeeded
yet, the watcher simply waits for a fix. Edits saved while Cargo is still
compiling supersede that candidate: `dev` coalesces the newly queued changes,
skips launching the stale executable, and builds the latest source. Restarts
and Ctrl-C terminate the
complete Cargo/app process group, so an earlier window cannot be stranded
beside the new one.
Use `--app-only` when toolkit edits should not restart this app, and
`--debounce-ms` to adjust the default 30 ms quiet period.
When the debug build is too slow to judge what you are changing (painting,
video, large documents), `tilo-app dev --release` builds the app with the
`thin-app` profile thin Tilo builds ship with: release optimisation without
LTO, so it still links quickly and reloads in place. `--profile NAME` picks any
Cargo profile; one other than `dev` or `thin-app` restarts the app on each
change instead, as an LTO profile cannot link the toolkit dynamically.
`scripts/agent-loop/session.py start APP --release` passes it on.
`tilo-app dev --platform web` runs the same loop in the browser
([The web](#the-web)).

Every template starts on the app framework (`ui::framework`): the app's
state is a `#[derive(Model)]` resource, every edit is an `#[action]` method in
its `#[resource]` impl, and `impl ResourceApp` supplies a view that binds
controls to those actions. `main` is one line, `ui::framework::run::<App>()`,
which provides the live window, durable record storage, sync and
collaboration, a History mode, per-author undo, permissions, the action
palette, agents, scripts, search, and the standard `--shots`,
`--headless-drive` and `--native-audit` modes. The app also starts with the
minimum sandbox declaration, a package manifest, and tests in `tests/app.rs`:
`Replica` tests the resource's actions without a window, and `TestApp` drives
the running app (controls, undo, a viewer's refused edit, a relaunch). Every
generated template also includes a native-contract test, so ordinary
`tilo-app test` fails on a control without an action, unlabeled controls,
invalid virtual-collection metadata, or unstable synthetic identity.
Interaction-specific states remain covered by `tilo-app scenario run --audit`.

`starter` binds a profile resource to a text field, checkbox, number field,
slider and dropdown (`cx.input`, `cx.checkbox`, and `cx.control` for any other
toolkit control); `data` begins with a selectable `DataTable` over a
100-million-row sparse model with a background page request, stale-result
suppression, targeted row notifications, and retained cell buttons that open
rows through the resource (`cx.actions()` binds controls a table, list or tree
builds on demand); `tree` begins with a stable-id hierarchy, keyboard and
multi-selection behavior, accessibility semantics, a retained row action that
pins nodes in the resource, bounded virtualization, safe subtree reparenting,
and structural motion already supplied by `TreeView`; `list` begins with a
sparse 100-million-item `ListModel`, background page loading, explicit
loading/error delegates, measured variable-height delegates, stable-key
multi-selection, and an independently interactive button inside each mounted
row; `graph` emits a model-bound `ui/main.json` screen, editable without
rewriting the app, above native controls bound to the resource.
The `async` starter keeps its input (the folder to scan) in the resource and
runs the scan as an effect action with `LatestTask` for automatic event-loop
wakeup, cancellation, and stale-result suppression, plus
`ModelView::from_state_by` for keyed loading, success, and failure subtrees.
The `qr` starter demonstrates a compact generated-visual composition path: an
IME-capable input invokes `set-payload`, `cx.value` follows the payload,
`ModelView::from_state` replaces only the generated `QrWidget`, and
payload-specific alternative text makes the output visible to screen readers,
inspection, and headless scenarios.
The `document` starter is the collaborative-text case: title and body are
Automerge text, concurrent typing merges, the live preview follows every
edit, and `clear` is a destructive action that asks for confirmation.

Every ordinary `LaunchSpec` also reuses that live root for a screenshot; no
second screenshot-only tree is required. `shot` writes `preview.png` in the
app root by default:

```bash
tilo-app shot --viewport 1280x800
tilo-app shot --output artifacts/empty.png --scenario empty
```

Arguments after `--` are forwarded as app-defined screenshot options. A
capture is the production view at the requested viewport and active shot
theme; a scenario stages it through the resource's actions and its views'
stage hooks.

Build an installable package from the same app root with:

```bash
tilo-app package
tilo-app package --identity publisher.json
tilo-app package --output dist/reading-list.tapp --force
```

`package` runs the structural checks, builds every entrypoint declared in
`tilo-app.toml` thin, places the resulting binaries at their declared payload
paths, and signs and verifies the bundle itself; no separate packer is needed.
A thin build uses the app's `thin` feature and the `thin-app` profile, and links
against the toolkit and `libstd` that Tilo OS ships, so a typical app is well
under a megabyte. `--static` links them in instead, which is useful for a local
experiment but lands far over the application size limit.
The verified result is published as `dist/<cargo-package>.tapp` by default.
An existing output is never replaced unless `--force` is present, and even
then replacement happens only after the new package verifies successfully.

Files under an app-local `payload/` directory are included before the built
entrypoints are overlaid. Symbolic links are rejected so package contents do
not depend on files outside the app. Without `--identity`, `package` signs with an
ephemeral key, which is useful for local testing; use one
persistent identity for updates and publication. `--profile`, `--target`,
`--features`, and `--no-default-features` expose the corresponding Cargo build
choices.

Every package run prints where the bytes go: each payload file, largest first,
with entrypoint binaries split into code, read-only data, dynamic-linking
tables and the rest, then the signed package against the 8 MiB application
limit. An app can hold itself to a tighter ceiling in `tilo-app.toml`:

```toml
[budget]
package = "1 MiB"   # the signed .tapp
binary = "900 KiB"  # each entrypoint binary
```

Sizes are byte counts or strings in B, KB, MB (powers of ten), KiB or MiB
(powers of two); an unknown key is an error, so a misspelt ceiling cannot
quietly stop applying. The budget is a build-time check, not package metadata.
A package over budget fails with the amount it is over, and the previous output
is left in place. `--json` carries the same breakdown under `size`.

### Other platforms and architectures

`--arch x86_64|aarch64` picks the CPU (the host's by default) and
`--platform` the system:

```bash
tilo-app package --arch aarch64                      # Tilo OS, thin
tilo-app package --platform linux --arch x86_64      # dist/<pkg>-linux-x86_64.tapp
tilo-app package --platform macos --arch aarch64     # dist/<pkg>-macos-aarch64.tapp
tilo-app package --platform windows --arch x86_64    # dist/<pkg>-windows-x86_64.tapp
tilo-app package --platform android                  # dist/<pkg>-android-{aarch64,x86_64}.tapp, .apk, .aab
```

Other Linux desktops, Windows and macOS ship no Tilo toolkit, so a build for
them is one static executable (`release` profile, `<arch>-unknown-linux-gnu`,
`<arch>-pc-windows-msvc` or `<arch>-apple-darwin`) with the toolkit linked in,
packed as `bin/<name>` (or
`bin/<name>.exe`). Its signed `[target]` names the platform, the architecture
and the toolkit release it links, which is `[package].runtime`, so the
manifest must declare one (`tilo-app new` does). Only the GUI entrypoint ships.
There, collaboration comes from the per-user `tilo-runtime` service when it is
running, as for Tilo's own apps on those systems. `[budget]` applies to the
thin Tilo OS build only; a desktop or Android build answers to the registry's
64 MiB limit instead, and the size report measures it against that. A
static build of a starter app is about 40 MB.

Build on the system you target, or with a cross toolchain Cargo can drive;
`--target` overrides the triple (for example `x86_64-pc-windows-gnu`).

`build`, `run`, `dev` and `check` take the same `--platform` and `--arch`:

```bash
tilo-app build --platform windows --arch aarch64 -o out  # compile, copy the .exe to out/
tilo-app run --platform web                              # build once and serve the page
tilo-app dev --platform linux                            # the reload loop, on this machine
tilo-app check --platform all                            # lint every other target too
```

`build` compiles exactly what `package` would, without signing a package.
`run` and `dev` refuse a platform or architecture this machine cannot execute
and say where to build it instead; for `android` they install on a device
instead ([Android](#android)). `check --platform` (repeatable, or `all`)
adds, after the host's checks, Clippy for each other platform's target triple
with warnings denied, and a full web build for `web`; the Rust target must be
installed (`rustup target add`).

A Tilo OS package declares only the architecture it was built for, at the
level `[package.isa]` names for it (`tilo-app new` declares `v3` and
`aarch64`).

### The web

In the browser the runtime is one WebAssembly module, and `wasm32` has no
dynamic linking, so an app's web build is the runtime bundle with the app
linked in and registered:

```bash
tilo-app build --platform web      # <target>/tilo-app/web/<pkg>/bundle, index.html?app=<id>
tilo-app dev --platform web        # serve it, rebuild and reload on save
tilo-app package --target web      # the bundle in the immutable layout Cloud serves
```

Nothing in the app is web-specific: the build registers the `definition()`
that `ui::dev_reload_entry!` defines, as component apps export theirs. A
small generated crate under the target directory depends on the runtime from
the SDK checkout the app builds against (`crates/browser/tilo-browser-runtime`,
see its `WEB_RUNTIME.md`) and on the app, and builds into the app
workspace's target directory, so the runtime compiles once and later builds
relink only what changed. `build --target web --bundle app` is the same
build; Tilo's own apps also build into the shared desktop bundle
(`--bundle desktop`).

The core the build links exports what the app imports. The core the registry
serves is built for Tilo's own web apps, so a package that imports something
only this app uses (a generic instantiation from `serde_json`, say) can fail
to link in a page that runs that core; the page names the missing symbols.

`dev --platform web` serves the page and rebuilds after every save. An edit to
the app relinks only its module; the page notices the finished build and
reloads into the same record. The record's state lives in the browser's
storage, not in the module, so the document survives the new code. An edit
to the SDK rebuilds the core too. `--bind` picks another address.

The build needs wasm-bindgen 0.2.108, binaryen's `wasm-opt`, lld's `wasm-ld`,
Node.js and brotli, which the builder image `tilo-app add ci` uses has.
`TILO_WEB_BUILD_RUNNER` runs the build under a command prefix instead, for
example a wrapper that runs it on a remote machine and brings `target/web`
back. A web package's `[budget]` applies to its module as it
does to a thin binary.

### Android

The same app builds as a standalone Android app, with nothing Android-specific
in its code or its `Cargo.toml`:

```bash
tilo-app doctor --platform android   # what the Android toolchain lacks
tilo-app setup android               # install the SDK side of it
tilo-app run --platform android      # install on the connected device, stream its log
tilo-app dev --platform android      # rebuild, reinstall and restart on every save
tilo-app package --platform android  # dist/<pkg>-android-<arch>.{tapp,apk,aab}, both arches
```

`--arch aarch64|x86_64` picks the ABI (`run` and `dev` default to the
device's), and `--device` the `adb` serial. The application id, launcher
label, `versionCode`, icon (`icon.png`) and permissions come from
`tilo-app.toml`; debug builds use the per-user debug key and `package` a
release key. [Android](android.md) has the mapping, signing, the toolchain and
what the Android host does not provide.

### Publishing

`tilo-app keys new`, `tilo-app apply` and `tilo-app publish` take a package to
the store; [Publish a Tilo app](publishing.md) walks through them.

### Linux, macOS and the web from one source

`run`, `build` and `package` take `--target linux|macos|web`. The app source
does not change: `tilo-app` selects the runner and the platform crates select
each backend. Linux and macOS artifacts build on their own OS; the web builds
anywhere.

```bash
tilo-app run --target macos            # a signed development .app, launched
tilo-app run --target web              # serve, rebuild on change, reload the page
tilo-app build --target linux          # target/tilo-app/linux/<app>/<bin>
tilo-app package --target linux        # a signed .tapp (thin against the shared runtime in this repository)
tilo-app package --target macos        # a zipped .app; Developer ID + notarization with TILO_MAC_* / TILO_NOTARY_* set
tilo-app package --target web          # dist/web/<app-id>/versions/<digest>/
```

`tilo-app.toml` narrows the targets with `platforms = ["linux", "macos"]` and a
`platform-reason`; the default is all three. `tilo-app check --target <t>` runs
the portability and capability checks for a target on any host. In an app that
keeps all three targets, platform code (a target `cfg`, `std::os`, a platform
path or OS tool, a thread or clock that panics in a page) is an error, not a
baseline entry: move it into the platform crates under `crates/platform/`
(`tilo-paths`, `tilo-files`, `tilo-process`, `tilo-net`, …), which select a
backend per target, and call them. See
[`app-model/cross-target.md`](app-model/cross-target.md) for the service table.
For `package`, a `--target` value other than `linux`, `macos` or `web` is a
Rust target triple.

### Files, preferences and secrets

Everything an app keeps outside its documents goes through `ui::storage`, the
same API on every target. With a `HostContext` use its methods; they are scoped
to the app the host prepared.

| What | Call | Backed by |
|---|---|---|
| Documents (collaborative) | resources and records; `ui::persist` for a one-document app | Automerge records |
| App files: imports, caches, databases | `cx.files().data()` / `.config()` / `.cache()` / `.state()` → `tilo_files::Dir` (`ui::storage::AppFiles::for_app(id)` without a context) | `tilo_paths::AppPaths`: per-user directories natively, the origin-private file system in a page |
| Preferences | `cx.settings()` (the app's section) or `ui::storage::Settings::section("locale")` for a shared one: `load`, `bool`, `string`, `update` | the desktop settings document (`settings-store`), or the host's source |
| Secrets | `cx.secrets()` (`ui::storage::secrets(id)`) | `tilo-secrets`: Secret Service, Keychain, wrapped IndexedDB |

Apps do not depend on `settings-store` or `app-state`: those are the SDK's
internals. The exceptions are
the Settings app, which owns the settings document, and Notes' capture
fault-injection scenario, which locates its record on disk. Desktop-only code
that must name a native path uses `tilo_paths` and `tilo_files::native`
(`native::private` for owner-only files such as keys).

### Capabilities

`[capabilities]` in `tilo-app.toml` lists every capability the code uses, as
`required` (the app cannot work without it; every declared target must provide
it) or `optional` (the app degrades where it is missing):

```toml
[capabilities]
required = ["network.http"]
optional = ["files.open", "app-files", "clipboard"]
```

`tilo-app check`, `build`, `run` and `package` fail before compiling when the
code uses one the manifest does not list, naming the site and the fix
(`tilo-app add capability <name>`); the policy gate
(`tools/policy/check_app_capabilities.py`) enforces the same table,
`tools/policy/app_capability_usage.toml`. macOS packaging derives entitlements
and usage strings from the list.

## The app shape

Keep the binary boring:

```rust
fn main() {
    ui::framework::run::<reading_list::App>();
}
```

The library declares the resource, its actions, and the view:

```rust
use ui::prelude::*;

#[derive(Model, Default)]
pub struct List {
    pub title: String,
}

#[resource(kind = "reading.list", label = "Reading list")]
impl List {
    /// Rename the list.
    #[action]
    pub fn rename(&mut self, title: String) {
        self.title = title;
    }
}

pub struct App;

impl ResourceApp for App {
    type Resource = List;
    const ID: &'static str = "org.example.reading-list";
    const TITLE: &'static str = "Reading list";

    fn view(cx: &mut ViewCx<List>) -> Element {
        Box::new(ui::column![
            cx.input(|list| list.title.clone(), List::RENAME).with_id("title"),
        ])
    }
}
```

The same declaration is shared by the real event loop, screenshots, the
headless driver and `TestApp`. Use toolkit widgets for controls so focus,
keyboard input, IME positioning, accessibility, theme behavior, and testing
metadata remain one implementation rather than app-local replicas. A custom
widget takes part as a surface (`ResourceSurface`) of the app's resource;
every launch is the framework app's.

Compose ordinary heterogeneous children with the layout macros instead of
writing one allocation step per child:

```rust
use ui::tokens::TextRole;
use ui::widget::{Button, Checkbox, Text, TextInput, Widget, labeled_check, labeled_field};

pub fn app_root() -> Box<dyn Widget> {
    Box::new(ui::column![
        Text::new("projects-title").with_role(TextRole::Title),
        labeled_field(
            "project-name-label",
            TextInput::local().with_id("project-name"),
        ),
        labeled_check(
            "project-pinned-label",
            Checkbox::new("project-pinned", "project-pinned"),
        ),
        ui::row![
            Button::new("cancel", "cancel-label"),
            Button::new("save", "save-label").role(ButtonRole::Primary),
        ],
    ])
}
```

`column!`, `row!`, and `stack!` return the normal concrete `Column`, `Row`,
and `Stack`, not a second UI language. Their standard modifiers remain
chainable (`ui::column![...].with_gap(12.0)`), and every child keeps the same
retained lifecycle, focus, accessibility, and inspection behavior. Use the
ordinary `push` builder when adding an already type-erased
`Box<dyn Widget>` conditionally.

`labeled_field(label, control)` is the corresponding vertical form primitive
for text, number, choice, date, time, colour, checkbox, toggle, and slider
controls. `labeled_check(label, control)` provides the conventional inline
checkbox/toggle layout. In both cases the label is rendered once as semantic
`Label` text and the same Fluent key becomes the control's accessible name, so
translation changes and refactors cannot leave visible and spoken labels out
of sync. They return ordinary `Column` and `Row` widgets, which can be nested
or further styled like any other layout:

```rust
let email = labeled_field(
    "account-email-label",
    TextInput::local()
        .with_id("account-email")
        .with_placeholder("account-email-placeholder"),
);
```

### Pointer, touch, and keyboard

Declare a semantic control once. Pointer, direct-touch tap, keyboard,
accessibility, Ctrl+K and headless activation all reach the same action intent:

```rust
let save = tilo_actions::ActionBinding::unit_for(&document, "save")?;
Button::new("save", "app-save")
    .on_action(save)
    .role(ButtonRole::Primary)
```

The host detects the current `InteractionEnvironment` and propagates it through
the retained tree. Buttons, checkboxes, toggles, sliders, text inputs, and
number inputs expose at least a 32 logical-pixel fine-pointer target and a 48px
coarse-touch target while keeping compact glyphs centred inside that hit area.
Virtual table, tree, and list rows retain their app-selected compact density
for a fine pointer, but impose a reversible 48px geometry floor for touch;
authored variable-height measurements are preserved when the device switches
back. Touch also suppresses hover-only affordances such as tooltips.

A tap is not emitted as a pointer press until touch-end. Moving beyond the tap
slop or adding a second finger cancels it, so beginning a scroll or navigation
gesture cannot toggle a pointer-down control. A one-finger drag emits
pixel-precise scroll deltas through the normal scrolling contract. On a hybrid
device, using the keyboard changes the last input modality but deliberately
keeps the coarse-pointer capability, so targets do not jump smaller after a
shortcut.

App code normally does not inspect this environment. A custom control that
needs adaptive geometry implements `Widget::set_interaction_environment` and
stores the supplied value; retained containers get recursive propagation from
the trait default. Platform shells and tests can explicitly set an environment
with `App::set_interaction_environment` or
`Driver::set_interaction_environment`. The headless driver also provides
`tap_at`, `tap_named`, and `swipe_at` for direct-touch scenarios.

### Platform conventions and shortcuts

Describe shortcuts and dialog actions by intent instead of branching on the
operating system. The host propagates `PlatformConventions` through the widget
tree: the command modifier is Command on macOS and Control elsewhere, shortcut
labels use the corresponding native notation, and modal primary/secondary
buttons follow native ordering.

```rust
use ui::{CommandEntry, ModalAction, ModalSpec, Shortcut};

let find = CommandEntry::new("find", "Find")
    .shortcut(Shortcut::command("F"));
let actions = [
    ModalAction::secondary("Cancel"),
    ModalAction::primary("Save"),
];
let modal = ModalSpec::new("Save changes?").actions(&actions);
```

`SearchField::opens_palette` uses this contract for its command-search chip
and accessible name. At the application layer, command search, undo, redo, and
range-control command gestures match the same semantic modifier. `ModalDialog`
also treats Enter as the primary action unless its content consumes Enter—for
example, a multiline editor can keep the key without accidentally submitting
the dialog.

App code normally uses the detected conventions. Native shells and tests can
override them with `App::set_platform_conventions`,
`Driver::set_platform_conventions`, or
`ModalSpec::platform_conventions`; this makes platform-specific geometry and
accessible labels deterministic without compiling on each target OS.

### Native platform views

Use `NativeView` when a platform component must remain a real child view—for
example an HWND, NSView, GTK widget, camera preview, or vendor control—rather
than being copied into a texture. The backend is created with ordinary app
code and the result remains a normal retained widget:

```rust
use ui::{NativeView, NativeViewBackend};
use ui::taffy::Dimension;

let account_picker = NativeView::new(
    "System account picker",
    AccountPickerBackend::new(),
)
.with_test_id("account-picker")
.with_size(Dimension::percent(1.0), Dimension::length(320.0))
.with_fallback(Box::new(portable_account_picker()));
```

Implement `NativeViewBackend::mount` by creating the child from
`NativeViewHost::window`. The host calls it only after the live parent window
exists and before that window is shown. The backend then receives physical and
logical geometry, scale and host changes, native platform conventions, focus,
enabled state, per-frame visibility, and guaranteed teardown while the parent
still exists. Native APIs that own their event queue can ignore
`handle_event`; framebuffer-style or manually dispatched APIs can consume the
normalized local pointer, modifier, and keyboard events supplied there.

Use `NativeView::platform_accessible` when the child publishes its own native
accessibility subtree. `NativeView::new` instead exports a named AccessKit
wrapper, which is appropriate for an opaque surface or a backend whose internal
semantics are exposed through toolkit children. This choice is explicit so the
same control is never announced twice.

Native child windows cannot be captured honestly by the Vello headless
renderer. `with_fallback` supplies the toolkit equivalent used by screenshots,
headless scenarios, remote/WASM builds, and mount failures; it receives the
same state synchronization, layout, input, localization, and accessibility
passes. Without a fallback, headless output shows a bordered diagnostic
placeholder and a mount error remains available through `mount_error`.

### Serialized UI object graphs

`UiDocument` is the versioned source format for a visual builder or an app
whose composition should be data. It compiles to the same retained widgets as
Rust construction; it does not introduce a second renderer or event model.

```json
{
  "version": 1,
  "root": {
    "type": "column",
    "id": "root",
    "gap": 8,
    "children": [
      {
        "type": "text",
        "id": "title-view",
        "text": { "source": "document", "path": "title" },
        "role": "title"
      },
      {
        "type": "text-input",
        "id": "title-input",
        "label": "Title",
        "placeholder": "Untitled",
        "value_path": "title"
      }
    ]
  }
}
```

For text inputs, checkboxes, and toggles, `label` renders semantic label text
and names the control in the accessibility tree. Text fields use a vertical
form layout; boolean controls use the conventional inline layout. It is
deliberately one property: changing locale or editing a screen cannot make the
visible and spoken labels disagree. Use `accessible_name` only for an
intentionally unlabeled control; validation rejects specifying both.

Start an app on this format directly, or add a graph to an existing app:

```bash
tilo-app new project-maker --template graph
tilo-app ui init ui/settings.json --title "Settings"
tilo-app ui edit ui/settings.json
tilo-app ui tree ui/settings.json
tilo-app ui validate ui/settings.json
```

`ui edit` opens the native builder. Select a node in the hierarchy and the
inspector exposes every serialized property for that node: stable ids;
dimensions, gaps, and layout flags; text source, role, and size; control labels,
accessible names, tooltips, model bindings, and enabled state; and button style
and navigation mode. Optional values can be cleared. Add any schema widget
type, reorder or delete the selection, then save back to the graph file. Each
property edit validates a complete candidate graph before publication, so a
bad number, duplicate id, empty required binding, or inaccessible control is
reported in the inspector without partially changing the preview or saved
document. Renaming a selected node keeps that node selected under its new id.

App-owned reusable Rust components participate in the same object graph through
a component node. The graph keeps only a stable factory name and serializable
props:

```json
{
  "type": "component",
  "id": "account-profile",
  "name": "profile-card",
  "props": {
    "title": "Ada",
    "detail": "Workspace owner"
  }
}
```

For typed display and form components, generate both the Rust component and its
validated graph adapter, then insert an instance without writing JSON by hand:

```bash
tilo-app add component profile-card --mount main \
  --props '{"title":"Ada","detail":"Workspace owner"}'
tilo-app add component profile-form --template form --mount main \
  --props '{"value_path":"profile-name","enabled_path":"profile-enabled"}'
```

That one command generates the typed Rust component, updates its module and
factory registries, migrates an untouched older generated graph root when
needed, and appends the first instance to `ui/main.json`. All candidate files
validate before publication; any failure leaves the component, registry, app
root, and screen byte-for-byte as they were. Model-backed tree, list, and table
templates still require an app-supplied model in Rust, and advanced text accepts
Rust-native style/engine values, so generate those templates without `--mount`.

For an existing component, or an app-owned router context, insert only the node:

```bash
tilo-app ui component ui/main.json profile-card \
  --props '{"title":"Ada","detail":"Workspace owner"}'
```

`ui component` appends to `root` by default, derives a unique id
(`profile-card`, `profile-card-2`, ...), and atomically rejects a bad factory
name, non-object props, invalid parent, duplicate explicit id, or out-of-range
index. Use `--id`, `--parent`, and `--index` when placement must be explicit.
Apps created with `--template graph` pass the generated
`components::ui_graph_context()` to their screen router. Adding the component
with `--mount` updates that registry and the screen in the same atomic
publication, so it is the complete wiring path—there is no app-root edit:

```rust
let app = screens::router_with(
    "main",
    &Theme::default(),
    components::ui_graph_context(),
)?;
```

The generated display `from_ui_graph` function converts `title` and `detail`
into the same typed `ProfileCardProps` used by ordinary Rust callers. The form
adapter validates non-empty `value_path` and `enabled_path` strings, then builds
the controls with the same document bindings used by built-in graph inputs and
checkboxes. Both return precise field errors, and `tilo-app doctor` checks those
generated prop contracts before compilation. Already-generated v1 form adapters
and their snapshot props remain recognized by `doctor`; regenerating a form uses
document bindings. Older generated graph roots are migrated when their original
router call is still intact; customized
app roots are never rewritten. For a custom component shape, write the
equivalent small function from `UiComponentRequest` to `Element` and register it
with `UiBuildContext::with_component` or extend the app-owned context explicitly.

The factory returns an ordinary retained widget, so its child controls, model
bindings, focus, accessibility, inspection, and headless scenarios work exactly
as they do when the component is constructed directly in Rust. Keep the graph
node id on the returned component's semantic root when the component itself is
an addressable test target. Missing factories and invalid props are explicit
construction errors containing the node id and component name; production never
silently substitutes a placeholder.

The standalone builder cannot load application code, so it displays a named,
accessible placeholder while still allowing the component name and JSON props
to be edited, moved, saved, undone, and inspected. The app replaces that
placeholder through its `UiBuildContext` at runtime. Component names use the
same lowercase kebab-case convention as `tilo-app add component`.

The center preview is the real `LiveUi` widget tree, so focus, keyboard input,
accessibility, bindings, layout, and theme behavior do not diverge from the
app. In a source checkout, `tilo-app` builds the workspace builder on first use.
Installed tools look for `tilo-ui-builder` beside `tilo-app` and then on `PATH`;
set `TILO_UI_BUILDER` to override that executable.

The builder's Undo and Redo controls operate on history owned by the shared
`UiGraphHandle`, not on a private copy of the editor. Every successful property
or structural candidate is validated before publication; rejected and no-op
edits do not affect history. Consecutive keystrokes in one property publish live
but share one undo baseline. Selecting another node, changing another property,
performing a structural edit, or saving finishes that group. This also keeps a
node-id field live while its own lookup identity changes. Ctrl+Z and
Ctrl+Shift+Z/Ctrl+Y use this local history while focus is inside the builder;
elsewhere they retain the app document's collaboration-safe undo behavior.

Undo and redo publish atomically to every live preview or runtime inspector
using that handle, and a new edit after undo discards the abandoned redo path.
The history retains the latest 100 baselines. App-owned inspectors can call
`can_undo`, `undo`, `can_redo`, and `redo` directly. Use
`update_node_grouped` for continuous typing or dragging and call
`finish_edit_group` when that interaction ends; call `clear_history` after
loading or saving when that document should become a new history boundary.

Structural commands accept either an inline node object or `--node-file`:

```bash
tilo-app ui insert ui/settings.json --parent root --index 1 \
  --node '{"type":"button","id":"save","label":"Save","style":"primary"}'
tilo-app ui move ui/settings.json --id save --parent actions --index 0
tilo-app ui replace ui/settings.json --id save --node-file nodes/publish.json
tilo-app ui remove ui/settings.json --id obsolete-help
tilo-app ui set ui/settings.json --id title --property text.value --value "Account settings"
tilo-app ui set ui/settings.json --id panel --property fill_height --value true
tilo-app ui set ui/settings.json --id title --property size --unset
tilo-app ui apply ui/settings.json ui/settings.edits.json
tilo-app ui format ui/settings.json
```

`ui set` changes one serialized property without copying and replacing the
complete node. Property names follow the JSON schema and dotted paths reach
nested objects such as `text.value` and `navigate.route`. Values are parsed as
JSON first, so numbers, booleans, `null`, arrays, and objects retain their
types; ordinary unquoted text remains a string. `--unset` restores the schema
default. Every edit rebuilds and validates the affected node, then validates
the complete graph before the original file is atomically replaced. A bad
property name, type, duplicate ID, invalid binding, or accessibility regression
therefore leaves the file byte-for-byte unchanged.

Use `ui apply` when a generator, migration, or coding agent needs several
coordinated edits. The edit file is a reviewable JSON array using the same
operations as the individual commands:

```json
[
  {
    "op": "set-property",
    "id": "title",
    "property": "text.value",
    "value": "Account settings"
  },
  {
    "op": "insert",
    "parent": "actions",
    "index": 0,
    "node": {
      "type": "button",
      "id": "save",
      "label": "settings-save",
      "style": "primary"
    }
  },
  {
    "op": "move",
    "id": "cancel",
    "parent": "actions",
    "index": 1
  }
]
```

Supported operation tags are `insert`, `remove`, `replace`, `move`,
`set-property`, and `unset-property`. All operations run against one in-memory
candidate and the source file is published once, after the last edit succeeds.
If edit 8 of 10 fails, none of the first seven edits reach disk and the error
identifies edit 8.

Every command parses through the same lightweight `tilo-ui-graph` schema used
by the renderer. Property, batch, insert, remove, replace, move, and formatting
edits write through a same-directory temporary file only after the complete
candidate validates; an invalid id, duplicate subtree, inaccessible control,
illegal reparent, or bad binding leaves the source file byte-for-byte unchanged.

Load it once and keep the handle when an inspector or builder should edit the
running tree:

```rust
let graph = UiGraphHandle::from_json(include_str!("../ui.json"))?;
let root: Box<dyn Widget> = Box::new(graph.widget(&theme)?);

let selected = graph.node("save")?;

graph.update_node("save", |node| {
    if let UiNode::Button { label, role, .. } = node {
        *label = "Publish".into();
        *role = UiButtonRole::Destructive;
    }
})?;

graph.insert_child("root", 1, help_text)?;
graph.move_node("save", "actions", 0)?;
let removed = graph.remove_node("obsolete-help")?;

std::fs::write("ui.json", graph.to_json_pretty()?)?;
```

Every id is unique and stable. Property and structural updates clone and
validate the complete candidate, then publish it atomically; a duplicate id,
invalid dimension, empty binding, inaccessible control, unsupported version,
illegal container edit, cyclic move, or concurrent edit leaves the live tree
unchanged. `LiveUi` installs the changed subtree during the next
synchronization pass. The serialized controls retain normal model bindings,
localization, accessibility, keyboard/IME behavior, semantic inspection, and
headless actions because they instantiate `Text`, `TextInput`, `Button`,
`Checkbox`, `Toggle`, `Row`, `Column`, and `Container` directly.

### Screen navigation

Graph buttons express navigation as data. Push and replace name a registered
screen; back and forward operate on the router's browser-style history:

```json
{
  "type": "button",
  "id": "open-settings",
  "label": "Settings",
  "navigate": { "action": "push", "route": "settings" }
}
```

Use `screens::router("main", &theme)` in a generated graph app. A push clears
the abandoned forward branch, replace changes the current entry without
growing back history, and reset starts a new history at one registered route.
Back and forward buttons, keyboard or host navigation gestures, and app code
all drive the same `RouteHandle`. Unknown destinations are rejected by the
handle; a graph button whose destination is not in that router is visible but
disabled. If a screen cannot be built, the router keeps the last valid screen
and exposes the build error instead of leaving navigation partially changed.

Routes are transient presentation state. They do not write to the Automerge
document, appear in collaborative changes, or create undo entries. On a route
transition the router replaces one retained subtree, marks structure dirty,
and asks an app configured with `autofocus_on_structure_change` to focus the
new screen's first focusable control. Generated apps enable initial and
transition autofocus in their `LaunchSpec`.

State has two deliberate levels:

- durable/shareable model state lives in the Automerge document passed to
  `Widget::sync` and `Widget::handle_event`; it gains merge, collaboration,
  persistence, and undo semantics;
- transient presentation state such as hover, an open popover, or a scroll
  offset lives on the widget and never enters the shared document.

The storage boundary stays visible in types: durable fields name a document
path, while process-local fields bind an explicit typed handle. Reusable
operations are declared once as resource actions and controls use
`ActionBinding`, so a button, command palette entry, script, agent, and undo
history all reach the same implementation.

#### State, files, records, and open apps

An open app is a projection over state, never the authority boundary itself.
Opening a second window must not mint a second copy of a file or silently turn
selection/scroll position into shared content. Choose storage and permission
scope from the identity of the thing being changed:

| State | Canonical home | Identity / permission scope | Replication |
|---|---|---|---|
| Package preference or account configuration shared with collaborators | package `state.automerge` through `ui::persist` (device-local preferences use `cx.settings()`) | application package id / `Scope::App` | replicated: in a shared workspace the package document is the app's collaboration route, one document per app and room. Keep person- or device-scoped settings out of it |
| Collaborative document, note, annotation set, or other artifact | one Automerge document per room record | room-scoped `ui::RecordId`; `Scope::ResourceInstance` for artifact authority | record route in its home room |
| Collaborative filesystem entry | filesystem catalogue/content store | rename-stable `FileRecordId`; `Scope::FsRecord` | follows rename and move |
| Host filesystem access | the host filesystem | component-aligned path; `Scope::FsPath` | never implies record authority and never matches CRDT operations |
| Open tab/window presentation | retained widget fields and the structural continuation envelope | app instance/tab id | local; a privacy-filtered continuation may restore presentation only |
| Cursor, selection, hover, open menu, scroll, playhead | retained widget state or transient presence | semantic control/resource id where another peer needs to see it | not a document action; presence is ephemeral |

`ui::RecordId::for_file(path)` is deliberately path-bearing local document
identity. It is suitable while a local editor parks one document per path, but
it must not leave the device, key a collaborator permission, or promise to
survive rename. The collaborative filesystem's `FileRecordId` is the stable
identity for those jobs; the shell passes it in a typed launch intent and uses
`FsRecord` for authority. `FsPath` is only a host-filesystem enforcement scope.
These names are not interchangeable merely because all three may refer to the
same visible file at one moment.

Keep secrets, absolute paths, clipboard/IME contents, and widget `ephemeral`
strings out of continuation payloads. The framework-owned structural
continuation restores bounded presentation fields and deliberately clears the
open resource; the shell reopens a collaborative file from its typed stable
record intent. App-specific continuation data may augment this envelope but
must follow the same privacy and identity rules.

Portable semantic continuation is a signed package contract, not an
application-id exception. Declare one of the platform schemas with
`continuity.semantic-schema` (`web-location-v1`, `publication-position-v1`,
`media-position-v1`, or `realtime-session-v1`) and include `semantic` in
`continuity.required` when losing that projection would be a fidelity
regression. The shell validates the declared shape, size, identifiers, and URL
rules before retaining it. A third-party app can therefore participate in
native continuation without serializing an opaque process snapshot, and a
later release cannot silently weaken the promise because the contract is part
of the signed manifest. Presentation remains mandatory for every GUI app.

Put a mutation in a resource action when it changes a durable resource or is
an observable effect. Do not make focus, viewport movement, or selection an
action merely to let an agent drive the app: expose semantic controls for
those interactions, and use transient interaction presence when collaborators
should see where the agent is operating. After a direct agent invocation the
host resolves the action back to the shortest enabled visible semantic path,
so the ordinary retained UI updates from the same state transition a human
would have caused.

#### Tabs belong to the desktop

An app shows one thing: a page, a file, a conversation. Never build a tab
strip, a list of open documents or a "switch document" control into an app.
Several things side by side are several instances of the app, and the shell
groups them as tabs of a window group, with one tab model, one set of chords
and one closed-tab history for every app.

When the app wants another of itself next to it — a link opened in a new tab,
"open beside", a file from a project tree — call
`ui::shell_tabs::open_beside(Some(uri), background)`. The shell launches
another instance right after this app's tab, opening `uri` (a scheme the
manifest declares under `[providers] schemes`), in front or, with
`background`, behind the current tab. `ui::shell_tabs::close_this()` closes
the app's own tab through the desktop's tab lifecycle, so it can be reopened.
Each instance keeps its own record; what it shows comes back on restart or
reopen through its continuity metadata, not through a session file of the
app's own.

Ctrl+T (another instance), Ctrl+W (close this tab) and Ctrl+Shift+T (reopen
the last closed tab) — Cmd on macOS — reach these requests whenever the
focused widget leaves the chord unhandled; Ctrl+Tab and Ctrl+Shift+Tab are
the shell's own tab bindings. Outside a Tilo session the same calls open a
new window of the app and close this one. The `tab.open-beside` shell action
is the same open for agents and scripts.

### One driveability surface

Every operation must be reachable through the resource/action catalogue. Use
`Widget::resources` and `ActionSpec` for domain operations: name the current
resource, publish document mutations as document actions, and publish I/O or
presentation work as effect/view operations. The host sends this same live
catalogue to Ctrl+K search, the `actions_enumerate` / `actions_invoke`
automation socket, and agents; none of those consumers has an app-specific
command path.

This is the Tilo application API, not an optional automation adapter. A toolbar
button, menu item, shortcut, context-menu item, search result, script, and agent
must converge on the same `ActionBinding` and action address. Do not wire a
direct callback beside `Widget::start_operation`; bind the control and let
the host invoke that operation handler. Do not add a second socket, command
enum, hidden test callback, or agent-only implementation. CI runs
`tools/policy/check_app_resource_action_contract.py`, and the runtime independently
validates the concrete surface before publishing it.

Every `ActionSpec` carries an `ActionContract`. Defaults are deliberately
conservative (write permission, at-most-once, immediate, live-only). State the
facts that let a caller plan safely:

- `availability`: keep a temporarily impossible action discoverable with a
  stable code and useful reason; the door checks it again immediately before
  execution.
- `permission`: `read`, `comment`, `write`, `invite`, or `admin`; the broker
  uses this exact value for both discovery and invocation.
- `risk`, `confirmation`, and `reversible`: destructive and privileged actions
  require a confirmation policy. Confirmation grants are short-lived,
  one-use, and bound to the actor, address, and arguments.
  An action whose consequence depends on its value (a generic typed edit
  that may delete) classifies each invocation instead
  (`ActionSpec::with_risk`, `tilo_actions::Action::risk`): a destructive
  value gets exactly the confirmation a declared destructive action gets, an
  ordinary one runs directly. Its contract carries the
  `confirmation:by-argument` tag; a caller that cannot classify arguments,
  such as the session broker, treats it as requiring confirmation
  (`ActionContract::advertised_confirmation`).
- `tags`: `surface:programmatic` (`ActionSpec::programmatic`) marks an action
  offered only to agents, scripts and the controls or surfaces that submit it
  — never a palette row or a menu entry. It is still published, resolvable
  and invocable by address under the same rules.
- `retry`: use `idempotent` only when repetition is observably harmless;
  otherwise use the default `at_most_once`. The reserved
  `retry_with_invocation_id` mode must not be advertised until the calling
  transport supplies its own deduplication key. Never let a transport timeout
  silently repeat an at-most-once effect.
- `execution`: immediate, background, or streaming, with explicit cooperative
  cancellation. Long work reports monotonic progress and one terminal state;
  every streaming item is validated against the stream's declared JSON Schema.
- `result`: no value, JSON checked by JSON Schema, a resource address, or the
  broker-owned job identity.

Use `ActionSpec::typed` and `ActionParam` for inputs. Parameters support text,
numbers, integers, booleans, objects, arrays, resource addresses, RFC 3339 date
times, nullable values, and redacted secrets. A deterministic JSON Schema
subset adds `type`, `enum`, `const`, numeric and length bounds, `pattern`, the
`date-time`/`uuid`/`uri` formats, array `items`, object `properties`/`required`,
exclusive `oneOf` alternatives,
and boolean `additionalProperties`. Common annotation keywords are accepted;
every other keyword is rejected when the surface is published so an assertion
is never silently ignored. Binding is strict: missing, extra, wrong-typed,
out-of-range, and malformed structured input is refused before application
code runs.

An invocation is admitted with a stable `InvocationId`. Background and
streaming operations publish ordered `InvocationUpdate::Progress` records and
finish exactly once with a typed output, structured failure, or cancellation.
The shell retains job status so Ctrl+K, scripts, and agents do not need to hold
open an RPC request while work runs. A cancellable implementation polls the
collaboration channel's cancellation flag and reports `Cancelled`; cancellation
is cooperative and never fabricated by the shell.

Resource and action IDs are protocol addresses. Derive them from durable model
keys, not labels, row numbers, focus order, or localized text. The same applies
to `semantic_key` on accessible controls. Reordering or relabeling a view must
not invalidate an address. Duplicate resource or semantic IDs fail closed;
they are never repaired with an ordinal suffix that could target the wrong
thing.

A framework app declares its effects as one typed enum and answers them where
the state they need lives:

```rust
#[derive(Clone, Debug, Effect)]
#[effect(operation = "mail.effects")]
pub enum MailEffect {
    /// Archive conversation.
    #[effect(job)]
    Archive { conversation: String },
}

// In the view (a custom surface implements `ResourceSurface<R, MailEffect>`
// and answers in `effect(&mut self, ..)` instead).
cx.effects(move |effect: MailEffect, cx| match effect {
    MailEffect::Archive { conversation } => Ok(cx.thread(
        move |progress| server.archive(&conversation, &progress),
        |(), archived, cx| {
            cx.invoke(Mail::MARK_ARCHIVED.with(archived?));
            Ok(Effected::done())
        },
    )),
});
let archive = cx.effect_button("Archive", &MailEffect::Archive { conversation: id });
```

Each variant is a published `Effect` action with typed parameters; its options
(`job`, `cancellable`, `returns`, `destructive`, `permission = "read"`) are its
contract. The framework publishes the vocabulary beside the resource's
actions, with the availability the handler reports now (`EffectStatus`), and
routes the palette, agents, scripts and bound controls to the handler as a
typed value. A job reports progress through its `Progress`, finishes on the UI
thread, and changes the document only through actions. A projection queued in
the document (an outbox) is drained with `ViewCx::drain`.

A widget outside the framework describes an effect with an `ActionSpec`:

```rust
ActionSpec::operation(
    "archive",
    "Archive conversation",
    OperationKind::Effect,
    "mail.archive",
    &ArchiveArgs { conversation_id },
)?
.requiring(PermissionRequirement::Write)
.reversible()
.retry_policy(RetryPolicy::Idempotent)
.returns(ResultSpec::Resource {
    resource_kind: "mail.conversation".into(),
})
```

For a small widget root with one launchable resource,
`ui::widget::resource_actions::immediate` owns the forwarding widget,
operation dispatch and failure conversion; the app supplies only its resource
contract and operation closure. An app on the framework declares its effects
with `#[derive(Effect)]` instead, and runs long work as a job through the
`EffectCx` its handler receives.

Register document action decoders in `App::action_registry_mut`. Handle every
effect and view type in `Widget::start_operation`, the single entry: return
`Completed` with the promised `ActionOutput` for synchronous work or `Accepted`
for a job `poll_resource_operation` finishes, and declare the locally handled
type in `Widget::local_resource_operation_handlers`. Read the request through
`op.unscoped()`, which hides a hosted invocation that carries its original
caller's custody; a handler that honours that custody reads it through
`op.with_custody()` instead and routes every effect and document write through
the scope. Offer unhandled types to children with
`ui::widget::start_in_children`.
Publication refuses duplicate addresses, unsafe confirmation declarations,
unregistered document actions, empty availability reasons, incoherent job
contracts, and other contract violations.

Non-mutating retained controls are projected into that catalogue automatically
from the semantic tree, so focus and safe navigation remain driveable while an
app is being developed. Effectful generic activation, set-value, toggles, and
sliders are deliberately not exported: without a domain contract they could
bypass the real action's permission, risk, and confirmation. Give every
projected control a stable ID and a localized accessible name. Every effectful
control must instead converge on a domain action with durable resource
identity, typed parameters, collaboration semantics, and a useful verb such as
“Archive conversation” instead of the generic “Activate”.

To verify the external path, enumerate the running app through the test
control socket, invoke the returned `(provider, id)` with `actions_invoke`, and
assert the resulting document or effect. A successful invocation is never
inferred from a stale snapshot: the shell re-resolves the address and checks
its capability immediately before dispatch.

```bash
tilo-control actions list --json
tilo-control actions invoke app-actions "$ACTION_ID" \
  --args-json '{"destination":"archive","notify":false}'
```

The acceptance test for a feature should invoke this external address in
addition to any direct-widget test. For consequential actions, assert that a
call without confirmation is refused. For jobs, assert progress ordering,
terminal status, and cancellation. For unavailable actions, assert the stable
reason code and that invocation remains refused after a stale enumeration.

For process-local form state, use a typed `LocalState<T>` handle. Clones share
the value. `TextInput::from_state` is a true two-way binding: every input edit
updates the handle, and a mutation through any clone updates every bound input
on the ordinary synchronization pass. `Text::from_state` observes the same
revision and only reruns its formatter after a mutation. A complete retained
form needs no snapshots, callbacks, or custom widget:

```rust
let query = LocalState::new(String::from("status:open"));
let live_updates = LocalState::new(true);
let opacity = LocalState::new(0.8);
let region = LocalState::new(String::from("Europe/Berlin"));
let due_date = LocalState::new(chrono::NaiveDate::from_ymd_opt(2026, 9, 1).unwrap());
let reminder = LocalState::new(TimeValue::new(9, 30));
let accent = LocalState::new(ui::peniko::Color::from_rgba8(54, 162, 235, 255));
let summary = Text::from_state(&query, |value| format!("Filter: {value}"));
let input = TextInput::from_state(&query)
    .with_id("filter-input")
    .with_accessible_name("Filter");
let updates = labeled_check(
    "live-updates-label",
    Checkbox::from_state(&live_updates).with_id("live-updates"),
);
let opacity_field = NumberInput::from_state(&opacity)
    .with_range(0.0, 1.0)
    .with_step(0.05);
let opacity_slider = Slider::from_state(&opacity);
let region_field = Dropdown::from_state(
    vec![String::from("Europe/Berlin"), String::from("Asia/Tokyo")],
    &region,
);
let due_date_field = DatePicker::from_state(&due_date);
let reminder_field = TimePicker::from_state(&reminder);
let accent_field = ColorPicker::from_state(&accent);
```

`TextInput`, `Checkbox`, `Toggle`, `NumberInput`, `Slider`, `Dropdown`,
`DatePicker`, `TimePicker`, and `ColorPicker` all offer `from_state` with the
same explicit typed contract. Time fields use `TimeValue`, including an
optional seconds component, rather than an unlabelled tuple. `set` replaces a
local value and `update` mutates it in place. A bound text change invalidates
rendering, multiline intrinsic measurement, and accessibility in the same
synchronization pass without being misreported as a user edit. Bind several
controls to the same handle for synchronized master/detail or inspector views;
the `opacity_field` and `opacity_slider` above stay synchronized without
app-owned fan-out. An optional `on_change` callback observes committed user
edits without owning synchronization; programmatic setters and model
synchronization do not invoke it. Semantic automation can set dates as
`YYYY-MM-DD`, times as `HH:MM` or `HH:MM:SS`, and colours as `#RRGGBB`, through
the same user-change path as pointer and keyboard input. Use `Text::dynamic`
when the value comes from an existing local owner instead of `LocalState`.

`LocalState` is also the typed boundary for background work. Create a writer
before spawning the worker; every completed mutation wakes the standard app
lifecycle, synchronizes bound widgets, and requests a frame. No event-loop
proxy, string message, polling timer, or app-specific `on_external_event`
implementation is required.

```rust
let result = LocalState::new(LoadState::Loading);
let worker_result = result.writer();
let unavailable = result.writer();

let spawned = ui::background::Background::current().spawn("load project", move || {
    worker_result.set(load_project().map_or_else(
        |error| LoadState::Failed(error.to_string()),
        LoadState::Ready,
    ));
});
if spawned.is_err() {
    unavailable.set(LoadState::Failed("Loading is unavailable on this host".into()));
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LoadScreen { Loading, Ready, Failed }

let body = ModelView::from_state_by(
    &result,
    |result| match result {
        LoadState::Loading => LoadScreen::Loading,
        LoadState::Ready(_) => LoadScreen::Ready,
        LoadState::Failed(_) => LoadScreen::Failed,
    },
    |result| match result {
        LoadState::Loading => Box::new(Spinner::new()),
        LoadState::Ready(project) => Box::new(project_view(project)),
        LoadState::Failed(message) => Box::new(Banner::error(message)),
    },
);
```

Start workers through `ui::background::Background::current().spawn`, not
`std::thread::spawn`. The host decides how background work runs: a native
process uses a thread pool, a deterministic test drives a pumped queue, and a
host that cannot run background work refuses it with `Unsupported`. Report a
refusal as a failure instead of leaving a spinner running. Later examples omit that branch
for brevity.

Use the ordinary `LocalState` handle for UI-thread reads and callbacks. Move
`LocalStateWriter` into workers so an idle window cannot miss their changes.
Several writers may update the same state; updates are serialized and each
completed update advances one observable revision.

For restartable work such as search, import, or refresh, use `LatestTask<T>`
instead of coordinating request generations in app code. `begin` publishes
the running state and returns a single-use `TaskCompletion`; only the newest
run may complete. Starting again or calling `cancel` invalidates older
capabilities atomically, so a late worker cannot overwrite newer UI state:

```rust
let task = LatestTask::new(LoadState::Idle);
let result = task.state();
let run_task = task.clone();

let run_job = move || {
    let completion = run_task.begin(LoadState::Loading);
    let _ = ui::background::Background::current().spawn("load project", move || {
        let value = load_project().map_or_else(
            |error| LoadState::Failed(error.to_string()),
            LoadState::Ready,
        );
        let _ = completion.complete(value);
    });
};
```

Register `run_job` behind an effect `ActionSpec` declared as
`ExecutionMode::Background` with `ResultSpec::Job`, then bind the Run control
to that action. Admission returns `Accepted`; polling reports its real terminal
result. A raw `on_click` worker would be invisible to scripts and could only
pretend that queueing meant success.

A long-running worker may check `TaskCompletion::is_current` between units of
work for cooperative early exit. Correctness does not depend on that check:
`complete` still rejects a stale result. The generated `async` app template
includes run, loading, failure, completion, and cancellation wiring.

Use `LatestRequest<T>` when the result belongs inside an existing model rather
than a standalone `LocalState`. `begin` returns a single-use
`RequestCompletion<T>`; `try_take` drains only the current result on the UI
thread. Completion wakes the app automatically, while restart and `cancel`
discard stale workers without requiring generation counters or channels in
the model:

```rust
let completion = self.page_request.begin();
let _ = ui::background::Background::current().spawn("fetch page", move || {
    let _ = completion.complete(fetch_page(range));
});

if let Some(page) = self.page_request.try_take() {
    self.cache.apply(page);
}
```

`WakeHandle` remains available for custom streaming sources that publish into
their own queue. Call `wake()` after a batch is visible; the next live frame
runs widget `update` before `sync`.

Use `ModelView::from_state` when the complete value is already cheap to clone
and compare. Use `from_state_by` for real application payloads: the key closure
returns only structural identity, while the builder borrows the payload without
requiring it to implement `Clone` or `PartialEq`.

`TextInput::on_change` covers keyboard input, paste, accessibility value
changes, and committed IME text. It does not fire for IME preedit, cursor or
selection movement, programmatic helpers such as `set_text` and
`insert_at_cursor`, or CRDT synchronization. This makes it a clean boundary
for validation, filtering, and dispatching an app action without
mistaking a model refresh for user intent. Use `with_on_change_path` or
`bind_text` instead when the value itself belongs in durable shared state;
those paths preserve Automerge authorship, collaboration, persistence, and
undo behavior.

### Using other apps' actions

An app that builds on what other apps publish — a shortcut editor, a
launcher — reaches the same door the palette and agents use with
`ui::RemoteActions`. The broker decides every request for the **app**, under
its own identity, never under the ambient authority of the person using it:

- the app always reaches what it owns: its own surface, and actions the
  session publishes under its package id;
- reading the catalogue (`catalogue`) needs the manifest to declare it;
- invoking another app's action (`invoke`) needs the manifest to declare it
  **and** the workspace to grant it to the app, like any collaborator grant.
  Without a grant, the person who is using the app is asked on a sheet only
  the compositor draws: "<App> wants to use <Other app>: <action>", with the
  action's risk and whether it still confirms each use. They answer *Allow
  once*, *Allow for this session*, *Always allow in this workspace* or
  *Deny*; the invocation waits for the answer. An allowing answer is recorded
  as an ordinary capability (the app as subject, `Scope::Action`, a matching
  expiry) granted by that person, and appears in the workspace's history.
  Nobody is asked when nobody can answer — the screen is locked, the
  connection belongs to someone else, or they cannot administer the
  workspace — and an action that governs the workspace is never offered:
  such a request is refused.

People review and revoke these grants in the workspace's Settings, under
Permissions › App actions. Agents and scripts use the workspace's
`app-action.grant` and `app-action.revoke` actions, which need the same
authority and confirmation as any other permission change.

```toml
[actions]
query = true    # read the catalogue of actions the person is offered
invoke = false  # invoke other apps' actions as this app
```

```rust
let remote = ui::RemoteActions::current();
let page = remote.catalogue(CatalogueQuery::default()); // LocalState<CatalogueCall>
let run = remote.invoke(row.target().unwrap(), InvokeArgument::None, None);
```

Answers arrive as `LocalState`s, so a bound view updates and an idle window
wakes. The root that owns the client returns it from
`Widget::collaborative_mut` (a `Decorator` can), which is how its requests
reach the connection. Tests stand in for the session with
`RemoteActions::enter(host, caller)` and an in-process `ActionHost`.

### Localization

`tilo-app new` creates `locales/en-US.ftl` and an embedded locale registry
installed by the app's view. Add a catalog and wire it into that registry in one command:

```bash
tilo-app add locale de-DE
```

The generated `src/locales.rs` uses `include_str!`, so packaging cannot omit a
registered catalog. `tilo-app` only rewrites a registry carrying its generated
header. It recognizes and migrates the previous generated inline
`install_catalog` block, but an app-owned localization module is left untouched
and must register additional catalogs in its own setup.

Installation validates every locale and Fluent resource before replacing the
live catalog. The process locale accepts BCP-47 and POSIX forms such as `de`,
`de-DE`, and `de_DE.UTF-8`; lookup tries the active locale's language, then the
source catalog, then exposes the untranslated key so missing messages remain
debuggable.

Pass message ids—not eagerly translated strings—to retained controls:

```rust
let title = Text::new("project-title");
let save = Button::new("save", "save-project");
let name = TextInput::local()
    .with_accessible_name("project-name")
    .with_placeholder("project-name-placeholder");
```

`Text`, fitted `Button` labels, input placeholders and accessible names,
collection names, and table headers resolve through the same catalog.
`I18nManager::set_locale("de-DE")` advances one observable catalog revision;
the next normal synchronization pass invalidates paint, intrinsic measurement,
and accessibility together. `Text::from_state` also reruns its formatter on a
locale revision, so localized messages with dynamic arguments do not need an
app-owned notification graph.

### Synchronized views and revision history

Read-only views can derive their content from the same Automerge document as
an editor. The app does not publish notifications or copy values between
controls: every projection runs in the standard widget synchronization pass
and invalidates only when its rendered result changes.

```rust
let editor = TextInput::from_document_string("title")
    .with_accessible_name("Title");
let title = Text::from_document_string("title");
let detail = Text::from_document(|doc, root| {
    format!("Editing {}", read_project_title(doc, root))
});
```

`from_document_string` accepts either a string scalar or an Automerge `Text`
object. Use `from_document` for formatting, derived values, and master/detail
projections. Since all of them read one document, local edits, undo/redo,
collaborator changes, and restored revisions reach every view through the same
path.

When the model selects a whole retained subtree rather than only a label, use
`ModelView::from_document`. Its projected value is also its reconciliation key: unrelated
document changes keep the existing subtree—and therefore its caret, scroll
position, and other local presentation state—while a changed value rebuilds
the subtree during the normal synchronization pass.

```rust
let detail = ModelView::from_document(
    |doc, root| selected_project_id(doc, root),
    |selected| match selected {
        Some(id) => Box::new(project_editor(id.clone())),
        None => Box::new(EmptyState::new("Select a project")),
    },
);
```

Keep the projection small and give it equality that reflects structure. A
projection such as `Option<ProjectId>` preserves an open editor across edits to
that project; projecting the entire project would intentionally rebuild it on
each model change. Call `autofocus_on_change(true)` for route-like replacement
where focus should move into the new subtree. Passive master/detail panes leave
it off so changing the detail never steals focus from the master control.

Ordinary Ctrl+Z/Ctrl+Shift+Z uses the document's collaboration-safe `UndoLog`:
it writes inverse changes forward, keeps concurrent edits, restores widget
focus metadata, and scopes undo to the local author. For persistent branching
history, `State::revision_history` reconstructs the causal graph directly from
the Automerge document:

```rust
let history = state.revision_history();
for revision in history.branch_points() {
    println!("{} has {} futures", revision.id, revision.children.len());
}

let changed = state.restore_revision(
    selected_revision,
    focused_node_index,
    widget_metadata,
    Some(current_user),
)?;
```

Revision ids are content-addressed and survive save/load and collaboration.
Each node exposes its causal parents and children, actor, timestamp, message,
operation count, and whether it is a current head. Restoring a revision does
not delete later work: it appends the required model delta as a new change,
leaves every branch addressable, and is itself undoable.

### Local UI with a remote model host

Keep an Automerge replica beside the UI even when a service owns final
authority. Widgets continue to read and bind to the ordinary local document;
the RPC boundary carries replayable actions and Automerge deltas rather than
widget state. This gives local and remote apps the same widget code and keeps
input responsive during network latency.

Create one `OptimisticQueue` after loading the document, register every domain
action the host may accept, and send the envelope returned by
`ActionHost::dispatch_optimistic`:

```rust
use ui::tilo_actions::{ActionRegistry, OptimisticQueue};
use ui::ActionHost;

let mut registry = ActionRegistry::new();
registry.register::<ProjectAction>("project.action");

let mut pending = OptimisticQueue::new(&mut state.doc);
let mut actions = ActionHost::new();

let envelope = actions.dispatch_optimistic(
    &mut state.doc,
    &mut pending,
    "project.action",
    &ProjectAction::Rename { title: "Atlas".into() },
)?;
model_connection.send(envelope);
```

The action has already changed `state.doc` when `send` runs, so synchronized
text, forms, tables, and other views update in the next normal widget pass.
Process host responses by action id:

```rust
match response {
    HostResponse::Accepted(id) => {
        pending.acknowledge(id, &registry)?;
    }
    HostResponse::Rejected(id) => {
        let batch = pending.reject(&mut state.doc, id, &registry)?;
        model_connection.ignore_late_replies(&batch.superseded);
        model_connection.send_all(batch.outbound);
    }
}
```

Acknowledgements may arrive out of order; the confirmed baseline advances in
submission order. Rejection rolls the visible document back to that baseline,
replays surviving actions with correct dependencies, and returns their new
envelopes. On reconnect, resend `pending.reconnect_batch()` unchanged. The host
must deduplicate by `ActionId`; resending does not apply an action to the local
document again.

For model changes made by another client or by the host itself, merge the host
replica with `merge_authoritative`, or pass incremental Automerge bytes to
`load_authoritative_incremental`. Both operations preserve the host update and
rebase pending local work on top. Their returned `RebaseBatch` replaces the old
in-flight envelopes just like rejection.

An accepted action enters the confirmed baseline through `acknowledge`; do not
also merge a host snapshot that echoes that action while it is pending. Either
buffer that snapshot until the corresponding acknowledgements settle, or send
unrelated host changes as a separate Automerge delta. Treat an action id as an
idempotency key on every transport.

While a queue is active, route durable host-authoritative changes through it.
A direct write to the live document is reported as `LiveDiverged` rather than
being silently discarded by a later rebase. Direct CRDT-bound typing and the
collaboration delta path remain appropriate for peer-authored fields that do
not require host acceptance.

`tilo-actions` is independent of the renderer, window system, and OS services,
and enables UUID generation through JavaScript on browser WASM targets. A
native client and a WASM client can therefore share the action enum, registry,
queue, and wire format while supplying different transport implementations.

## Text layout and typography

For ordinary application text, select intent and let the retained widget
follow live theme and scale changes:

```rust
use ui::tokens::TextRole;
use ui::widget::Text;

let title = Text::new("projects-title").with_role(TextRole::Title);
let field_label = Text::new("project-name-label").with_role(TextRole::Label);
```

Use raw sizes only for content with an actual custom typographic contract.
Semantic roles keep generated and hand-written app chrome consistent without
passing a `Theme` through every app builder. `Title`/`Display` also create a
level-1 accessibility heading and `Heading` creates level 2; use
`with_heading_level(3)` for nested sections. See [Typography](typography.md)
for the full role set and advanced text layout controls.

`TextStyle` is shared by measurement and painting. Kerning, tracking,
OpenType features, variable-font axes, wrapping, alignment, line height, and
hyphenation therefore cannot silently differ between the two passes:

```rust
use ui::text_render::{OpenTypeTag, TextStyle};

let body = TextStyle::new(18.0)
    .with_line_height(1.5)
    .with_kerning(true)
    .with_letter_spacing(0.15)
    .with_font_feature(OpenTypeTag::STANDARD_LIGATURES, 1)
    .with_auto_hyphenation("de-DE");
```

Use an application hyphenator when dictionary or domain-specific breaks are
required. Breaks are UTF-8 byte offsets inside the supplied text; endpoints,
out-of-range values, and offsets inside a code point are ignored. The cache
key must change when configuration that affects the returned breaks changes.

```rust
use std::sync::Arc;
use ui::text_render::{Hyphenator, TextStyle};
use ui::widget::Text;

struct ProductNames;

impl Hyphenator for ProductNames {
    fn breaks(&self, text: &str) -> Vec<usize> {
        product_dictionary_breaks(text)
    }

    fn cache_key(&self) -> u64 { 1 }
}

let label = Text::new("product-description").with_style(
    TextStyle::new(18.0).with_hyphenator(Arc::new(ProductNames)),
);
```

For text that must flow through a non-rectangular area, use a polygon in the
widget's local coordinates. Concave polygons are supported, so a line can
occupy separate spans on either side of a cutout. Paint is clipped to the
exact polygon while line bands use conservative spans, preventing glyphs from
crossing a sloped or notched boundary.

```rust
use ui::kurbo::Point;
use ui::text_render::{PolygonTextLayout, TextFlowRegion};
use ui::widget::Text;

let region = TextFlowRegion::polygon([
    Point::new(0.0, 0.0),
    Point::new(640.0, 0.0),
    Point::new(640.0, 420.0),
    Point::new(360.0, 420.0),
    Point::new(360.0, 120.0),
    Point::new(240.0, 120.0),
    Point::new(240.0, 420.0),
    Point::new(0.0, 420.0),
])?;

let article = Text::new("article-body")
    .with_style(body)
    .with_layout_engine(PolygonTextLayout::new(region));
```

Implement `TextLayoutEngine` to bring a different shaping or layout engine.
Its `measure` and `draw` methods receive the same text, `TextStyle`, and
`TextLayoutConstraints`. Installing it with `Text::with_layout_engine` keeps
the standard widget boundary: Fluent localization, accessibility, Taffy
layout, scene caching, inspection, presence, and permissions still work.
Return a monotonically changing value from `revision` when shared engine state
changes; `Text` then invalidates measurement and paint automatically. Call
`invalidate_layout_engine` after an external change that has no observable
revision.

## Large collections

`DataTable<Row>` accepts typed rows and column closures. Give every row a
stable key before enabling its internal viewport; measurements, selection, and
scroll anchoring then follow the key when rows move. Use `set_viewport` when
the table owns its scrolling area, or leave it unset when an outer
`Scrollable` owns the viewport.

For ordinary text columns, use one typed accessor for paint and accessibility:

```rust
let name = ColumnDef::new("name", "results-name", ColumnWidth::Flex(1.0))
    .with_text(|row: &ResultRow| row.name.clone());
```

`with_text` applies the standard typography and selection contrast, keeps the
localized header as the cell name, and publishes the returned string as that
row's cell value. If a column needs custom scene drawing, pair
`with_text_render` or `with_render` with `with_accessible_value`; this keeps the
visible and spoken content explicit without forcing an app to manufacture
synthetic child widgets.

For a database, remote query, or a table whose logical size should not be
materialized, implement `TableModel` and construct the same widget with
`DataTable::from_model`. The table asks the model only for stable keys and row
states in the visible pixel window plus overscan:

```rust
use ui::LatestRequest;
use ui::widget::data_table::{
    DataTable, TableModel, TableModelChange, TableRowState,
};

impl TableModel for QueryRows {
    type Row = ResultRow;

    fn len(&self) -> usize { self.total_count }
    fn key_at(&self, index: usize) -> Option<u64> { self.order.key_at(index) }
    fn index_of(&self, key: u64) -> Option<usize> { self.order.index_of(key) }
    fn row_state(&self, index: usize) -> TableRowState<'_, ResultRow> {
        self.cache.row_state(index)
    }
    fn request_range(&mut self, visible: std::ops::Range<usize>) {
        let completion = self.page_request.begin();
        let _ = ui::background::Background::current().spawn("fetch page", move || {
            let _ = completion.complete(fetch_page(visible));
        });
    }
    fn poll_changes(&mut self) -> Vec<TableModelChange> {
        let Some(page) = self.page_request.try_take() else {
            return Vec::new();
        };
        let changed_keys = self.cache.apply(page);
        vec![TableModelChange::RowsChanged { keys: changed_keys }]
    }
}

let mut table = DataTable::from_model(columns, QueryRows::new());
table.set_viewport(480.0, 32.0);
```

Use `ColumnDef::with_widget` when a cell contains a control rather than only
paint or text. The closure returns an ordinary retained widget, so the control
keeps its own focus, input, animation, inspection, and accessibility behavior:

```rust
let actions = ColumnDef::new("actions", "Actions", ColumnWidth::Fixed(120.0))
    .with_widget(|row: &ResultRow, _visible_index| {
        let id = format!("open-{}", row.id);
        let row_id = row.id.clone();
        Box::new(Button::new(&id, "Open").on_view(move || open(&row_id)))
    });
```

Cell widgets are retained by the stable row identity plus column key and only
exist for the visible range plus overscan. Sorting or moving a row therefore
repositions its existing control instead of recreating it. A control consumes
its own pointer gesture before table row selection, while clicks on the rest of
the row retain normal selection behavior. Its accessibility node is nested
inside the corresponding virtual `Row` and `Cell`, not duplicated beside the
table. The tallest mounted control also contributes to variable row
measurement and stable scroll anchoring.

Return `TableRowState::Loading` or `TableRowState::Error` for non-resident
rows; both remain real rows with stable geometry and accessibility semantics.
`poll_changes` drains completed work during the framework update pass.
For a model mutated through another shared owner, call
`notify_model_changes` after applying the batch. Report `RowsInserted`,
`RowsRemoved`, and `RowsMoved` for structural edits; mounted cell controls
whose stable keys survive are repositioned rather than rebuilt. Report
`RowsChanged { keys }` when data or loading state changed, and only controls
for those keys are rebuilt. `Reset` and the compatibility
`notify_model_changed` method invalidate every mounted control. Stable-key
anchoring and selection reconcile all variants without scanning or storing
the complete order. Model-backed header sorting calls
`TableModel::sort_by`; `on_header_click` remains available when an app wants
to enqueue a remote sort itself. Give a background worker a cloneable
`WakeHandle` and call `wake()` after publishing its batch. The standard app
lifecycle owns the event-loop connection, and the next redraw runs
`update` before `sync`, so app models need neither a proxy nor busy polling.
Reserve `needs_poll` for sources that require continuous frames
while a request is in flight.

Use `VirtualList::from_items` for a typed list whose rows are real widgets:

```rust
let list = VirtualList::from_items(
    contacts,
    40.0,
    |contact| contact.id,
    |contact, _index, _key| {
        Box::new(ui::row![
            Text::new(&contact.name),
            Button::new("message", "Message"),
        ])
    },
)?
.with_id("contacts-list")
.with_semantic_key(|key| key.to_string())
.with_viewport_height(480.0)
.multi_select(true);
```

The list mounts only the visible stable keys plus overscan. Its delegate may
return any widget subtree; an interactive child receives pointer input before
the row-selection gesture, and appears beneath the row's `ListItem` in the
accessibility tree as the real retained control rather than a semantic copy,
so activating its button does not activate or select the row. Actual child
layout heights feed back into scrolling and hit testing. The built-in
scrollbar, keyboard navigation, and key-based selection all use the same
viewport.

Start from the `virtual-list` example (`tilo-app new activity-feed
--from-example virtual-list`) for the sparse-model form instead. Its
`ActivityModel` reports 100 million stable keys
without allocating their rows, mixes measured 44px and 72px delegates, embeds
an independently actionable button in each mounted row, and ships with a
headless contract for bounded mounting, selection, variable geometry, child
activation, and direct-touch scrolling.
`with_semantic_key` makes visible list items addressable to AccessKit,
inspection, and headless scenarios without exposing the key type; the encoded
string must be unique and remain unchanged for the lifetime of the logical
item. Omit it for a purely presentational list rather than publishing unstable
index-based actions.

Virtual list, table, and tree rows publish the same absolute logical-pixel
bounds used for painting and hit testing, including variable measurements and
the current scroll offset. Table cells additionally publish their resolved
column bounds. AccessKit scales this geometry to the window, while headless
inspection keeps logical pixels, so assistive-technology hit testing and tests
refer to the row the user actually sees.

When they own their viewport, all three virtual collections accelerate
repeated detented-wheel input with a bounded decay curve, while touchpad pixel
deltas remain direct and stop wheel momentum. Their bodies also support
pointer drag-to-scroll: movement stays a click until it crosses a four-pixel
threshold, then pans without changing selection and continues with a bounded,
filtered release velocity. Interactive row controls, scrollbar thumbs, column
resize handles, and reorder gestures take precedence. Reduced-motion mode
keeps the direct input delta but disables the decaying continuation.

Page Up and Page Down move by
90% of the viewport using measured row geometry rather than an estimated row
count. If one row is taller than the viewport, paging reveals the next section
without changing focus, and a nearest-row reveal preserves whichever section
is already visible. Home and End move to the collection boundaries; Shift
extends selection where the collection supports ranges.

For an owned `VecListModel`, add `.reorderable()` to get stable-key row
dragging, before/after insertion lines, edge auto-scroll, and model updates.
For an external or constrained model, use `with_reorder(validate, commit)`:
validation decides which proposed `DropTarget`s may be previewed, and commit
receives the validated `ReorderDrop` and mutates the model. Returning `true`
from commit reconciles once while preserving the visible stable-key anchor and
retained row widgets. Interactive delegate children still receive the initial
press before a row drag can begin, and Escape cancels an active reorder.

For a database or computed collection, implement `ListModel::len`, `key_at`,
and `index_of`, then call `VirtualList::new`. For a source mutated through a
shared owner, call `notify_model_changes` with `RowsInserted`, `RowsRemoved`,
or `RowsMoved`; mounted delegates whose stable keys survive are repositioned
without being recreated. Use `RowsChanged { keys }` for content changes so
only those delegates rebuild. `update_model_changes` applies the same typed
batch around a mutation when the list owns its model. The compatibility
`notify_model_changed` path reconciles structure without assuming that ready
row content changed, while `update_model` conservatively rebuilds all mounted
delegates.

Paged lists can also implement `row_state`, `request_range`, `poll_changes`,
and `needs_poll`, then use `VirtualList::new_async`. The delegate receives a
`ListRowState` for every mounted key, so the app supplies localized ready,
loading, and error rows without making the whole data source resident:

```rust
let list = VirtualList::new_async(model, 28.0, |model, index, key, state| {
    match state {
        ListRowState::Ready => model.row_widget(index, key),
        ListRowState::Loading => Box::new(Text::new("Loading…")),
        ListRowState::Error(message) => Box::new(Text::new(message)),
    }
});
```

The list requests one visible-plus-overscan range when its virtual window or
model state changes. A `poll_changes` batch reconciles keys and replaces
delegates whose loading state changed or whose keys appear in `RowsChanged`.
Loading rows expose `busy` and error rows expose `invalid` plus the model's
error description to accessibility; unavailable rows cannot be selected,
activated, or used as reorder targets. `needs_poll` schedules frames only
while the source has work in flight. The older boolean `poll_changed` hook
remains supported; pair it with `invalidate_rows` when ready row content
changed but its key and `ListRowState` did not.

`TreeView::new(TreeNode<T>)` is the shortest form for an app-owned hierarchy.
Give collection controls an explicit `with_id` when scenarios or inspection
tools address them; this is especially important when a screen contains more
than one table, tree, or list.
Batch edits through `update_model` so expansion, selection, cached row heights,
and the scroll anchor reconcile once:

```rust
tree.update_model(|root| {
    root.children.push(TreeNode::leaf(42, "New item", ()));
});
```

Use `TreeView::from_model` for a database, CRDT, or paged hierarchy. A
`TreeModel` exposes stable node ids, indexed parent/child navigation, labels,
and branch state. After changing the source, report one batch with
`update_source`; a source shared outside the widget can instead call
`notify_model_changes` after applying the batch.

```rust
tree.update_source(
    &[TreeModelChange::RowsInserted {
        parent: folder_id,
        index: 0,
        count: 1,
    }],
    |model| model.insert_child(folder_id, 0, child),
);
```

Paged models may know `child_count` before the corresponding ids are in memory.
Override `TreeModel::child_state` and return `TreeChildState::Loading` or
`TreeChildState::Error`; the tree keeps the slot in layout and accessibility
instead of collapsing it. A model-owned source implements `request_load` and
drains `poll_changes`; `LatestRequest` supplies stale-result suppression and
the event-loop wakeup:

```rust
fn request_load(&mut self, request: TreeLoadRequest) -> bool {
    let page = page_containing(request.slot);
    let completion = self.page_request.begin();
    let _ = ui::background::Background::current().spawn("fetch children", move || {
        let _ = completion.complete(fetch_children(page));
    });
    true
}

fn poll_changes(&mut self) -> Vec<TreeModelChange> {
    let Some(page) = self.page_request.try_take() else {
        return Vec::new();
    };
    let slots = self.cache.apply(page);
    vec![TreeModelChange::SlotsChanged { slots }]
}
```

Use `on_load_request` instead when an external owner, rather than the model in
the widget, schedules work. Loading slots are requested once when they enter
the visible window, while an error row issues a `TreeLoadReason::Retry`
request when clicked or activated with Enter or Space. Sources that cannot
wake the application may return `true` from `needs_poll`; idle workers should
prefer `LatestRequest`.

Use `with_slot_delegate` to replace the built-in placeholder copy with
localized retained widgets. The delegate receives the stable `TreeSlot`, its
depth, and `TreeSlotState::Loading` or `TreeSlotState::Error`. Only unresolved
rows in the virtual window are mounted. The tree keeps the outer tree-item
semantics, marks loading rows busy and error rows invalid, measures variable
placeholder heights, and preserves click/keyboard activation as retry:

```rust
tree.with_slot_delegate(|_, slot, _depth, state| {
    let message = match state {
        TreeSlotState::Loading => "project-tree-loading",
        TreeSlotState::Error { .. } => "project-tree-load-error",
    };
    Box::new(Text::new(message).with_id(format!("tree-slot-{}", slot.index)))
});
```

Generated tree components install this localized delegate by default. Replace
it with a progress indicator, diagnostic detail, or a retry button when the app
needs richer loading UI; ordinary child-widget focus and accessibility remain
nested beneath the slot's `TreeItem` semantic node.

When a slot changes state, send `TreeModelChange::SlotsChanged`. The tree
invalidates request deduplication and transfers focus, measured height, and the
viewport anchor from the placeholder to the loaded node id.

```rust
tree.notify_model_changes(&[TreeModelChange::SlotsChanged {
    slots: vec![TreeSlot {
        parent: folder_id,
        index: page_start,
    }],
}]);
```

Set an internal tree viewport with `set_viewport(height, estimated_row_height)`.
Painting, hit testing, load requests, and accessibility nodes are then bounded
to the visible range plus overscan. `set_row_height` records a measured height
against the stable row identity, and `set_scroll_offset` accepts `f64` for very
large logical extents. Use `with_viewport` when configuring the tree in a
builder chain; the generated tree starter uses this form.

Call `multi_select(true)` for platform selection gestures: Ctrl/Command-click
toggles a node, Shift-click extends from the stable-key anchor, and
Ctrl/Command+A selects the model without enumerating every row. Read or seed
the state through `selection`, `selection_mut`, and `with_selection`.
Selections use node ids rather than visible indices, so collapsing a branch or
moving a node does not silently change what is selected.

Use `with_row_delegate` when a tree row needs more than a label. The delegate
receives the model, stable node id, and depth, and returns an ordinary retained
widget subtree:

```rust
let tree = TreeView::from_model(files)
    .with_expanded([root_id])
    .with_row_delegate(|model, id, _depth| {
        Box::new(ui::row![
            Text::new(model.label(id).unwrap_or("")),
            Button::new("more", "More actions"),
        ])
    });
```

`TreeView` still owns indentation, disclosure, focus, selection, paged
loading/error rows, and each row's `TreeItem` semantics. Interactive delegate
children receive input before the row gesture and appear beneath that
`TreeItem` in the accessibility tree. Only the visible window plus overscan is
mounted, retained widgets follow their node ids when rows move, and measured
delegate heights feed back into the sparse viewport. A
`TreeModelChange::RowsChanged` notification rebuilds only the named mounted
rows; call `invalidate_row_delegates` directly when presentation data changes
outside the model notification stream.

For an owned `TreeNode`, add `.reorderable()` to enable safe sibling moves and
cross-parent reparenting. The built-in mutation preserves complete subtrees,
resolves sibling indices after detaching the source, and rejects roots,
self-drops, leaf parents, cycles, and no-op placements.

For an external or domain-constrained model, use `with_reorder(validate,
commit)`. The validator sees the source node and a stable-key `DropTarget`
whose placement is `Before`, `Inside`, or `After`; rejected targets never
receive a preview. The commit closure receives a validated `ReorderDrop`,
mutates the model, and returns whether it changed. The tree prevents drops that
would make a node its own ancestor even if the app validator accepts them. It
paints insertion lines or an inside-row highlight, scrolls near viewport
edges, expands a collapsed branch after hovering over its inside target, and
reconciles selection, expansion, retained delegates, and the viewport anchor
once after a successful commit. Pressing Escape cancels the gesture.

Structural tree changes animate automatically. Surviving node ids interpolate
from their previous row geometry, newly visible rows fade in, and removed or
collapsed rows fade out from a bounded paint snapshot. This applies equally to
expansion, collapse, insertion, removal, and moves reported through the model
notification APIs. Timing comes from the theme's standard motion tokens;
`Theme::reduce_motion` disables the snapshots and presents the settled tree in
one frame. Transition state is capped independently of model size, so enabling
motion does not make a hundred-million-row tree resident.

## Testing an app

Keep the root constructor in the library crate and test it through the same
headless launch path used by generated projects. Prefer semantic assertions
over pixel coordinates: inspect roles, names, selected/expanded state, and
focus after dispatching keyboard or pointer input. Use deterministic rendering
only for visual behavior that semantics cannot express.

The toolkit's debug and capture switches are `TILO_UI_*` environment
variables (`TILO_UI_DEBUG`, `TILO_UI_PRESENT`, `TILO_UI_VELLO_CPU`, …); the
rendering ones are listed in [`performance-flags.md`](performance-flags.md).
Read one from app or test code with `ui::env::var`. The old `QUICK_UI_*`
names still work and warn once per variable; new scripts use the new names.

For a post-layout inspection without opening a window, write the widget and
accessibility tree directly. `inspect` writes `inspect.json` in the app root by
default; `--targets` additionally writes the addressable-widget index:

```bash
tilo-app inspect --viewport 1280x800 --targets targets.json
```

The JSON combines the retained widget hierarchy, stable test ids, logical
rectangles, and each widget's actual `AccessNode`. Virtualized semantic rows
and cells remain nested under that node even when they have no retained widget
of their own. Protected text fields expose their role, label, geometry, and
protected state but never their value. The snapshot can be taken after the
same semantic actions an accessibility client sends, so it can assert
resulting values, focus state, and dependent output without coordinate or
keyboard-layout assumptions. Put those actions in a JSON array and pass it
with `--actions`; generated projects include `tests/headless-actions.json` as
an executable example. Relative action and output paths are resolved from the
app root:

```json
[
  {"perform": {"target": "filter-input", "action": "set_value", "value": "status:closed"}},
  {"perform": {"target": "filter-input", "action": "focus"}}
]
```

```bash
tilo-app inspect --actions tests/headless-actions.json --output after.json
tilo-app inspect --audit --audit-report target/audit/report.json \
  --actions tests/headless-actions.json --output after.json
```

`--audit` evaluates the final laid-out semantic tree used by assistive
technology and exits unsuccessfully for contract violations. It checks both
retained widgets and synthetic table/list/tree descendants for unnamed
controls or focus targets, `Role::Unknown`, protected values accidentally
exposed to inspection, invalid collection positions or hierarchy levels,
mixed keyed/positional siblings, and duplicate synthetic keys or action
targets. Diagnostics include a stable widget/semantic path and a short issue
code, while `--output` still writes the complete tree for investigation.
`--audit-report` writes the versioned `{ "version": 1, "issues": [...] }`
result as JSON. It
may be used without `--audit` to collect a report without making issues fail
the command; combining both is the CI enforcement form.
Inspection, target, and document outputs create explicitly requested parent
directories, so a first run can write directly under `target/audit/`.

Run saved interaction scenarios directly, or retain the exact post-action
state of each case for debugging and CI review:

```bash
tilo-app scenario run
tilo-app scenario run settings-smoke --viewport 1024x768
tilo-app scenario run --artifacts target/scenario-artifacts
tilo-app scenario run --audit --artifacts target/scenario-artifacts
```

With no name, the conventional `tests/headless-actions.json` case runs first,
followed by every JSON file under `tests/scenarios/` in name order. The command
validates the complete suite before starting the app, so a malformed later
case cannot leave a half-run suite. Each case receives private data, state,
configuration, and cache directories; saved app state from one case cannot
affect another or overwrite the developer's normal app state. Runtime failures
are accumulated and reported together. Use the reserved name `default` to run
only `tests/headless-actions.json`.

`--artifacts` writes three deterministic files per case: `<name>.inspect.json`
contains the laid-out retained and semantic trees, `<name>.targets.json` lists
every addressable widget and its bounds, and `<name>.document.json` contains
the isolated Automerge document. Relative artifact paths resolve from the app
root, even when the command is launched from a nested source directory. The
files come from the same process and final frame that evaluated the scenario;
they do not replay the actions or read the developer's normal saved state.
Add `--audit` to apply the same accessibility contracts after every isolated
case. An accessibility failure fails only that case, the remaining suite still
runs, and requested artifacts preserve the exact failing semantic tree plus a
fourth machine-readable `<name>.audit.json` issue report.

The `perform` operation addresses a retained widget by `with_id`/`test_id`, or
by the fallback path reported in `targets`. Its semantic verbs are `activate`,
`focus`, `toggle_check`, `increment`, `decrement`, `expand`, `collapse`,
`scroll_into_view`, `show_context_menu`, `set_value`, and `set_selection`.
`set_value` adds a string `value`; `set_selection` adds non-negative `anchor`
and `focus` grapheme indices. Widgets receive the action through their normal
accessibility implementation. Focus and generic activation use the same
app-level fallback as a live window, while a verb unsupported by that widget
fails the scenario instead of becoming a no-op. Use `click_named`, `tap_named`,
`tap_at`, `swipe_at`, `key`, `type`, and `modifiers` when input behavior itself
is what the test needs to exercise. For example, the headless JSON form of an
upward touch scroll is `{"swipe_at":[400,600,400,120]}`.

Saved scenarios can synchronize with worker-driven UI using a bounded
`wait_for_named` operation. It drives production-equivalent frames until the
retained widget appears, and fails the scenario with the target and elapsed
bound if it does not. The timeout is required, is expressed in milliseconds,
and must be between 1 and 60,000, so malformed scenarios cannot wait forever:

```json
[
  {"perform": {"target": "run-task", "action": "activate"}},
  {"expect": {
    "target": "task-result",
    "role": "static_text",
    "name_contains": "Found",
    "timeout_ms": 1000
  }}
]
```

`expect` turns the saved flow into a semantic regression test. It addresses a
retained widget by id/path, or adds `semantic_target` to address a virtual row
by its opaque model identity. Assertions may include `role`, exact `name` or
`value`, `name_contains` or `value_contains`, and the `focused`, `selected`,
`checked`, `expanded`, `enabled`, `busy`, and `invalid` states. Every supplied
field must match; one failure reports all mismatches with their actual values.
An expectation without `timeout_ms` is immediate. With an explicit bound from
1 through 60,000 milliseconds, it drives production-equivalent frames until
the complete semantic contract matches, then reports the last complete set of
mismatches if the bound expires. This handles both a not-yet-mounted widget and
a mounted widget whose async name, value, or state has not settled.
The runner also reports its one-based action number and target; entered text
and `set_value` contents are reduced to an action name or character count so a
failed password flow does not echo credentials into CI logs.
Role names use the toolkit's snake-case semantic names, such as `text_input`,
`list_item`, `tree_item`, and `static_text`:

```json
{"expect": {
  "target": "workspace-tree",
  "semantic_target": "node:3",
  "role": "tree_item",
  "selected": true
}}
```

Rust tests use the same matcher through a typed builder, including stable
model identities and `Role` values rather than wire-format role strings:

```rust
app.expect(
    &SemanticExpectation::named("workspace-tree")
        .semantic_target("node:3")
        .role(Role::TreeItem)
        .selected(true)
        .within(Duration::from_secs(1)),
)?;
```

Rust app tests can wait for worker-driven UI without sleeping for an assumed
completion time. `Driver::wait_until` runs the same update, sync, layout, and
draw sequence as a live frame until a predicate succeeds or its bound expires;
`wait_for_named` is the concise form for a retained control created by an
asynchronously loaded delegate:

```rust
app.wait_for_named("open-3", Duration::from_secs(1))?;
```

Interactive virtual table, list, and tree rows expose an `action_target` in
the inspection tree. It is an opaque stable model identity owned by the
collection, not a visible row index. Pass it as `semantic_target` to focus,
activate, or scroll that exact row; tree rows additionally support expand and
collapse through the same implementation used by AccessKit:

```json
[
  {"perform": {"target": "workspace-tree", "semantic_target": "node:2", "action": "collapse"}},
  {"perform": {"target": "workspace-tree", "semantic_target": "node:2", "action": "expand"}},
  {"perform": {"target": "workspace-tree", "semantic_target": "node:3", "action": "activate"}}
]
```

Controls drawn as parts of one retained widget—tabs, toggle/radio segments,
colour swatches, and slash-menu items—also expose an opaque `action_target`.
Activating that target selects the exact part and emits the same action intent
or typed local choice as pointer input. Action-capable parts route through the
custom widget's `ActionEmitter`; they do not hide a second callback. These
nodes advertise only actions they can perform: unlike positioned virtual rows,
an unpositioned drawn part does not claim that it can scroll itself into view.

Collapsing a branch does not discard a selected descendant. Its semantic row
temporarily leaves the bounded accessibility window, while the tree's value
and model-key selection remain intact until it is expanded again.

Headless scenario objects are strict: misspelled fields and unknown actions
fail with a usage error instead of producing an apparently successful no-op.
