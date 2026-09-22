# Installation: `tilo-install` and the on-disk layout

`app` is the user-facing package manager; `store` is an exact alias. `tilo app`
and `tilo store` fan out to the same command, so these are equivalent:

```sh
app u browser
store update browser
tilo app update browser
```

`tilo update` updates apps and, when the provider exists, the Tilo system image.
On another Linux distribution it skips unavailable system management rather than
making the app commands unusable. Explicit `tilo system ...` commands report that
the provider is unavailable.

The lower-level `tilo-install` verifies signatures, unpacks payloads, drops PATH
shims, and launches. All state
lives under `$XDG_DATA_HOME` and `$XDG_CONFIG_HOME`; no root, no system-
wide side effects (except the packages themselves, which never need root).

## Verbs

```
$ tilo-install --help
Install, launch, list, or remove Tilo packages.

Usage: tilo-install <COMMAND>

Commands:
  install  Install a .tapp into the local store. Verifies signature first
  update   Fetch a newer version and keep the app's per-room state
  list     List installed packages
  remove   Remove an installed package by id
  run      Launch an installed package via tilo-launcher
  inspect  Print manifest + install info for an installed package
  trust    Manage the trusted-attestor registry
  revoke   Revoke a publisher key
  revoked  List revoked keys
  path     Print the directory the shim installer writes PATH shims into
  runtime  Manage installed Tilo runtime bundles
```

`add` is an alias for `install`, so `tilo-install add ./app.tapp` is accepted
as an alternative spelling.

For catalogue apps, use `app install <package-id>` (or its `add`
alias). It resolves the configured repositories and pins both the expected
publisher and package-root hash when handing the download to `tilo-install`.
Repository management is available without hand-editing TOML:

```sh
app repo list
app repo add team https://apps.tilo.so --owner-key <64-hex-key>
app repo disable team
app repo enable team
app repo remove team
```

Changes are atomically written to the same `registries.toml` used by the
Store. Repositories retain list order as their resolution priority.

## Stable and development builds

Catalogue repositories default to `channel = "stable"`, including existing
configuration files without that field. Stable selection excludes SemVer
prereleases; numeric CalVer and older opaque versions keep their prior behavior.
Use `app repo channel NAME development` to opt into prereleases, or
`app repo channel NAME stable` to return to stable selection. This
changes future offers; it does not silently downgrade an installed development
build. Stable promotion requires publishing the corresponding stable version.

Development builds derive `0.1.1-dev.N+gSHA` from declared app version `0.1.0`.
`N` is the commit's position in complete authoritative main first-parent history;
shallow histories and side-branch sources are rejected. SHA identifies the source
but never orders updates. Retrying the same source derives the same identity;
changed bytes under that identity are rejected. A new source advances the numeric
sequence. Explicit stable tags retain the declared manifest version, and stable
clients continue to see `0.1.0` even when development clients see `0.1.1-dev.N`.
Publishing stable `0.1.1` supersedes its development prereleases in both channels.

`dist/apps/package-apps.sh` enables this derivation only with
`TILO_APP_DEV_SOURCE=<full main SHA>`. It stages the version inside the signed
package and collection index. Partial collections retain each unchanged app's
previous artifact version. The app workflow sets this flag for main pushes and
explicitly publishing manual main runs. Tags keep stable versions; nonpublishing
qualification keeps its existing manifest versions. Artifacts still upload to
Forgejo packages; the authoritative main-branch publisher submits newly built
tapps through its direct signed-registry path, without a second
catalogue-submission job.

These settings govern the signed snapshot catalogue and `app install`.
The legacy `tilo-install update` registry resolver uses a separate configuration
and protocol; it does not inherit this catalogue channel setting.

## The on-disk layout

```
$XDG_DATA_HOME/                     # default: ~/.local/share
  tilo-apps/
    registry.toml                   # one entry per installed package
    payloads/
      <pkg-id>/                     # unpacked payload
        manifest.toml
        bin/…                       # entrypoint(s)
        …                           # everything from the payload/ tree
    bin/                            # PATH-shim install root
      <command>                     # copy of tilo-cli-shim
      <command>.tilo-shim            # sidecar (TOML)

  tilo-runtime/
    <version>/                      # e.g. 2026.07
      bundle.toml                   # manifest: version, root_hash, files[]
      lib/
        ld-linux-x86-64.so.2
        libc.so.6
        libgcc_s.so.1
        …
      etc/ld.so.cache

  tilo-rooms/                        # written by the launcher, not the installer
    <room>/                         # workspace id, or `personal` for a launch with no workspace
      <pkg-id>/                     # the app's one writable directory
        .local/share/…              # HOME and every XDG_* point in here

$XDG_CONFIG_HOME/                   # default: ~/.config
  tilo/
    trusted-attestors.toml
    revoked-keys.toml
```

Override `$XDG_DATA_HOME` per-app for tests, or set `TILO_APPS_DATA_DIR`
to override just the tilo-apps subtree (`tilo-launcher::registry` reads
the same var).

### The system store

An OS image ships a second store, in the same shape, at
`/usr/share/tilo-apps/`:

```
/usr/share/tilo-apps/
  registry.toml
  payloads/<pkg-id>/…
  bin/…
```

It is written during the image build by `os/scripts/pack-apps.sh`, which
runs the ordinary `tilo-app-package pack` + `tilo-install install` against a
`TILO_APPS_DATA_DIR` pointed there — so every preinstalled package went
through the same packing, signing and install path a user's own package
goes through, and is described by the same `RegistryEntry` fields. It
exists so a fresh install has apps to list before anyone has run
`tilo-install`.

`tilo-launcher::registry::list_installed` reads both stores, system first,
and a user entry **shadows** a system entry with the same id — installing
your own build of a preinstalled app replaces it in the launcher rather
than producing a duplicate row. `TILO_APPS_SYSTEM_DIR` overrides the
system path the same way `TILO_APPS_DATA_DIR` overrides the user one.

Being preinstalled grants a package nothing. The sandbox is still built
from its manifest, the payload is still bound read-only, and the one
writable directory still comes from `tilo-rooms/` — there is no
first-party branch anywhere in the launcher. The store is read-only only
because it lives inside an immutable image; `tilo-install` writes to the
user store, so removing or replacing a preinstalled app is a normal
per-user install and needs no write access to `/usr`.

`tilo-rooms/` is the app-data side and belongs to the launcher: it creates
`<room>/<pkg-id>` on first launch and binds it at `/run/tilo/app`. Two rooms
are two directories, and no manifest field selects either. See
[`sandbox.md`](./sandbox.md).

### `registry.toml`

```toml
[[entry]]
id                = "so.tilo.hello"
version           = "0.1.0"
package_id_hex    = "b8bde92e…"
payload_root      = "/…/tilo-apps/payloads/so.tilo.hello"
manifest_path     = "/…/tilo-apps/payloads/so.tilo.hello/manifest.toml"
publisher_key_hex = "c4c627b5…"
pinned            = false
installed_at      = 1783176312
attested_by       = ""                 # empty for self-signed
cli_commands      = ["notes"]          # PATH commands registered
```

`payload_root` uses the reverse-DNS `id`, not the content hash — updates
overwrite the same directory. If you need side-by-side versions (multiple
`0.1.0` and `0.2.0` of the same package), install into a distinct
`XDG_DATA_HOME` per version.

## `install`

```
tilo-install install <tapp>
    [--pinned]                      # retained across size-pressure GC
    [--force]                       # overwrite same-id install
    [--require-package-root <hex>]  # bind to a signed registry version
    [--require-publisher <hex>]     # bind to that registry's publisher
    [--require-attestor <id>]       # refuse unless signed by this attestor
    [--allow-self-signed]           # permit self-signed as fallback
```

Sequence:

1. Read the `.tapp` tar into an in-memory blob store.
2. Verify the signed root and every hash. If registry constraints were
   supplied, require both the advertised package root and publisher key;
   this prevents a valid package from another release or publisher being
   substituted after catalogue resolution. Apply trust policy (see
   [`trust.md`](./trust.md)).
3. If a package with the same `[package].id` is already installed,
   refuse unless `--force`.
4. Remove any old `payloads/<id>/` directory.
5. Unpack payload into `payloads/<id>/`. Write `manifest.toml` next to
   it explicitly (so the launcher can find it without re-reading the
   `.tapp`).
6. If the manifest has `[cli].commands`, drop a shim + sidecar in
   `bin/` per command.
7. Append a `RegistryEntry` to `registry.toml`.

Example run against the hello demo:

```
$ tilo-install install /tmp/tilo-hello-demo/tilo-hello.tapp
tilo-install: WARN — package self-signed by ed25519:c4c627b57… (no trusted-attestor signature); use --require-attestor <id> to demand an attestation
installed: so.tilo.hello 0.1.0
  payload  : /tmp/tilo-install-demo-data/tilo-apps/payloads/so.tilo.hello
  pinned   : false
run with:  tilo-install run so.tilo.hello
```

## `list`

```
$ tilo-install list
id                                version   pinned  attested-by         installed
so.tilo.hello                     0.1.0     no      -                   0s ago
```

## `inspect`

```
$ tilo-install inspect so.tilo.hello
id            : so.tilo.hello
version       : 0.1.0
package-id    : tapp:b8bde92e2352dd7a3b3e094968272c42d51196697a5cec17f36e3595a55a71d3
publisher     : ed25519:c4c627b57a518bc318c2854662b44f741345f6d55a45eb059d3b384048789e39
payload-root  : /tmp/tilo-install-demo-data/tilo-apps/payloads/so.tilo.hello
manifest      : /tmp/tilo-install-demo-data/tilo-apps/payloads/so.tilo.hello/manifest.toml
pinned        : false
attested-by   : (self-signed)
installed     : 0 seconds ago
```

## `run`

```
tilo-install run <pkg-id>
    [--no-services]                 # skip service-fd resolution
    [--allow-missing-runtime]       # fall back to --minimal-libs if runtime bundle absent
    [--require-attestor <id>]       # re-check attestation at launch
    [--allow-self-signed]
```

Re-verifies the trust policy at launch (revocations may have arrived
since install), then `exec()`s `tilo-launcher run --manifest <path>
--payload-root <path>`, which forks bwrap and starts the sandbox flow
described in [`sandbox.md`](./sandbox.md).

Because `run` uses `exec()`, this process's exit code is the launcher's.

## `remove`

```
$ tilo-install remove so.tilo.hello
removed: so.tilo.hello
```

Deletes the payload dir, un-writes any registered PATH shims, and drops
the registry entry.

It does not touch the app's data. `<data home>/tilo-rooms/<room>/<pkg-id>`
survives removal, one directory per room the app was opened in; deleting
those is a separate, deliberate act.

## `path`

Prints the PATH-shim install directory + a snippet the user can drop
into their shell rc:

```
$ tilo-install path
/tmp/tilo-install-demo-data/tilo-apps/bin

# Prepend the above to your shell's PATH so CLI commands
# installed via `tilo-install install <pkg.tapp>` are picked up.
#
# bash / zsh (~/.bashrc, ~/.zshrc):
export PATH="/tmp/tilo-install-demo-data/tilo-apps/bin:$PATH"
#
# fish (~/.config/fish/config.fish):
fish_add_path -pg /tmp/tilo-install-demo-data/tilo-apps/bin
```

## `runtime`

Manages the shared `tilo-runtime/<version>` bundles. See
[`packaging.md`](./packaging.md) for what a runtime bundle is
(essentially: ld-linux + libc + libgcc_s + friends, content-addressed).

```
$ tilo-install runtime install --version 2026.07 --from-host --force
installed runtime bundle: tilo-runtime/2026.07
  path     : /tmp/tilo-install-demo-data/tilo-runtime/2026.07
  files    : 11
  root-hash: blake3:1e7dedc8881ab7e8225b0c680e832e2332e3b1c14406f34fd1b2a70edec3e58d

$ tilo-install runtime list
version               path
2026.07               /tmp/tilo-install-demo-data/tilo-runtime/2026.07

$ tilo-install runtime verify 2026.07
verified: tilo-runtime/2026.07
  path     : /tmp/tilo-install-demo-data/tilo-runtime/2026.07
  files    : 11
  root-hash: blake3:1e7dedc8881ab7e8225b0c680e832e2332e3b1c14406f34fd1b2a70edec3e58d

$ tilo-install runtime path --version 2026.07
/tmp/tilo-install-demo-data/tilo-runtime/2026.07

$ tilo-install runtime remove 2026.07
removed runtime bundle: tilo-runtime/2026.07
```

`--from-host` copies from the running host root; `--from <path>` copies
from an explicit prefix (e.g. a distrobox rootfs). The bundle is
content-addressed by BLAKE3 root of the sorted `(path, blake3(bytes))`
pairs — any byte change flips the root hash.

Discovery order the launcher uses:

1. `$XDG_DATA_HOME/tilo-runtime/<version>/` — per-user (installed via
   `tilo-install runtime install`).
2. `/usr/lib/tilo-runtime/<version>/` — system-wide (baked into an OS
   image).

## Updates

There is no in-place upgrade command yet. Update = `install --force
<new.tapp>`:

- Registry entry retains the same `id`.
- Old payload dir is deleted, new one written.
- Old PATH shims are un-registered, new ones written.
- `installed_at` refreshes.

The `package_id_hex` (BLAKE3 content hash) changes — that's how the
integrity check catches update tampering. The user-visible
`[package].version` field is unopinionated: publishers set semver and
the installer doesn't compare versions. `--force` is the only path to
overwrite; a mistaken `install` without `--force` on a same-id package
refuses:

```
tilo-install: package so.tilo.hello already installed (use --force to overwrite)
```

## Trust management

See [`trust.md`](./trust.md) for the full model. Quick reference:

```
$ tilo-install trust add --id "tilo-curated/2026.07" --pubkey <hex> --scope "all"
$ tilo-install trust list
$ tilo-install trust remove <id>

$ tilo-install revoke <pubkey_hex> --reason "compromised 2026-Q3"
$ tilo-install revoked list
```

Next: [`trust.md`](./trust.md) — publisher identities, attestation
registry, revocation.
