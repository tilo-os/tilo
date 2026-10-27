# CLI apps

A Tilo CLI app is a package with `entrypoint.type = "cli"` and (typically)
a `[cli]` block listing the commands it publishes on the user's PATH.

## The `[cli]` block

```toml
[entrypoint]
bin  = "bin/notes"
type = "cli"

[cli]
commands = ["notes", "notes-import"]
```

At install time, `tilo app install` walks `[cli].commands` and drops
one copy of `tilo-cli-shim` per entry into
`~/.local/share/tilo-apps/bin/`. A sidecar TOML (`<command>.tilo-shim`)
holds the mapping back to the package id and command name.

## What the user sees

```
$ tilo-install path
/tmp/tilo-install-demo-data/tilo-apps/bin

# Prepend the above to your shell's PATH so CLI commands
# installed via `tilo app install <pkg.tapp>` are picked up.
#
# bash / zsh (~/.bashrc, ~/.zshrc):
export PATH="/tmp/tilo-install-demo-data/tilo-apps/bin:$PATH"
#
# fish (~/.config/fish/config.fish):
fish_add_path -pg /tmp/tilo-install-demo-data/tilo-apps/bin
```

The user prepends the printed dir to their PATH once. From then on, every
`tilo app install …` that ships a `[cli].commands` entry adds fresh
PATH commands automatically. `tilo app remove` un-writes the shims.

## What happens on `notes "hello world"`

```text
   $ notes "hello world"
        │
        ▼
   ~/.local/share/tilo-apps/bin/notes   ← copy of tilo-cli-shim (~880 KB)
        │  read sidecar notes.tilo-shim (TOML: package_id, command, launcher)
        ▼
   /usr/libexec/tilo-launcher run-installed --package so.tilo.notes-cli
                                            --command notes
                                            -- "hello world"
        │  fork + bwrap + Landlock (see sandbox.md)
        │  setenv TILO_CLI_COMMAND=notes
        ▼
   /var/tilo-apps/so.tilo.notes-cli/bin/notes "hello world"
        │
        ▼
   cli::run_main(|| { … })
        └── write to workspace path (or fallback dir if standalone)
```

The shim is one binary reused for every command; the sidecar is what
picks the launcher and the package id at exec time.

### The sidecar

```toml
# ~/.local/share/tilo-apps/bin/notes.tilo-shim
package_id = "so.tilo.notes-cli"
command    = "notes"
launcher   = "/tmp/tilo-tools/target-docs/release/tilo-launcher"  # dev override
```

The `launcher` line is optional. In production installs it is unset, and
the shim resolves to `/usr/libexec/tilo-launcher`. It exists so integration
tests can install shims that route through a `target/release/tilo-launcher`
without root.

## The `cli` SDK

CLI apps typically depend on the `cli` crate, which layers
CLI-specific ergonomics on top:

```rust
use cli::{run_main, resolve_workspace_path, is_tilo_native};

fn main() -> std::process::ExitCode {
    run_main(|| {
        let path = resolve_workspace_path("notes/inbox.md")?;
        std::fs::write(path, "hello\n")?;
        Ok(())
    })
}
```

Public API:

```rust
pub use runtime_portal::NotAvailable;
pub use runtime_portal::is_tilo_native;
pub use output::{OutputBuilder, output};
pub use workspace::{
    /* WorkspaceHandle, is_workspace_available, workspace_root, resolve_workspace_path */
};

pub fn invoked_command() -> Option<String>;   // TILO_CLI_COMMAND
pub fn run_main<F: FnOnce() -> anyhow::Result<()>>(f: F) -> ExitCode;
pub fn package_id() -> String;                // TILO_PACKAGE_ID with fallback
pub fn println_terminal_aware(text: &str) -> io::Result<()>;
pub fn ensure_dir(dir: &Path) -> Result<()>;
pub fn standalone_state_dir(pkg_id: &str) -> PathBuf;
```

`run_main` wraps your closure so that `anyhow::Error` becomes a non-zero
exit with the error on stderr, and a broken pipe (`| head`) exits cleanly
instead of panicking.

`resolve_workspace_path("<rel>")` picks one of two roots:

1. **Inside Tilo**: `/var/workspace/<rel>` — the FUSE-mounted workspace.
2. **Standalone**: `~/.local/share/<package-id>/<rel>` — a per-package
   fallback state dir. Uses `dirs::data_dir()` so `$XDG_DATA_HOME` moves
   it if set. Under the launcher it is set, to
   `/run/tilo/app/.local/share`, so the fallback lands in the directory
   the app was given.

The two paths are deliberately unrelated to `runtime_fs` so the CLI keeps
working when the FUSE is offline. If your app needs the CRDT-backed
workspace, wire `runtime_fs::try_connect()` explicitly; if it only needs
a place to put its own state, use `resolve_workspace_path`.

## Walkthrough: notes-cli

The reference CLI shipped in-tree is `examples/packaging/notes-cli/`. Its
manifest:

```toml
[package]
id      = "so.tilo.notes-cli"
version = "0.1.0"
kind    = "app"

[publisher]
identity  = "ed25519:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
signature = ""

[sandbox]
namespaces = ["user", "pid", "uts", "ipc", "mount", "net"]
seccomp    = "tilo-default"
landlock   = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin  = "bin/notes"
type = "cli"

[cli]
commands = ["notes"]
```

The manifest declares no filesystem path, and does not need to: the
launcher creates the app's directory and points `HOME` and the `XDG_*`
variables at it before the binary runs.

Install and run:

```
$ tilo package pack \
    --manifest examples/packaging/notes-cli/manifest.toml \
    --payload  demo/payload \
    --output   demo/notes-cli.tapp
packed tapp:36c777e279b33bd1c7ca6ee8a5a7033e0e500abfc5e2262c2b2b0476a9f33eca (4 blobs)

$ tilo app install demo/notes-cli.tapp
tilo-install: WARN — package self-signed by ed25519:… (no trusted-attestor signature); use --require-attestor <id> to demand an attestation
  shims    : wrote 1 shim(s) into /tmp/tilo-install-demo-data/tilo-apps/bin
  PATH     : prepend /tmp/tilo-install-demo-data/tilo-apps/bin to your shell's PATH to pick them up (see `tilo-install path`).
installed: so.tilo.notes-cli 0.1.0
  payload  : /tmp/tilo-install-demo-data/tilo-apps/pkgs/<package-id>
  pinned   : false
run with:  tilo-install run so.tilo.notes-cli

$ ls -la /tmp/tilo-install-demo-data/tilo-apps/bin/
-rwxr-xr-x. 1 you you 886728  4. Jul 16:45 notes
-rw-r--r--. 1 you you    158  4. Jul 16:45 notes.tilo-shim

$ export PATH="/tmp/tilo-install-demo-data/tilo-apps/bin:$PATH"

$ notes "hello from the shim"
tilo-sandbox-exec: landlock: partially enforced (kernel ABI dropped some rights) (12 ro, 3 rw, net_restricted=true)
wrote note → [file:///run/tilo/app/.local/share/so.tilo.notes-cli/notes/inbox.md]
```

That path is not a coincidence and nothing in the manifest picked it.
`standalone_state_dir` joins the package id onto `dirs::data_dir()`, which
reads `XDG_DATA_HOME` — and the launcher set that to
`/run/tilo/app/.local/share`. There is no directory to create before launch
and no bind to get wrong; the launcher makes the app's directory and then
starts the binary.

On the host the same file is at
`<data home>/tilo-rooms/<room>/so.tilo.notes-cli/.local/share/so.tilo.notes-cli/notes/inbox.md`.
A CLI run from a shell has no workspace, so its room is `personal`.

## Reading arguments

Your `[cli].commands` entries all resolve to the same entrypoint binary.
Distinguish them with `cli::invoked_command()`:

```rust
match cli::invoked_command().as_deref() {
    Some("notes")        => append_note(),
    Some("notes-import") => import_notes(),
    _                    => usage(),
}
```

The launcher forwards everything after `--` to your `argv[1..]` verbatim,
so `clap` / `structopt` / a hand-rolled parser all work as-is.

## `type = "both"` — GUI and CLI in one package

```toml
[entrypoint]
type    = "both"
gui-bin = "bin/notes-gui"
cli-bin = "bin/notes"

[cli]
commands = ["notes"]
```

`tilo-install run <pkg>` execs `gui-bin`. The PATH shim execs `cli-bin`.
Both binaries live inside the same signed payload and share the same
sandbox.

Next: [`formats.md`](./formats.md) — sandboxed WebAssembly providers for file
formats.
