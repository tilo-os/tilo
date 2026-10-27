# Hello, world: from `cargo new` to `tilo-install run`

The full recipe. Run the commands from a current checkout; exact output varies
with the toolchain, host, and installed services.

## Prerequisites

You need:

- A Rust toolchain (rustc / cargo), as pinned by the repository's
  `rust-toolchain.toml`.
- The Tilo packaging tools: `tilo-app-package`, `tilo-install`, `tilo-launcher`,
  `tilo-cli-shim`, `tilo-runtime-bundle`. Build them from the workspace:

  ```bash
  cargo build --release -p tilo-app-package-cli -p install -p launcher -p cli-shim -p runtime-bundle
  ```

- `bubblewrap` (`sudo dnf install bubblewrap` on Fedora). The launcher
  invokes it under the hood.
- A checkout of the Tilo repo (for the workspace paths to the runtime
  crates). Once a crates.io release exists these will simplify to version
  requirements.

Add the release binaries to your PATH, e.g.:

```bash
export PATH="/path/to/tilo/target/release:$PATH"
```

## Step 1: `cargo new`

```
$ mkdir -p /tmp/hw-walkthrough
$ cd /tmp/hw-walkthrough
$ cargo new --bin my-example-hello
    Creating binary (application) `my-example-hello` package
note: see more `Cargo.toml` keys and their definitions at https://doc.rust-lang.org/cargo/reference/manifest.html
```

## Step 2: depend on the runtime crates

Edit `my-example-hello/Cargo.toml`:

```toml
[package]
name = "my-example-hello"
version = "0.1.0"
edition = "2024"

[[bin]]
name = "my-example-hello"
path = "src/main.rs"

[dependencies]
runtime-portal = { path = "/path/to/tilo/crates/runtime/runtime-portal" }
runtime-collab = { path = "/path/to/tilo/crates/runtime/runtime-collab" }
runtime-fs = { path = "/path/to/tilo/crates/runtime/runtime-fs" }
runtime-identity = { path = "/path/to/tilo/crates/runtime/runtime-identity" }
```

## Step 3: write a `main.rs`

`my-example-hello/src/main.rs`:

```rust
use runtime_collab as collab;
use runtime_fs as fs;
use runtime_identity as identity;
use runtime_portal::{NotAvailable, is_tilo_native};

fn main() {
    println!("my-example-hello: running as Tilo-native = {}", is_tilo_native());

    match collab::try_connect() {
        Ok(_) => println!("  collab  : connected"),
        Err(NotAvailable::ServiceFdMissing) => println!("  collab  : not available (graceful)"),
        Err(e) => println!("  collab  : unavailable ({e:?})"),
    }
    match fs::try_connect() {
        Ok(_) => println!("  fs      : connected"),
        Err(NotAvailable::ServiceFdMissing) => println!("  fs      : not available (graceful)"),
        Err(e) => println!("  fs      : unavailable ({e:?})"),
    }
    match identity::load() {
        Ok(_) => println!("  identity: loaded"),
        Err(NotAvailable::ServiceFdMissing) => println!("  identity: not available (graceful)"),
        Err(e) => println!("  identity: unavailable ({e:?})"),
    }

    println!("my-example-hello: done.");
}
```

## Step 4: build

```
$ cd my-example-hello
$ cargo build --release
   Compiling my-example-hello v0.1.0 (/tmp/hw-walkthrough/my-example-hello)
    Finished `release` profile [optimized] target(s) in 0.24s
```

Verify the binary:

```
$ ls -la target/release/my-example-hello
-rwxr-xr-x. 2 you you 3075216  4. Jul 16:49 target/release/my-example-hello
```

3 MB. That's a stripped release build; running under `tilo-install` on
stock Linux (with a runtime bundle already installed) will produce the
service / sandbox output.

## Step 5: install a runtime bundle

Once per system. Copy the host loader + libc into a content-addressed
bundle Tilo's launcher can mount into the sandbox:

```
$ tilo-install runtime install --version 2026.07 --from-host --force
installed runtime bundle: tilo-runtime/2026.07
  path     : /tmp/tilo-hw-data/tilo-runtime/2026.07
  files    : 11
  root-hash: blake3:1e7dedc8881ab7e8225b0c680e832e2332e3b1c14406f34fd1b2a70edec3e58d
```

11 files: `ld-linux`, `libc.so.6`, `libgcc_s.so.1`, `libm.so.6`,
`libdl.so.2`, `libpthread.so.0`, `libutil.so.1`, `librt.so.1`, plus a
copy of `/etc/ld.so.cache`. All content-addressed by their BLAKE3 hash;
`root-hash` is the BLAKE3 of the sorted `(path, hash)` list.

## Step 6: write a manifest

`/tmp/hw-walkthrough/pkg-workdir/manifest.toml`:

```toml
[package]
id       = "my.example.hello"
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
bin  = "bin/my-example-hello"
type = "gui"
```

There is no filesystem block. A manifest names no path to read or write —
the launcher supplies both, and a manifest that declares `sandbox.fs.read`
or `sandbox.fs.write` is refused at install and at launch. See
[`sandbox.md`](./sandbox.md).

Notes on the placeholder `publisher.identity`: `tilo package pack` with no
`--identity` flag generates a fresh Ed25519 keypair, signs the package
with it, and overwrites this field with the generated public key. What you
put here is discarded. For repeatable publishing, pass
`--identity <keyfile.json>`.

## Step 7: assemble the payload

```
$ mkdir -p /tmp/hw-walkthrough/pkg-workdir/payload/bin
$ cp target/release/my-example-hello /tmp/hw-walkthrough/pkg-workdir/payload/bin/
```

The payload's structure is the shape your binary will see inside the
sandbox (rooted at `/var/tilo-apps/my.example.hello/`). Manifest field
`entrypoint.bin = "bin/my-example-hello"` is relative to the payload
root.

## Step 8: pack

```
$ tilo package pack \
    --manifest /tmp/hw-walkthrough/pkg-workdir/manifest.toml \
    --payload  /tmp/hw-walkthrough/pkg-workdir/payload \
    --output   /tmp/hw-walkthrough/my-example-hello.tapp
packed tapp:20bd87389c30f2a0fcbb162845b15e88e72aa281a50322c6080dcd811cc311bb (4 blobs)
```

The `tapp:<hex>` is the content-addressed identity: BLAKE3 of the
173-byte signed root. Four blobs: signed root, manifest, collection,
and the entrypoint binary.

## Step 9: install

```
$ tilo app install /tmp/hw-walkthrough/my-example-hello.tapp
my.example.hello 0.1.0 is from an unverified publisher (ed25519:d7830a490384cbbebdc713adec05dddb4ff7db090ca9bf9dd95d775841e47d4a): nobody you trust vouches for it.
Install it anyway? [y/N] y
installed: my.example.hello 0.1.0
  payload  : /tmp/tilo-hw-data/tilo-apps/pkgs/<package-id>
  pinned   : false
run with:  tilo-install run my.example.hello
```

The question is expected — `tilo package pack` generated a fresh keypair for
this build, so nobody you trust signed it. A script passes `--yes` instead of
answering. Add an attestor and use `--require-attestor` (see
[`trust.md`](./trust.md)) when you're ready to harden.

## Step 10: run

```
$ tilo-install run my.example.hello --no-services
tilo-sandbox-exec: landlock: partially enforced (kernel ABI dropped some rights) (12 ro, 2 rw, net_restricted=true)
my-example-hello: running as Tilo-native = false
  collab  : not available (graceful)
  fs      : not available (graceful)
  identity: not available (graceful)
my-example-hello: done.
```

The launcher:

1. Verified the trust policy (no revocation, no `--require-attestor`).
2. Built a bubblewrap argv from the manifest and the fixed system read
   profile.
3. Bind-mounted your payload → `/var/tilo-apps/my.example.hello/` and the
   runtime bundle → `/usr/lib/tilo-runtime/2026.07/`, both read-only, plus
   your app's own directory → `/run/tilo/app`, read-write.
4. Installed the seccomp filter.
5. Forked bwrap, which fork-execed the sandbox-exec shim.
6. The shim installed the Landlock ruleset — read rights on the profile
   entries and the payload, write rights on `/run/tilo/app` and `/tmp` —
   enforced the network restriction, and finally execvp'd
   `/var/tilo-apps/my.example.hello/bin/my-example-hello`. The rule counts
   in the line above track the profile and what your kernel's Landlock
   ABI accepts, so they move between hosts.
7. Your `main()` ran. The `collab`/`fs`/`identity` calls all returned
   `NotAvailable::ServiceFdMissing` gracefully — the launcher didn't
   pass any fds because `--no-services` was set and the manifest
   declared no `[sandbox.services]` deps anyway.

## Optional: install into your user data

The walkthrough above scoped `XDG_DATA_HOME` and `XDG_CONFIG_HOME` to
`/tmp/tilo-hw-data` / `/tmp/tilo-hw-config` for isolation. To install
into your home directory:

```bash
unset XDG_DATA_HOME XDG_CONFIG_HOME
tilo-install runtime install --version 2026.07 --from-host --force
tilo app install /tmp/hw-walkthrough/my-example-hello.tapp
tilo-install list
tilo-install run my.example.hello
```

The install tree lands under `~/.local/share/tilo-apps/` and
`~/.local/share/tilo-runtime/`. Removal:

```bash
tilo app remove my.example.hello
tilo app runtime remove 2026.07
```

## Where to go next

- **Add a `[cli]` block** to your manifest for PATH commands. See
  [`cli-apps.md`](./cli-apps.md).
- **Get a real signing identity** and store it out-of-band. See
  [`trust.md`](./trust.md) for `tilo package pack --identity`.
- **Ship for Linux, macOS and the web** from the same source. See
  [`cross-target.md`](./cross-target.md).
- **Publish a format codec** as a WebAssembly module. See
  [`formats.md`](./formats.md).
- **Understand the runtime API** you can call. See
  [`runtime.md`](./runtime.md).
- **Understand what your manifest is granting** the sandboxed process.
  See [`sandbox.md`](./sandbox.md).
