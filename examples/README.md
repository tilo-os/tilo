# Repository examples

These workspace members demonstrate public Tilo interfaces without being
production services or desktop applications.

- `packaging/hello` exercises the native application runtime, sandbox, portals,
  package installation, and optional thin-runtime linkage.
- `packaging/notes-cli` is the reference command-line application and package
  manifest used by the CLI packaging walkthrough.

Keeping examples outside `crates/` makes the production crate map describe
shipped libraries and services only. The examples remain workspace members so
dependency updates and normal checks still compile them.
