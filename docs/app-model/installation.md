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
making the app commands unusable. Explicit `tilo device ...` commands report that
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
app repo add offline file:///srv/tilo-registry --owner-key <64-hex-key>
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
  tilo-apps/                        # the package store (crates/packaging/package-store)
    registry.toml                   # one entry per installed package
    content/                        # the content store holding payload blobs
    pkgs/
      <package-id>/                 # materialised payload tree, read-only files
        bin/…                       # entrypoint(s)
        …                           # everything from the payload/ tree
      <package-id>.pkg/             # signed-root, manifest.toml, collection.qcl
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

Payload bytes live once, in the content store; each tree under `pkgs/`
is materialised from it by reflink where the filesystem supports it
(Btrfs, XFS), so tree and store share extents rather than holding two
copies. A tree is named by its signed package id, so an update installs
the new tree beside the old one and replaces the registry row; blobs no
installed package references are then evicted. The launcher, the shell,
the app host and `tilo-install` all read and write this one store.

Override `$XDG_DATA_HOME` per-app for tests, or set `TILO_APPS_DATA_DIR`
to override just the tilo-apps subtree (`package-store` reads the same
var).

### The system store

An OS image ships a second store, in the same shape, at
`/usr/share/tilo-apps/`:

```
/usr/share/tilo-apps/
  registry.toml
  pkgs/<package-id>/…
  pkgs/<package-id>.pkg/…
  bin/…
```

It is written during the image build by `os/scripts/pack-apps.sh`, which
runs the ordinary `tilo app install` against a `TILO_APPS_DATA_DIR`
pointed there — so every preinstalled package went through the same
verification and install path a user's own package goes through, and is
described by the same `InstalledPackage` fields. A release image installs
the app collection Release / apps published: the very `.tapp` files the
registry serves, signed by the registry's publisher, at the registry's
versions. A development image whose apps changed since the last published
collection packs its own commit's apps instead; the registry's newer
releases still update them.
The build drops the content store afterwards: the image's store is
read-only, never installs again, and its trees are already materialised.
It exists so a fresh install has apps to list before anyone has run
`tilo-install`.

The image's apps are installed packages like any other: `app list
--installed` lists them (from `image`), and `app update <name>`, `app
update`, `tilo update` and the App Store's **Update** / **Update all**
update them from the registry with the same signature and trust chain a
user's own install gets (the pinned registry owner key, the publisher the
signed entry names, attestations). An update installs the newer version
into the user store, where it **shadows** the image's copy;
`package_store::list_installed` reads both stores, system first, and lists
the copy that runs. Updating an app never needs an image switch or a reboot.
All of them read one list of repositories, `$XDG_CONFIG_HOME/tilo-store/registries.toml`,
which `app repo` edits; the sandboxed App Store reads it through the app host
(`repositories`), so it keeps no copy of its own.

Which copy runs (`package_store::runs_over_image`):

- the newer of the two. An image update that ships a newer build of an app
  than the user updated to wins, so a stale user copy never hides a fix
  the image carries; the next `app update` removes that copy. An image
  update with an older build does not take a user-updated app back.
- except a copy the user went back to on purpose: `app rollback` below the
  image's version (or a forced install of an older build) marks the user
  row `held`, and it runs whatever the image ships until the next update.
- `app rollback` with no older published version between, or `--to` the
  image's version, removes the user copy, which returns to the image's.
  `tilo app remove` does the same; the image's copy itself cannot be
  removed.
- never a user copy that requires a system interface version this OS does
  not serve (`[sandbox.services]` `required@N`, against
  `/usr/share/tilo/system-interfaces.toml`; see
  [packaging.md](./packaging.md#sandboxservices)). Such a copy is never
  offered or installed, but one installed for a newer OS that was then
  rolled back stays in the user store, and the image's copy runs until the
  system serves it again or the copy is removed (`app remove <name>`). The
  image's copy is built with the image, so it always fits.

In a developer build (debug, or `--features dev-overrides`),
`TILO_APPS_SYSTEM_DIR` overrides the system path the same way
`TILO_APPS_DATA_DIR` overrides the user one; a release build ignores it and
always reads the image's store.

What is not an app, and so updates only with the image: the compositor
(`shell`), the launcher and sandbox (`tilo-launcher`, `tilo-app-host`), the
package tools themselves (`tilo-install`, `app`, `tilo`), the greeter,
installer, unlock helper and system services, and the runtime bundle
(`/usr/lib/tilo-runtime/<v>`) apps link against. These decide what an app
may do or verify what it is, so a package must not be able to replace
them. Every package in `/usr/share/tilo-apps`, Settings and Files
included, is an app and updates per app.

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
payload_root      = "/…/tilo-apps/pkgs/b8bde92e…"
manifest_path     = "/…/tilo-apps/pkgs/b8bde92e….pkg/manifest.toml"
publisher_key_hex = "c4c627b5…"
pinned            = false
installed_at      = 1783176312
last_used         = 1783176312         # orders size-pressure eviction
attested_by       = ""                 # empty for self-signed
cli_commands      = ["notes"]          # PATH commands registered
```

There is one row per `id`; `payload_root` names the tree by the signed
package id, so a row can never point at another package's files. If you
need side-by-side versions (multiple `0.1.0` and `0.2.0` of the same
package), install into a distinct `XDG_DATA_HOME` per version — their
shared files are still stored once per store. Rows written before the
content store existed (`payloads/<id>`) are moved into it once,
idempotently, the next time `tilo-install` or the app host runs.

Size-pressure eviction goes least recently used first, by the later of
`last_used` (written when the app opens) and the modification time of the
tree's launch lease under `leases/`, which the launcher sets again when the
app closes, on the tree the app's row names then too when an update
replaced the one it ran on; an app kept open for weeks counts as used the
day it closed. It never evicts a pinned app, an app a running sandbox has
mounted, an app a launch holds (including one still running a build an
update replaced), or an app the change it follows just installed or
updated: a store whose kept apps alone exceed the cap stays over it.

## `install`

```
tilo app install <tapp>
    [--pinned]                      # retained across size-pressure GC
    [--force]                       # overwrite same-id install
    [--require-package-root <hex>]  # bind to a signed registry version
    [--require-publisher <hex>]     # bind to that registry's publisher
    [--require-attestor <id>]       # refuse unless signed by this attestor
    [--allow-self-signed]           # permit self-signed as fallback
    [--payload-from <registry-url>] # fetch blobs a thin .tapp lacks
```

Sequence:

1. Stream the `.tapp` through the format codec, checking the metadata
   hash chain and every carried payload blob.
2. Verify the signed root. If registry constraints were
   supplied, require both the advertised package root and publisher key;
   this prevents a valid package from another release or publisher being
   substituted after catalogue resolution. Apply trust policy (see
   [`trust.md`](./trust.md)).
3. If a package with the same `[package].id` is already installed,
   refuse unless `--force`.
4. Put payload blobs into the content store. Blobs a thin `.tapp` does
   not carry are looked up in the store first and fetched from
   `--payload-from` only when missing; each is checked against its hash.
5. Materialise `pkgs/<package-id>/` from the store and write the signed
   record to `pkgs/<package-id>.pkg/` (the launcher reads its
   `manifest.toml`).
6. Replace the package's row in `registry.toml` atomically — the commit
   point — then remove a replaced version's tree and evict blobs no
   installed package references.
7. If the manifest has `[cli].commands`, drop a shim + sidecar in
   `bin/` per command.

Example run against the hello demo:

```
$ tilo app install /tmp/tilo-hello-demo/tilo-hello.tapp
tilo-install: WARN — package self-signed by ed25519:c4c627b57… (no trusted-attestor signature); use --require-attestor <id> to demand an attestation
installed: so.tilo.hello 0.1.0
  payload  : /tmp/tilo-install-demo-data/tilo-apps/pkgs/b8bde92e…
  stored   : 1 blob(s), 0 already in the content store
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
$ tilo app inspect so.tilo.hello
id            : so.tilo.hello
version       : 0.1.0
package-id    : tapp:b8bde92e2352dd7a3b3e094968272c42d51196697a5cec17f36e3595a55a71d3
publisher     : ed25519:c4c627b57a518bc318c2854662b44f741345f6d55a45eb059d3b384048789e39
payload-root  : /tmp/tilo-install-demo-data/tilo-apps/pkgs/b8bde92e…
manifest      : /tmp/tilo-install-demo-data/tilo-apps/pkgs/b8bde92e….pkg/manifest.toml
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
$ tilo app remove so.tilo.hello
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
# installed via `tilo app install <pkg.tapp>` are picked up.
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

The runtime thin apps link is named by its build, not only its release:
`tilo-runtime/2026.07+native.<abi digest>`, where the digest covers the
bundle's payload (`libruntime_dylib.so`, the Rust `libstd-<hash>.so`, the
loader set). Those Rust dylibs have no stable ABI, so a packaged app names the
exact build it was linked against (packaging stamps it over the authored
`tilo-runtime/2026.07`), and `tilo-install` refuses an app whose build is not
installed. The registry offers each build as the `so.tilo.runtime` package
(`kind = "runtime"`, `package.provides = "tilo-runtime/<build>"`), whose large
libraries travel as content-addressed, chunked payload blobs; `app install`
and `app update` install the build an app needs before the app, and refuse —
changing nothing — when no registry offers it. Installing a runtime package
(`tilo-install install <runtime.tapp> --payload-from <registry>`) verifies the
bundle the way the launcher does (signature, a publisher from
`/etc/tilo/runtime-bundle-trusted-publishers.txt`, architecture, and that its
payload has the digest it is named by) and places it in the per-user runtime
directory below.

Because the runtime is rebuilt with every release, the newest version of an
app usually names a build the device does not have. Opening an app that is
not installed (`app install`, the Store's Open, a peer's window) therefore
takes the newest offered version whose runtime build is already installed —
the image's or a per-user one — and only falls back to the newest version,
and its runtime, when no retained version runs on one. Every candidate is a
version of the same publisher-signed entry, installed through the same
verified pipeline. `app update` still moves the app to the newest version and
its runtime. When a runtime does have to be installed, every payload blob
whose BLAKE3 hash an installed bundle's `bundle.toml` lists is copied from
that bundle, re-hashed while it is copied, and only the rest is fetched, with
four requests in flight.

A per-user runtime is writable by the user's unconfined processes, so the
launcher proves every byte of it before every launch, not only at install.
Hashing 260 MB took a quarter-second per open, so the installer *seals* the
verified bundle with fs-verity instead (`sealed   : N file(s) with
fs-verity`): every `bundle.toml` lists each file's fs-verity digest (SHA-256,
4 KiB blocks) under a second publisher signature (`verity_signature_hex`)
that leaves the root hash, the ABI digest and the signature older launchers
check unchanged. A sealed file can never be written again and the kernel
checks every page read from it against its Merkle tree, so the launcher
accepts a file unread when its kernel-measured digest equals the signed one,
and hashes every other file as before: an unsealed or replaced file, one
with another digest, a bundle without the verity signature, any filesystem
without fs-verity (XFS, tmpfs). Launch verification of a sealed runtime costs
a few system calls per file. What it guarantees is unchanged:

- a byte that differs from the signed payload is refused before the sandbox
  is built — replacing a sealed file through its writable directory leaves
  an unsealed file, or one that measures another digest, and either is
  hashed and refused;
- a forged digest breaks the verity signature, which every verification
  (install and launch) refuses;
- the window between verification and the sandbox's bind mount, in which a
  process of the same user could swap a file, is the one the full hash had
  (shorter, now that verification takes microseconds); closing it needs a
  runtime directory the user cannot write, which is the system bundle's
  property, not the per-user one's.

```
$ tilo-install runtime install --version 2026.07 --from-host --force
installed runtime bundle: tilo-runtime/2026.07
  path     : /tmp/tilo-install-demo-data/tilo-runtime/2026.07
  files    : 11
  root-hash: blake3:1e7dedc8881ab7e8225b0c680e832e2332e3b1c14406f34fd1b2a70edec3e58d

$ tilo app runtime list
version               path
2026.07               /tmp/tilo-install-demo-data/tilo-runtime/2026.07

$ tilo-install runtime verify 2026.07
verified: tilo-runtime/2026.07
  path     : /tmp/tilo-install-demo-data/tilo-runtime/2026.07
  files    : 11
  root-hash: blake3:1e7dedc8881ab7e8225b0c680e832e2332e3b1c14406f34fd1b2a70edec3e58d

$ tilo-install runtime path --version 2026.07
/tmp/tilo-install-demo-data/tilo-runtime/2026.07

$ tilo app runtime remove 2026.07
removed runtime bundle: tilo-runtime/2026.07
```

`--from-host` copies from the running host root; `--from <path>` copies
from an explicit prefix (e.g. a distrobox rootfs). The bundle is
content-addressed by BLAKE3 root of the sorted `(path, blake3(bytes))`
pairs — any byte change flips the root hash.

Discovery order the launcher uses:

1. `$XDG_DATA_HOME/tilo-runtime/<version>/` — per-user (installed from a
   runtime package, or built with `tilo-install runtime install`).
2. `/usr/lib/tilo-runtime/<version>/` — system-wide (baked into an OS
   image).

## Updates

`tilo-install update <id>` resolves a newer version through the
configured registries (or `--from <new.tapp>`); `install --force
<new.tapp>` replaces an installed package too. Either way:

- Registry entry retains the same `id`, and its pin.
- The new tree is materialised beside the old one; files the versions
  share are not stored or fetched again. The row replacement commits the
  update, then the old tree and its unshared blobs go.
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
$ tilo app trust add --id "tilo-curated/2026.07" --pubkey <hex> --scope "all"
$ tilo app trust list
$ tilo app trust remove <id>

$ tilo app trust revoke <pubkey_hex> --reason "compromised 2026-Q3"
$ tilo app trust revoked
```

Next: [`trust.md`](./trust.md) — publisher identities, attestation
registry, revocation.
