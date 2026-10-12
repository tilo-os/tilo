# Packaging: manifest schema and the `tilo-app-package` CLI

A `.tapp` is a tar archive of a signed root + a manifest + a Collection of
content-addressed blobs. This document is the source of truth for what
belongs in the manifest and what `tilo-app-package` does to it.

## The manifest file

Every manifest is TOML. The schema comes straight from
`crates/packaging/tilo-app-package/src/manifest.rs`. A complete minimal manifest for a
GUI app:

```toml
[package]
id       = "so.tilo.hello"
version  = "0.1.0"
kind     = "app"
runtime  = "tilo-runtime/2026.07"

[publisher]
identity  = "ed25519:0000000000000000000000000000000000000000000000000000000000000000"
signature = ""

[sandbox]
namespaces = ["user", "pid", "uts", "ipc", "mount", "net"]
seccomp    = "tilo-default"
landlock   = "strict"

[sandbox.net]
mode = "off"

[sandbox.portals]

[sandbox.services]

[entrypoint]
bin  = "bin/tilo-hello"
type = "gui"
```

### Blocks

#### `[package]`

| Field     | Required | Notes                                                             |
| --------- | -------- | ----------------------------------------------------------------- |
| `id`      | yes      | Reverse-DNS, e.g. `so.tilo.notes-cli`. Stable across versions; used as the install slot and to name the sandbox profile. |
| `version` | yes      | Semver. Rejected at validate time if not parseable.               |
| `kind`    | yes      | One of `app`, `service`, `format-provider`, `plugin`, `runtime`.  |
| `runtime` | no       | `<name>/<version>` of the runtime bundle to bind against. The version side must be semver OR calver (`YYYY.MM[.MICRO]`, e.g. `2026.07`). Must be omitted for `kind = "runtime"`. |

#### `[publisher]`

| Field       | Required | Notes                                                                                            |
| ----------- | -------- | ------------------------------------------------------------------------------------------------ |
| `identity`  | yes      | `ed25519:<64-hex>` — the publisher's Ed25519 public key. When `tilo-app-package pack` generates a fresh keypair (no `--identity`), it overwrites whatever was here with the generated public key. |
| `signature` | no       | Hex-encoded 64-byte signature. Empty at author time; filled by `tilo-app-package pack` / `tilo-app-package sign`.  |

#### `[trust]`

Optional. Third-party attestations. Repeated as `[[trust.attested_by]]`:

```toml
[[trust.attested_by]]
attestor  = "tilo-curated/2026.07"
claim     = "deadbeef"      # attestor-specific bytes, hex
signature = ""              # 64-byte Ed25519 signature, hex; empty for legacy claim-only rows
```

The attestor list is verified against `~/.config/tilo/trusted-attestors.toml`
at install and run time. See [`trust.md`](./trust.md).

#### `[sandbox]`

| Field        | Required | Notes                                                                                     |
| ------------ | -------- | ----------------------------------------------------------------------------------------- |
| `namespaces` | yes      | Linux namespaces to unshare. Names match `unshare(2)`: any subset of `user`, `pid`, `uts`, `ipc`, `mount`, `net`. |
| `seccomp`    | yes      | Named seccomp profile. `tilo-default` is the shipped baseline.                             |
| `landlock`   | yes      | `strict` — enforce Landlock v3+; `lax` — best-effort; `off` — disable.                    |

##### Filesystem — no block

A manifest names no filesystem path. `[sandbox.fs]`, with its `read` and
`write` lists, is retired: a manifest that still carries an entry is
refused at install and at launch, with a message naming the package and
the offending entry.

The launcher gives every app one writable directory, bound at
`/run/tilo/app`, with `HOME` and the `XDG_*` variables pointed inside it.
Reads come from a fixed system profile — loader and C library, fonts, GPU
loader manifests, `/sys`, CA bundle, timezone data — identical for every
package. Anything else is a capability the user grants at runtime. See
[`sandbox.md`](./sandbox.md).

##### `[sandbox.net]`

```toml
[sandbox.net]
mode  = "allowlist"
hosts = ["api.example.com", "cdn.example.com"]
```

`mode` is one of:

- `off` — no network at all. Common for pure viewers/editors.
- `host` — full host network. Only for `runtime` / privileged apps.
- `dns-only` — DNS resolution allowed; nothing else.
- `allowlist` — outbound to the listed hosts only.

For any mode other than `allowlist`, `hosts` is ignored.

##### `[sandbox.portals]`

XDG desktop portals the launcher should plumb the D-Bus session bus in for.
The map key is the portal name (`file-chooser`, `screencast`, `notifications`,
`secret`, `open-uri`). The value is `"via-host"` or `"deny"`.

```toml
[sandbox.portals]
file-chooser  = "via-host"
notifications = "via-host"
open-uri      = "via-host"
```

**Absence == no bus plumbed.** Your `ashpd` / `runtime::portal_xdg` calls
will surface `PortalError::NotAvailable`. This is a security default: the
D-Bus session bus is a wide attack surface, so the launcher only forwards
it when the manifest says to.

##### `[sandbox.services]`

Declared dependencies on Tilo system services. Each value parses as
`required@<version>` or `optional@<version>`.

```toml
[sandbox.services]
collab     = "optional@1"
tilo-fs     = "optional@1"
```

At launch, the launcher opens a connected Unix socket to every declared
service and passes the fd in as `TILO_SVC_<NAME>_FD`. If a `required` service
can't be reached the launcher aborts; if an `optional` service is missing
the app sees `NotAvailable::ServiceFdMissing` and is expected to degrade.

The `required` entries are also the package's **system interface
requirements**: the versioned contracts with the OS it cannot work without.
An OS image declares every version it serves of each interface in
`/usr/share/tilo/system-interfaces.toml` (source:
`os/usr/share/tilo/system-interfaces.toml`). A registry version records its
build's requirements as `target.interfaces` (`["system@1"]`), which the
registry checks against the signed manifest; `app update`, `app install` and
the App Store offer a device only builds its OS serves, `tilo-install`
refuses the rest, and the launcher refuses to run one (see
[installation.md](./installation.md#the-system-store)). A host with no
declaration judges nothing.

`system` is the interface of the system services the device's control
panel drives — network, Bluetooth, power, accounts, date and time,
updates — reached through the image-owned grant in
`crates/packaging/launcher/src/system_authority.rs`. It has no socket. Only
the packages the image names as its control panel receive the grant, and
only when they declare it:

```toml
[sandbox.services]
system = "required@1"
```

A change to that grant (or to any service protocol) that an existing
client could not work with is a new version: the image serves the old one
beside it for as long as published builds use it.

#### `[entrypoint]`

Required for `kind = "app"`, absent for `kind = "runtime"`, optional for
services (which usually install a systemd unit).

| Field             | Required                | Notes                                                        |
| ----------------- | ----------------------- | ------------------------------------------------------------ |
| `bin`             | yes when `type != "both"` | Payload-relative path to the binary. Fallback used by `both` when the split fields are absent. |
| `type`            | no (default `gui`)      | `gui`, `cli`, or `both`.                                     |
| `gui-bin`         | only for `type = "both"`| Payload-relative GUI binary.                                 |
| `cli-bin`         | only for `type = "both"`| Payload-relative CLI binary.                                 |
| `splash-segments` | no                      | List of `"<path>[<start>:<end>]"` byte ranges the loader prefetches before exec. `<start>`/`<end>` are decimal or `0x`-hex. |
| `splash-budget`   | no (default `100`)      | Milliseconds the loader will keep the splash before falling back. |

#### `[cli]`

Only meaningful when the entrypoint's `type` is `cli` or `both`. See
[`cli-apps.md`](./cli-apps.md).

```toml
[cli]
commands = ["notes", "notes-import"]
```

At install time `tilo-install install` drops a `tilo-cli-shim` copy under
`~/.local/share/tilo-apps/bin/<command>` for each entry.

#### `[providers]`

For an app, this declares the file and link resources its GUI entrypoint can
actually consume. A `format-provider` uses the same MIME list for its codec
scope; see [`formats.md`](./formats.md).

```toml
[providers]
mime = ["text/markdown"]
ext  = ["md", "markdown"]
schemes = ["web+notes"]
```

#### `[target]` — written by the packer

Authors never write this block. `tilo-app-package pack-desktop --platform
linux|windows|macos --arch x86_64|aarch64` (and `tilo-app package` for
Android) writes it for a build for another system. Those systems ship no
toolkit, so the package is one self-contained executable at `bin/<name>`;
`module` names it (and so does `[entrypoint]`), `isa` is the feature level it
was compiled for, and `runtime` is the linked toolkit release, equal to
`package.runtime`. The first four fields are exactly the registry's signed
target (see [registry.md](../registry.md#5-publisher-workflow) for the
spellings):

```toml
[target]
platform = "macos"
architecture = "aarch64"
isa = "aarch64"
runtime = "tilo-runtime/2026.07"
module = "bin/books"
```

### Validation

`tilo-app-package pack` calls
`validate_manifest_with_payload` before writing anything, which enforces:

- `sandbox.fs.read` and `sandbox.fs.write` are empty. Checked first, so a
  package that still declares paths reads that error rather than a semver
  nit four lines down. `tilo-install install` runs the same check, and the
  launcher refuses again at launch.
- `package.version` is semver.
- `package.runtime` is `<name>/<version>` with a semver or calver version, and
  is absent for `kind = "runtime"`.
- `publisher.identity` parses as `ed25519:<64-hex>` and `publisher.signature`
  is either empty or `128-hex`.
- `kind = "app"` has an `[entrypoint]` with `bin` (or the correct split
  binary for `type = "both"`).
- `kind = "format-provider"` has non-empty `providers.mime` AND ships a
  `provider.wasm` in the payload.
- provider extensions, concrete MIME types, and URI schemes use their strict
  normalized identifier grammars; active/reserved `file`, `data`, and
  `javascript` schemes are rejected.
- a Linux, Windows, macOS or Android `[target]` belongs to a `kind = "app"` with no
  `[package.isa]`; its `module` is the entrypoint, a file in the payload, and
  its `runtime` equals the non-empty `package.runtime`. Such a package may be
  up to 64 MiB rather than the Tilo OS app limit.

### On-disk layout

A `.tapp` is a plain (uncompressed) tar. Contents of the example hello-world
package:

```
$ tar tvf /tmp/tilo-hello-demo/tilo-hello.tapp
-rw-r--r-- 0/0              96 1970-01-01 01:00 tapp.toml
-rw-r--r-- 0/0             173 1970-01-01 01:00 signed-root
-rw-r--r-- 0/0             770 1970-01-01 01:00 manifest.toml
-rw-r--r-- 0/0              61 1970-01-01 01:00 collection.qcl
-rw-r--r-- 0/0              61 1970-01-01 01:00 blobs/55ab8b6bdeb4e85d7fd37e975914278f67347056414d3e489aaaad4a1b4de2ee
-rw-r--r-- 0/0         7811120 1970-01-01 01:00 blobs/70a0c763f562d63c09220bc85a427e806cc6cd28f8730cf0796ba52ad2982fe1
-rw-r--r-- 0/0             770 1970-01-01 01:00 blobs/774d5714edfb4734e3f158e5e74aa5a8aeb34aab98ae21ef675894a578dbb312
-rw-r--r-- 0/0             173 1970-01-01 01:00 blobs/b8bde92e2352dd7a3b3e094968272c42d51196697a5cec17f36e3595a55a71d3
```

- `tapp.toml` — 2-field header: `package_id = "tapp:<hex>"`, `format = 1`.
  Duplicates info in `signed-root`; kept human-readable for `tar tf`.
- `signed-root` — 173-byte binary: `manifest_hash + payload_root_hash +
  publisher_identity + signature`.
- `manifest.toml` — UTF-8 TOML. Same content as author input.
- `collection.qcl` — encoded payload Collection: the mapping from payload-
  relative path to blob hash.
- `blobs/<hex>` — every referenced blob, named by its BLAKE3 hex hash. The
  signed root, manifest, and collection are each also written here under
  their hash (so a hash-only downloader can reconstruct without walking the
  named entries).

Uncompressed on purpose: each blob is already content-addressed and (in
deployed Tilo) lives in an `iroh-blobs` store with chunk-level dedup. Adding
gzip over that adds CPU cost without improving deduplication.

The `PackageId` is `blake3(signed-root-bytes)`. It is the `tapp:<hex>` URL
you see in `tilo-app-package inspect`.

## `tilo-app-package` — the CLI

```
$ tilo-app-package --help
Pack / sign / verify / inspect Tilo packages (.tapp)

Usage: tilo-app-package <COMMAND>

Commands:
  pack     Pack a payload directory + manifest into a .tapp, signing it with a freshly-generated identity unless --identity is passed
  sign     Re-sign an existing .tapp with the given identity, writing the result back into the same file
  verify   Verify a .tapp — checks every cryptographic invariant and reports the first one that breaks. By default the embedded publisher identity is treated as trusted (so the command succeeds on a self-signed package); pass --trusted-key to override
  inspect  Print the manifest + payload tree of a .tapp
  unpack   Extract the payload + a copy of the manifest into `dest`
  help     Print this message or the help of the given subcommand(s)
```

### `pack`

```
tilo-app-package pack --manifest <toml> --payload <dir> --output <tapp> [--identity <json>]
```

- Reads `--manifest` and `--payload/**`.
- If `--identity` is absent, generates a fresh Ed25519 keypair and overwrites
  `publisher.identity` in the embedded manifest with the fresh public key.
- Signs the signed root with that identity.
- Writes the `.tapp`.

Example run against the demo project:

```
$ tilo-app-package pack \
    --manifest /tmp/tilo-hello-demo/manifest.toml \
    --payload  /tmp/tilo-hello-demo/payload \
    --output   /tmp/tilo-hello-demo/tilo-hello.tapp
packed tapp:b8bde92e2352dd7a3b3e094968272c42d51196697a5cec17f36e3595a55a71d3 (4 blobs)
```

The `--identity` file is JSON:

```json
{ "secret_key_hex": "<32-byte hex — the Ed25519 signing-key seed>" }
```

### `sign`

Re-sign an existing `.tapp` with a different identity. Fails unless the
identity's public key matches the `.tapp`'s embedded publisher identity —
this command is for detached signing flows (e.g. an offline HSM signed
what `tilo-app-package pack --identity dummy.json` produced), not for identity
substitution.

### `verify`

Every cryptographic invariant. On the demo bundle:

```
$ tilo-app-package verify /tmp/tilo-hello-demo/tilo-hello.tapp
ok: tapp:b8bde92e2352dd7a3b3e094968272c42d51196697a5cec17f36e3595a55a71d3 verifies and publisher is trusted
```

Pass `--trusted-key <hex>` (repeatable) to override the default of trusting
the embedded publisher key. Exit code:

- `0` — trusted publisher, all signatures OK.
- `2` — signature OK but publisher untrusted (`--trusted-key` set and did
  not match).
- `1` — any cryptographic failure (broken signature, wrong hash, revoked).

### `inspect`

Human-readable dump of everything but the payload contents:

```
$ tilo-app-package inspect /tmp/tilo-hello-demo/tilo-hello.tapp
package-id: tapp:b8bde92e2352dd7a3b3e094968272c42d51196697a5cec17f36e3595a55a71d3
manifest-hash: blake3:774d5714edfb4734e3f158e5e74aa5a8aeb34aab98ae21ef675894a578dbb312
payload-root-hash: blake3:55ab8b6bdeb4e85d7fd37e975914278f67347056414d3e489aaaad4a1b4de2ee
publisher-identity: ed25519:c4c627b57a518bc318c2854662b44f741345f6d55a45eb059d3b384048789e39
signed: yes

--- manifest.toml ---
[package]
id = "so.tilo.hello"
version = "0.1.0"
kind = "app"
runtime = "tilo-runtime/2026.07"
...
```

### `unpack`

```
tilo-app-package unpack --dest <dir> <tapp>
```

Writes the payload tree into `--dest` and drops a copy of `manifest.toml`
next to it. Useful for auditing a signed bundle without a running Tilo.

## The pack pipeline

Under the hood, `pack` runs this sequence:

```
    ┌─────────────────┐
    │payload/**       │  ── blake3 each file
    │Cargo output     │        │
    └─────────────────┘        ▼
                          ┌────────────┐
                          │Collection  │ (path → hash map)
                          └────────────┘
                                │
                                ▼
    ┌────────────────┐    ┌────────────┐
    │manifest.toml   │──▶ │signing-bytes│ = manifest_hash || payload_root_hash || publisher_key
    └────────────────┘    └────────────┘
                                │
                     Ed25519 sign with publisher key
                                │
                                ▼
                          ┌────────────┐
                          │SignedRoot  │  (173 bytes)
                          └────────────┘
                                │
                     BLAKE3 hash the SignedRoot
                                │
                                ▼
                          ┌────────────┐
                          │ PackageId   │  "tapp:<64-hex>"
                          └────────────┘
```

The `PackageId` is derived entirely from the signed root, so a re-sign
(with a different keypair or a different signature) produces a different
`PackageId`. Two identical builds with the same keypair produce the same
`PackageId` — the pack path is deterministic given deterministic inputs
(BLAKE3 is content-addressed; Ed25519 signatures are non-deterministic by
default, so identity re-use with a fresh key each pack changes the ID).

Next: [`sandbox.md`](./sandbox.md) explains what each `[sandbox.*]` block
enables at launch.

## Packaging against a verified native SDK

For a thin app in an SDK checkout, select the SDK bundle using the publisher
and payload root from its independently authenticated build/delivery receipt:

```sh
tilo-app package --sdk-bundle /path/to/verified/sdk \
  --sdk-publisher <expected-public-key> --sdk-root <expected-payload-root>
```

Use `tilo-app new --id <your-app-id>` inside that checkout to create the app.
Keep the new workspace package's lockfile entry alongside its Cargo manifest;
packaging uses `--locked` and does not resolve a replacement dependency graph.
The native SDK path resolves all checkout app features with the existing thin
compiler wrapper, while building only the selected app's binary targets.

The tool verifies the complete signed SDK payload, checks the actual Cargo
toolkit artifact and compiler standard library against it, and inspects the
staged executable's architecture, dependency closure and versioned dynamic
imports without executing the app. It writes the verified SDK's native
content fingerprint into the signed package. The selected publisher/root
must come from the receipt; reading them out of an untrusted bundle does not
authenticate that bundle.

An incompatible graph is refused with a paired SDK build/update requirement.
Changing the authored runtime tag cannot make a different compiled SDK match.
Standalone and static builds retain their existing behavior; this native SDK
selection currently requires a checkout workspace app. Packaging does not
install the SDK or grant publisher trust.

The real authored-app qualification is opt-in because it compiles the native
SDK, builds a canonical signed bundle from its actual closure, packages a
new app, and then executes its shared headless semantic assertions:

```sh
cargo test --locked -p tilo-app-cli --features native-sdk-qualification \
  --test native-sdk generated_app_packages_only_with_its_actual_signed_native_sdk -- --exact
```

### Reusing the SDK compiler interface

An app can use the SDK's already compiled Rust units instead of recompiling the
checkout graph. This requires a separately signed **developer compiler bundle**,
not just the installed runtime directory. The compiler bundle binds the exact
runtime root and publisher, toolkit bytes, architecture, native compiler host and
full `rustc --version --verbose` identity. It contains the actual unit metadata
and proc-macro artifacts from that SDK build, with a closed signed inventory.
It is a developer download; it is not added to the end-user runtime layer.

The SDK producer exports it once, after sealing the runtime. The normal full
apps+runtime assembler supports an optional separate output directory:

```bash
python3 os/dev/assemble-app-runtime.py --repo "$PWD" --target-dir "$PWD/target" \
  --profile thin-app --dest target/sdk-component \
  --compiler-interface-out target/sdk-developer-bundles
```

The assembler captures the actual Cargo artifact messages from the complete SDK
build. `tilo-app sdk capture` makes metadata-only rlibs retaining their exact
`lib.rmeta` members, and copies the actual proc-macro libraries. `tilo-app sdk
export` verifies the selected sealed runtime and signs the compiler inventory
with its publisher. The runtime root is computed first, so there is no circular
binding. Existing runtime bundles without a compiler interface remain usable
for running apps, but cannot satisfy this authoring path.

After independently obtaining the runtime and compiler roots and publisher,
create a normal app and package it with both selections:

```bash
tilo-app new hello --id com.example.hello --title Hello --directory apps/hello
tilo-app package apps/hello \
  --sdk-bundle /path/to/runtime --sdk-publisher <expected-public-key> \
  --sdk-root <expected-runtime-root> \
  --sdk-compiler-bundle /path/to/developer-bundle \
  --sdk-compiler-root <expected-compiler-root> \
  --identity /private/publisher.json --output hello.tapp
```

Only the authored app's units compile. The compiler wrapper refuses dependency
rebuilds, and normal packaging still verifies the binary's actual runtime/std
linkage, imported symbols, native closure and ISA before signing. Shared resource
declarations come from the signed SDK inventory; authored declarations come
from the compiled source snapshot. Neither signing nor declaration inspection
executes the app.

Currently this interface supports native Linux authoring in a canonical checkout
with the exact producer compiler, the `thin-app` profile, and dependencies already
provided by the SDK (`tilo-app`, `runtime-dylib`, and `workspace-hack`). Cross-host
compilation, application build scripts and additional dependencies refuse
explicitly. A changed SDK, compiler or compiled dependency feature set requires
a corresponding verified compiler interface; a runtime label cannot substitute
for it. Developer artifacts are much larger than the approximately 1–2 MB app
package and can be cached independently of end-user runtime delivery.
