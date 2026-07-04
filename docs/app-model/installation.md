# Installation: `qos-install` and the on-disk layout

`qos-install` is the one-command user-facing install surface. It verifies
signatures, unpacks payloads, drops PATH shims, and launches. All state
lives under `$XDG_DATA_HOME` and `$XDG_CONFIG_HOME`; no root, no system-
wide side effects (except the packages themselves, which never need root).

## Verbs

```
$ qos-install --help
Install, launch, list, or remove qOS packages.

Usage: qos-install <COMMAND>

Commands:
  install  Install a .qpkg into the local store. Verifies signature first
  list     List installed packages
  remove   Remove an installed package by id
  run      Launch an installed package via qos-launcher
  inspect  Print manifest + install info for an installed package
  trust    Manage the trusted-attestor registry
  revoke   Revoke a publisher key
  revoked  List revoked keys
  path     Print the directory the shim installer writes PATH shims into
  runtime  Manage installed qOS runtime bundles
```

## The on-disk layout

```
$XDG_DATA_HOME/                     # default: ~/.local/share
  qos-apps/
    registry.toml                   # one entry per installed package
    payloads/
      <pkg-id>/                     # unpacked payload
        manifest.toml
        bin/…                       # entrypoint(s)
        …                           # everything from the payload/ tree
    bin/                            # PATH-shim install root
      <command>                     # copy of qos-cli-shim
      <command>.qos-shim            # sidecar (TOML)

  qos-runtime/
    <version>/                      # e.g. 2026.07
      bundle.toml                   # manifest: version, root_hash, files[]
      lib/
        ld-linux-x86-64.so.2
        libc.so.6
        libgcc_s.so.1
        …
      etc/ld.so.cache

$XDG_CONFIG_HOME/                   # default: ~/.config
  qos/
    trusted-attestors.toml
    revoked-keys.toml
```

Override `$XDG_DATA_HOME` per-app for tests, or set `QOS_APPS_DATA_DIR`
to override just the qos-apps subtree (`qos-launcher::registry` reads
the same var).

### `registry.toml`

```toml
[[entry]]
id                = "dev.qos.hello"
version           = "0.1.0"
package_id_hex    = "b8bde92e…"
payload_root      = "/…/qos-apps/payloads/dev.qos.hello"
manifest_path     = "/…/qos-apps/payloads/dev.qos.hello/manifest.toml"
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
qos-install install <qpkg>
    [--pinned]                      # never GC'd (GC not implemented yet)
    [--force]                       # overwrite same-id install
    [--require-attestor <id>]       # refuse unless signed by this attestor
    [--allow-self-signed]           # permit self-signed as fallback
```

Sequence:

1. Read the `.qpkg` tar into an in-memory blob store.
2. Verify the signed root and every hash. Apply trust policy (see
   [`trust.md`](./trust.md)).
3. If a package with the same `[package].id` is already installed,
   refuse unless `--force`.
4. Remove any old `payloads/<id>/` directory.
5. Unpack payload into `payloads/<id>/`. Write `manifest.toml` next to
   it explicitly (so the launcher can find it without re-reading the
   `.qpkg`).
6. If the manifest has `[cli].commands`, drop a shim + sidecar in
   `bin/` per command.
7. Append a `RegistryEntry` to `registry.toml`.

Actual run against the hello demo:

```
$ qos-install install /tmp/qos-hello-demo/qos-hello.qpkg
qos-install: WARN — package self-signed by ed25519:c4c627b57… (no trusted-attestor signature); use --require-attestor <id> to demand an attestation
installed: dev.qos.hello 0.1.0
  payload  : /tmp/qos-install-demo-data/qos-apps/payloads/dev.qos.hello
  pinned   : false
run with:  qos-install run dev.qos.hello
```

## `list`

```
$ qos-install list
id                                version   pinned  attested-by         installed
dev.qos.hello                     0.1.0     no      -                   0s ago
```

## `inspect`

```
$ qos-install inspect dev.qos.hello
id            : dev.qos.hello
version       : 0.1.0
package-id    : qpkg:b8bde92e2352dd7a3b3e094968272c42d51196697a5cec17f36e3595a55a71d3
publisher     : ed25519:c4c627b57a518bc318c2854662b44f741345f6d55a45eb059d3b384048789e39
payload-root  : /tmp/qos-install-demo-data/qos-apps/payloads/dev.qos.hello
manifest      : /tmp/qos-install-demo-data/qos-apps/payloads/dev.qos.hello/manifest.toml
pinned        : false
attested-by   : (self-signed)
installed     : 0 seconds ago
```

## `run`

```
qos-install run <pkg-id>
    [--no-services]                 # skip service-fd resolution
    [--allow-missing-runtime]       # fall back to --minimal-libs if runtime bundle absent
    [--require-attestor <id>]       # re-check attestation at launch
    [--allow-self-signed]
```

Re-verifies the trust policy at launch (revocations may have arrived
since install), then `exec()`s `qos-launcher run --manifest <path>
--payload-root <path>`, which forks bwrap and starts the sandbox flow
described in [`sandbox.md`](./sandbox.md).

Because `run` uses `exec()`, this process's exit code IS the launcher's.

## `remove`

```
$ qos-install remove dev.qos.hello
removed: dev.qos.hello
```

Deletes the payload dir, un-writes any registered PATH shims, and drops
the registry entry.

## `path`

Prints the PATH-shim install directory + a snippet the user can drop
into their shell rc:

```
$ qos-install path
/tmp/qos-install-demo-data/qos-apps/bin

# Prepend the above to your shell's PATH so CLI commands
# installed via `qos-install install <pkg.qpkg>` are picked up.
#
# bash / zsh (~/.bashrc, ~/.zshrc):
export PATH="/tmp/qos-install-demo-data/qos-apps/bin:$PATH"
#
# fish (~/.config/fish/config.fish):
fish_add_path -pg /tmp/qos-install-demo-data/qos-apps/bin
```

## `runtime`

Manages the shared `qos-runtime/<version>` bundles. See
[`packaging.md`](./packaging.md) for what a runtime bundle is
(essentially: ld-linux + libc + libgcc_s + friends, content-addressed).

```
$ qos-install runtime install --version 2026.07 --from-host --force
installed runtime bundle: qos-runtime/2026.07
  path     : /tmp/qos-install-demo-data/qos-runtime/2026.07
  files    : 11
  root-hash: blake3:1e7dedc8881ab7e8225b0c680e832e2332e3b1c14406f34fd1b2a70edec3e58d

$ qos-install runtime list
version               path
2026.07               /tmp/qos-install-demo-data/qos-runtime/2026.07

$ qos-install runtime verify 2026.07
verified: qos-runtime/2026.07
  path     : /tmp/qos-install-demo-data/qos-runtime/2026.07
  files    : 11
  root-hash: blake3:1e7dedc8881ab7e8225b0c680e832e2332e3b1c14406f34fd1b2a70edec3e58d

$ qos-install runtime path --version 2026.07
/tmp/qos-install-demo-data/qos-runtime/2026.07

$ qos-install runtime remove 2026.07
removed runtime bundle: qos-runtime/2026.07
```

`--from-host` copies from the running host root; `--from <path>` copies
from an explicit prefix (e.g. a distrobox rootfs). The bundle is
content-addressed by BLAKE3 root of the sorted `(path, blake3(bytes))`
pairs — any byte change flips the root hash.

Discovery order the launcher uses:

1. `$XDG_DATA_HOME/qos-runtime/<version>/` — per-user (installed via
   `qos-install runtime install`).
2. `/usr/lib/qos-runtime/<version>/` — system-wide (baked into an OS
   image).

## Updates

There is no in-place upgrade command yet. Update = `install --force
<new.qpkg>`:

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
qos-install: package dev.qos.hello already installed (use --force to overwrite)
```

## Trust management

See [`trust.md`](./trust.md) for the full model. Quick reference:

```
$ qos-install trust add --id "qos-curated/2026.07" --pubkey <hex> --scope "all"
$ qos-install trust list
$ qos-install trust remove <id>

$ qos-install revoke <pubkey_hex> --reason "compromised 2026-Q3"
$ qos-install revoked list
```

Next: [`trust.md`](./trust.md) — publisher identities, attestation
registry, revocation.
