# Build a qOS app

The shortest path from an idea to an app running in the live development loop
is one command:

```bash
cargo run -p qos-dev -- new reading-list --dev
```

Creation is atomic. Once the app exists, `--dev` runs its structural preflight,
opens it, and watches both app code and the local toolkit. A successful edit
replaces the running process; a failed build leaves the last good app open and
waits for the next save. Press Ctrl-C to stop.

The same stages remain available separately for automation and focused work:

```bash
cargo run -p qos-dev -- new reading-list
cargo run -p qos-dev -- check apps/reading-list
cargo run -p qos-dev -- test apps/reading-list
cargo run -p qos-dev -- dev apps/reading-list
cargo run -p qos-dev -- run apps/reading-list
cargo run -p qos-dev -- shot apps/reading-list
cargo run -p qos-dev -- inspect apps/reading-list
cargo run -p qos-dev -- package apps/reading-list
```

If `qos-dev` is installed, omit `cargo run -p qos-dev --` from these commands.
The app-aware commands find the app root from any file or directory inside it,
run the same structural checks as `doctor`, and invoke Cargo with the app's
manifest explicitly. They therefore behave the same for an app in this
workspace and for a standalone app. Arguments after `--` are sent to the app
or test harness:

```bash
qos-dev test -- --nocapture
```

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

`qos-dev check` is the copyable local/CI quality gate. It reports the complete
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

With no name, `qos-dev new` runs as a small terminal wizard for the package
name, window title, application ID, and starter shape. It presents nine
numbered templates with their intended use; numbers, full names, and shortcuts
are accepted. Invalid package names, IDs, and template choices are explained
and re-prompted without discarding earlier answers. Enter accepts a displayed
default. Before writing, the wizard shows the resolved app title, package,
application ID, template purpose, and destination for confirmation. Declining
or ending input cancels before any app files or workspace edits are published.

With a name and flags the command is deterministic and suitable for scripts;
omitted `--template` defaults to `starter` outside an interactive terminal:

```bash
cargo run -p qos-dev -- new log-browser \
  --id org.example.log-browser \
  --title "Log Browser" \
  --template data

cargo run -p qos-dev -- new activity-feed --template list
cargo run -p qos-dev -- new qr-code --template qr
cargo run -p qos-dev -- new repo-scan --template async
cargo run -p qos-dev -- new notes --template document
cargo run -p qos-dev -- new project-workspace --template workspace
```

Use `--template workspace` for a composed professional starting point rather
than an isolated widget example. It creates a responsive master-detail shell
with stable tree selection, a keyed detail view, and directly document-bound
fields for each item. Wide windows show both panes; compact windows switch
between full-width navigation and detail panes with an accessible Back action.
Switching items preserves independent edits, while the standard app document
supplies persistence, collaboration, inspection, and undo without app-owned
synchronization callbacks. The generated headless scenario exercises
navigation and editing, and the normal generated test covers wide and compact
geometry plus the initial accessibility audit.

After creation, the printed commands use shell-safe paths and include the
first test, development, run, saved-scenario, screenshot, inspection, and
package workflows.

Inside a qOS checkout the tool finds the workspace `ui` crate and registers
the new package automatically. It writes the complete app into a private
sibling staging directory, then publishes that directory and the workspace
registration as one rollback-safe operation. A validation, write, or
registration failure therefore leaves neither a partial app nor a stray
workspace member; an explicitly supplied empty destination is restored with
its original permissions if publication fails. Outside a checkout, the tool
discovers `ui` from `QOS_UI_PATH`, the running `qos-dev` binary's source tree,
or the checkout it was built from. The interactive wizard shows that path as
an editable default, validates it before the review screen, and re-prompts on
a bad path. For a pinned or relocated toolkit, set `QOS_UI_PATH` or pass
`--ui-path /path/to/quickdesktop/ui`; an explicit flag always wins.

Grow an existing app from any directory inside it with `add`:

```bash
qos-dev add
qos-dev add component profile-card --template form
qos-dev add component activity-card --mount main \
  --props '{"title":"Recent activity","detail":"Nothing pending"}'
qos-dev add component profile-form --template form --mount main \
  --props '{"value_path":"profile-name","enabled_path":"profile-enabled"}'
qos-dev add component project-tree --template tree
qos-dev add component activity-list --template list
qos-dev add component results-table --template table
qos-dev add component article-body --template text
qos-dev add screen account-settings --template settings --link-from main
qos-dev add screen new-project --template form --link-from account-settings
qos-dev add screen first-run --template empty-state
qos-dev add scenario settings-smoke --screen account-settings
qos-dev scenario run
qos-dev add locale de-DE
qos-dev locale sync
```

With no subcommand, `qos-dev add` opens a terminal wizard that discovers the
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
`TableModel<Row = Row>`, so ordinary text, custom-painted cells, and real
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
and prints the `qos-dev ui edit` command that opens it. The registry exposes a
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
runs it with `qos-dev scenario run`.

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
qos-dev locale sync
qos-dev locale sync de-DE --dry-run
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
cargo run -p qos-dev -- doctor apps/reading-list
```

`doctor` finds the app from any descendant path, parses the Cargo manifest,
resolves its `ui` dependency, validates `qos-app.toml` through the same package
schema used by installation, and checks the standard app-root and launch
lifecycle seams. It also runs `capnp --version`, because the shared UI runtime
generates its collaboration protocol while compiling; a missing compiler is
reported with an install-or-development-container remedy before Cargo starts.
It validates every serialized screen with the renderer's
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
qos-dev dev
qos-dev dev apps/reading-list --app-only
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
`--debounce-ms` to adjust the default 150 ms quiet period.

The generated app is not a toy lifecycle that must later be replaced. It uses
`ui::launch::LaunchSpec`, so the same root widget gets live startup, persisted
CRDT state, argument validation, and headless driving. It also starts with the
minimum sandbox declaration, a package manifest, and a runnable semantic app
test in `tests/app.rs`. Every generated template also includes an initial
accessibility-contract test, so ordinary `qos-dev test` fails on unlabeled
controls, invalid virtual-collection metadata, or unstable synthetic identity
without requiring a separate audit command. Interaction-specific states remain
covered by `qos-dev scenario run --audit`.
`starter` demonstrates a
complete local input-to-state-to-output loop; `data` begins with a selectable
`DataTable` with a 100-million-row sparse model, a real background page
request, stale-result suppression, targeted row notifications, and retained
cell controls;
`tree` begins with a
stable-id hierarchy, keyboard and multi-selection behavior, accessibility
semantics, a retained interactive row action, bounded virtualization, safe
subtree reparenting, and structural motion already supplied by `TreeView`;
`list` begins with a sparse 100-million-item `ListModel`, background page
loading, explicit loading/error delegates, stale-result suppression, measured
variable-height ready delegates, stable-key multi-selection, and an
independently interactive button inside each mounted row; `graph` emits a model-bound
`ui/main.json` screen that can be edited without rewriting the app lifecycle.
The `async` starter shows a complete button-to-worker flow using
`LatestTask` for automatic event-loop wakeup, cancellation, and stale-result
suppression, plus `ModelView::from_state_by` for keyed loading, success, and
failure subtrees without polling or string messages.
The `qr` starter demonstrates a compact generated-visual composition path: an
IME-capable input writes one typed `LocalState<String>`,
`ModelView::from_state` replaces only the generated `QrWidget`, and
payload-specific alternative text makes the output independently visible to
screen readers, inspection, and headless scenarios. No app-owned event bus or
extra image dependency is required.
The `document` starter is the durable counterpart: title and body inputs write
directly to the persisted Automerge model, live preview text reads those same
fields, and the generated headless contract proves an edit is synchronized
and undoable. It is the shortest starting point for user data that must survive
restarts or participate in collaboration.

Every ordinary `LaunchSpec` also reuses that live root for a screenshot; no
second screenshot-only tree is required. `shot` writes `preview.png` in the
app root by default:

```bash
qos-dev shot --viewport 1280x800
qos-dev shot --output artifacts/empty.png --scenario empty
```

Arguments after `--` are forwarded as app-defined screenshot options. Use
`shot_root` only when a deterministic scenario needs different seeded data,
or `shot_handler` for an app-specific capture pipeline. The default is the
production widget tree at the requested viewport and active shot theme.

Build an installable package from the same app root with:

```bash
qos-dev package
qos-dev package --identity publisher.json
qos-dev package --output dist/reading-list.qpkg --force
```

`package` runs the structural checks, builds every entrypoint declared in
`qos-app.toml` with Cargo's `release` profile, places the resulting binaries at
their declared payload paths, and asks `qos-pkg` to sign and verify the bundle.
The verified result is published as `dist/<cargo-package>.qpkg` by default.
An existing output is never replaced unless `--force` is present, and even
then replacement happens only after the new package verifies successfully.

Files under an app-local `payload/` directory are included before the built
entrypoints are overlaid. Symbolic links are rejected so package contents do
not depend on files outside the app. Without `--identity`, `qos-pkg` creates an
ephemeral self-signing key, which is useful for local testing; use one
persistent identity for updates and publication. `--profile`, `--target`,
`--features`, and `--no-default-features` expose the corresponding Cargo build
choices. Set `QOS_PKG` to the executable path when `qos-pkg` is not on `PATH`.

## The app shape

Keep the binary boring:

```rust
fn main() {
    ui::launch::run_or_exit(
        &ui::launch::LaunchSpec::new(
            "org.example.reading-list",
            reading_list::app_root,
        )
        .configure_app(|app| {
            app.autofocus_first = true;
            app.autofocus_on_structure_change = true;
        }),
    );
}
```

The library owns the application and returns `Box<dyn Widget>`. This boundary
is shared by the real event loop, screenshots, and the headless driver; it is
also the seam for deterministic app tests. Use toolkit widgets for controls so
focus, keyboard input, IME positioning, accessibility, theme behavior, and
testing metadata remain one implementation rather than app-local replicas.

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
            Button::new("cancel", "cancel-label", None),
            Button::new("save", "save-label", None).primary(),
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

Declare a semantic control once. Pointer, direct-touch tap, keyboard, and
accessibility activation all reach the control's existing event or semantic
action path and therefore the same callback or document binding:

```rust
Button::new("save", "app-save", Some("save-requested"))
    .primary()
    .fit_content(32.0)
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
      },
      {
        "type": "button",
        "id": "save",
        "label": "Save",
        "style": "primary",
        "on_click_path": "save-requested",
        "click_value": "yes"
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
qos-dev new project-maker --template graph
qos-dev ui init ui/settings.json --title "Settings"
qos-dev ui edit ui/settings.json
qos-dev ui tree ui/settings.json
qos-dev ui validate ui/settings.json
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
qos-dev add component profile-card --mount main \
  --props '{"title":"Ada","detail":"Workspace owner"}'
qos-dev add component profile-form --template form --mount main \
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
qos-dev ui component ui/main.json profile-card \
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
checkboxes. Both return precise field errors, and `qos-dev doctor` checks those
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
same lowercase kebab-case convention as `qos-dev add component`.

The center preview is the real `LiveUi` widget tree, so focus, keyboard input,
accessibility, bindings, layout, and theme behavior do not diverge from the
app. In a source checkout, `qos-dev` builds the workspace builder on first use.
Installed tools look for `qos-ui-builder` beside `qos-dev` and then on `PATH`;
set `QOS_UI_BUILDER` to override that executable.

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
qos-dev ui insert ui/settings.json --parent root --index 1 \
  --node '{"type":"button","id":"save","label":"Save","style":"primary"}'
qos-dev ui move ui/settings.json --id save --parent actions --index 0
qos-dev ui replace ui/settings.json --id save --node-file nodes/publish.json
qos-dev ui remove ui/settings.json --id obsolete-help
qos-dev ui set ui/settings.json --id title --property text.value --value "Account settings"
qos-dev ui set ui/settings.json --id panel --property fill_height --value true
qos-dev ui set ui/settings.json --id title --property size --unset
qos-dev ui apply ui/settings.json ui/settings.edits.json
qos-dev ui format ui/settings.json
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

Every command parses through the same lightweight `qos-ui-graph` schema used
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
    if let UiNode::Button { label, style, .. } = node {
        *label = "Publish".into();
        *style = UiButtonStyle::Danger;
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
actions go through `ActionHost`, so a button, command palette entry, agent
action, and undo history can invoke the same operation.

### One driveability surface

Every operation must be reachable through the resource/action catalogue. Use
`Widget::resources` and `ActionSpec` for domain operations: name the current
resource, publish document mutations as document actions, and publish I/O or
presentation work as effect/view operations. The host sends this same live
catalogue to Ctrl+K search, the `actions_enumerate` / `actions_invoke`
automation socket, and agents; none of those consumers has an app-specific
command path.

This is the qOS application API, not an optional automation adapter. A toolbar
button, menu item, shortcut, context-menu item, search result, script, and agent
must converge on the same command function. Put domain logic in that function;
make both the direct control callback and `invoke_resource_operation` call it.
Do not add a second socket, command enum, hidden test callback, or agent-only
implementation. CI runs `tools/check_app_resource_action_contract.py`, and the
runtime independently validates the concrete surface before publishing it.

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
- `retry`: use `idempotent` only when repetition is observably harmless;
  otherwise use the default `at_most_once`. The reserved
  `retry_with_invocation_id` mode must not be advertised until the calling
  transport supplies its own deduplication key. Never let a transport timeout
  silently repeat an at-most-once effect.
- `execution`: immediate, background, or streaming, with explicit cooperative
  cancellation. Long work reports monotonic progress and one terminal state;
  every streaming item is validated against the stream's declared JSON Schema.
- `discovery`: live, launchable, or durable. Instance operations normally stay
  live; an app-level action may opt into launchable discovery when it can run
  after the shell starts the installed app.
- `result`: no value, JSON checked by JSON Schema, a resource address, or the
  broker-owned job identity.

Use `ActionSpec::typed` and `ActionParam` for inputs. Parameters support text,
numbers, integers, booleans, objects, arrays, resource addresses, RFC 3339 date
times, nullable values, and redacted secrets. A deterministic JSON Schema
subset adds `type`, `enum`, `const`, numeric and length bounds, `pattern`, the
`date-time`/`uuid`/`uri` formats, array `items`, object `properties`/`required`,
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

A representative effect looks like this:

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

For a small app with one launchable resource, wrap its ordinary retained root
with `ui::widget::resource_actions::immediate` or `background`. The toolkit
owns the forwarding widget, operation dispatch, pending-job bookkeeping, and
failure conversion; the app supplies only its resource contract and operation
closure. `qos-dev new` uses this path and imports action types through
`ui::qos_actions`, so a generated project contains no copied host
implementation and needs no direct internal actions dependency.

Register document action decoders in `App::action_registry_mut`. Handle every
effect and view type in `Widget::invoke_resource_operation_v2`, return an
`ActionOutput`, and declare the locally handled type in
`Widget::local_resource_operation_handlers`; the legacy unit-returning hook is
only a compatibility adapter.
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

To verify the real external path, enumerate the running app through the test
control socket, invoke the returned `(provider, id)` with `actions_invoke`, and
assert the resulting document or effect. A successful invocation is never
inferred from a stale snapshot: the shell re-resolves the address and checks
its capability immediately before dispatch.

```bash
qos-control actions list --json
qos-control actions invoke app-actions "$ACTION_ID" \
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

std::thread::spawn(move || {
    worker_result.set(load_project().map_or_else(
        |error| LoadState::Failed(error.to_string()),
        LoadState::Ready,
    ));
});

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

let run = Button::new("run", "Run", None).on_click(move || {
    let completion = run_task.begin(LoadState::Loading);
    std::thread::spawn(move || {
        let value = load_project().map_or_else(
            |error| LoadState::Failed(error.to_string()),
            LoadState::Ready,
        );
        let _ = completion.complete(value);
    });
});
```

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
std::thread::spawn(move || {
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

### Localization

`qos-dev new` creates `locales/en-US.ftl` and an embedded locale registry used
from `app_root`. Add a catalog and wire it into that registry in one command:

```bash
qos-dev add locale de-DE
```

The generated `src/locales.rs` uses `include_str!`, so packaging cannot omit a
registered catalog. `qos-dev` only rewrites a registry carrying its generated
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
let save = Button::new("save", "save-project", None);
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
use ui::qos_actions::{ActionRegistry, OptimisticQueue};
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

`qos-actions` is independent of the renderer, window system, and OS services,
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
        std::thread::spawn(move || {
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
        Box::new(Button::new(&id, "Open", Some("open-result")))
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
Reserve `needs_poll` for sources that genuinely require continuous frames
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
            Button::new("message", "Message", None),
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

Use `qos-dev new activity-feed --template list` to start from the sparse-model
form instead. The generated `ActivityModel` reports 100 million stable keys
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
    std::thread::spawn(move || {
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
            Button::new("more", "More actions", None),
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

For a post-layout inspection without opening a window, write the widget and
accessibility tree directly. `inspect` writes `inspect.json` in the app root by
default; `--targets` additionally writes the addressable-widget index:

```bash
qos-dev inspect --viewport 1280x800 --targets targets.json
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
qos-dev inspect --actions tests/headless-actions.json --output after.json
qos-dev inspect --audit --audit-report target/audit/report.json \
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
qos-dev scenario run
qos-dev scenario run settings-smoke --viewport 1024x768
qos-dev scenario run --artifacts target/scenario-artifacts
qos-dev scenario run --audit --artifacts target/scenario-artifacts
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
Activating that target selects the exact part and fires the same callback or
pending choice as pointer input. These nodes advertise only actions they can
perform: unlike positioned virtual rows, an unpositioned drawn part does not
claim that it can scroll itself into view.

Collapsing a branch does not discard a selected descendant. Its semantic row
temporarily leaves the bounded accessibility window, while the tree's value
and model-key selection remain intact until it is expanded again.

Headless scenario objects are strict: misspelled fields and unknown actions
fail with a usage error instead of producing an apparently successful no-op.
