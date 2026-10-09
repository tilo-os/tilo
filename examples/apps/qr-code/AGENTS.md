# Working on this Tilo app

Start with `src/lib.rs`, `tests/app.rs`, and `tilo-app.toml`. `README.md`
contains this project's longer workflow. Use `tilo-app --help` or a command's
`--help` for flags; the SDK revision is pinned by the project's dependencies.

## Make a feature

1. Put durable state in the `#[derive(Model)]` resource. Give collection items
   stable ids. Widget structs hold visual caches, not a second copy of app state.
2. Edit fields at their typed paths and make everything else an `#[action]`
   verb on the `#[resource]` implementation; never write a setter verb. Bind the
   view through `ViewCx`: `cx.input(p.title)`, `cx.checkbox(key, p.done)`,
   `cx.control(make, p.level)` for fields, `cx.button(key, Verb.with(..))` for
   verbs, `cx.form::<Params>()` for a verb's parameters, and `cx.each` for
   collections (`let p = cx.paths()`). A click handler alone is not the app's
   public API: accessibility, automation and collaborators use the same actions.
   Commit the public surface with `tilo-app surface`.
3. Use shared widgets and the theme. Put visible strings in `locales/*.ftl`,
   and pass Fluent message ids to controls. Start empty; keep sample data in
   tests or explicit demos. Tilo OS owns window chrome.
4. Extend `tests/app.rs`. `Replica` tests actions without a window; `TestApp`
   drives the production resource and view. Check the visible result, undo,
   reopening, and refusal of edits by a viewer where relevant.

Storage, history, per-author undo/redo, permissions and collaboration belong
in the resource runtime. Keep edits on that path so they can be attributed,
merged and replayed. Device sync uses the host's collaboration transport;
do not invent app-local identities, a second database or a permission bypass.
An agent is an ordinary collaborator subject to the same roles.
For a short, offline workflow with runnable peer-edit and undo tests, use
`tilo-app guide collaborate --json` (also `guide undo`, `history` or `sync`).
For live roles, permission-aware panes and refused actions, use
`tilo-app guide permissions --json` (also `guide roles` or `access`).

## Work and verify

Run these commands from any directory inside the app:

```sh
tilo-app dev                       # rebuild on save; retain state after reload
tilo-app test                      # resource and production-view tests
tilo-app inspect                   # headless widget/accessibility tree
tilo-app shot                      # headless image of the production root
tilo-app check                     # quality gate, including audited scenarios
```

A failed rebuild leaves the last good app running. Use `tilo-app add --help`
for components, screens, scenarios, translations, capabilities and CI.
Declare host capabilities in `tilo-app.toml`; keep optional capabilities
optional and handle unavailable hosts through the SDK.

For automation, append `--json`: stdout is one result document, tool progress
is stderr, and commands do not prompt. Inspect `ok`, the exit status, and
`result`; a failed quality verdict still contains its findings. Keep the SDK,
toolchain and lockfile pinned. Run the relevant tests and `tilo-app check`
before publishing; `tilo-app package --help` and `tilo-app publish --help`
describe packaging and registry submission.
