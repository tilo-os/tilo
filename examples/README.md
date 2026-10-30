# Repository examples

These workspace members demonstrate public Tilo interfaces without being
production services or desktop applications.

## Apps

Complete apps, each teaching one idea of the app framework: a model whose
fields views bind by path, verbs for everything else, forms for a verb's
parameters ([docs/app-development.md](../docs/app-development.md)).
Read one, run it
with `tilo-app run examples/apps/NAME`, or start your own app from it with
`tilo-app new my-app --from-example NAME`. `tilo-app examples` lists them.

- [`counter`](apps/counter/): a model, its verbs and a field edited at its path: validation, undo, a reset Undo takes back, a viewer role.
- [`bound-controls`](apps/bound-controls/): text, checkbox, number, slider and dropdown, each bound to a field by its path.
- [`virtual-list`](apps/virtual-list/): a hundred-million-row list that loads only the rows in view.
- [`data-table`](apps/data-table/): text and widget columns over a sparse table model.
- [`tree-browser`](apps/tree-browser/): tree navigation in an adaptive master-detail shell.
- [`background-task`](apps/background-task/): cancellable work on a worker thread, started through an action.
- [`shared-checklist`](apps/shared-checklist/): what merges when two people edit at once, what undo takes back, and a form for the next item.
- [`localized`](apps/localized/): English and German catalogs, arguments, plurals and the pseudo-locale.
- [`graph-screens`](apps/graph-screens/): serialized screens, a mounted Rust component and the screen router.
- [`qr-code`](apps/qr-code/): a generated visual with alternative text that starts empty.

## Agent trials

Trial packs an operator installs on a hosted desktop for someone else's AI
agent to work through with semantic actions, each with its documents, task
cards and a checker.

- [`agent-trials/documents-and-forms`](agent-trials/documents-and-forms/): fill and arrange PDFs, correct a letter, and complete a multi-step application on a mock portal.

## Packaging

- `packaging/hello` exercises the native application runtime, sandbox, portals,
  package installation, and optional thin-runtime linkage.
- `packaging/notes-cli` is the reference command-line application and package
  manifest used by the CLI packaging walkthrough.

Keeping examples outside `crates/` makes the production crate map describe
shipped libraries and services only. The examples remain workspace members so
dependency updates and normal checks still compile them.
