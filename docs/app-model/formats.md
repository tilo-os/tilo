# Format providers (file-format codecs)

A Tilo format provider is a core WebAssembly module that implements the
format-provider ABI from `crates/doc/format-provider-abi`. The
`tilo-format-host` daemon runs providers under the wasmi interpreter,
exposes a Cap'n Proto socket, and the editor / FS layer routes
`detect / import / export` calls through it.

Same packaging pipeline: your provider is a `.tapp` with
`kind = "format-provider"` and a `provider.wasm` in the payload.

## The provider ABI

A provider does three things:

- **`detect`** — given the first N bytes of a file, return a `(tier,
  score)` confidence plus an optional MIME the provider claims. Called on
  disagreement between multiple providers claiming the same extension.
- **`import`** — given the file bytes, return the **canonical CRDT
  bytes** the Tilo document layer speaks. For qdoc that is the
  `automerge::save()` output of a `tilo_doc::Document`.
- **`export`** — the inverse: canonical CRDT bytes back to the
  provider's on-disk format.

Rust providers implement the `format_provider_abi::Provider` trait and
invoke `format_provider_abi::export_provider!`, which emits the module
exports. Providers in other languages implement the exports directly
(ABI version 1):

| Export                | Signature           | Meaning                                  |
| --------------------- | ------------------- | ---------------------------------------- |
| `memory`              | memory              | The guest's linear memory.               |
| `tilo_fp_abi_version` | `() -> i32`         | Returns `1`.                             |
| `tilo_fp_alloc`       | `(len: i32) -> i32` | Reserves `len` bytes for host input.     |
| `tilo_fp_detect`      | `(ptr, len) -> i64` | Sniffs a file head.                      |
| `tilo_fp_import`      | `(ptr, len) -> i64` | Native format bytes to canonical bytes.  |
| `tilo_fp_export`      | `(ptr, len) -> i64` | Canonical bytes to native format bytes.  |

Every call runs in a fresh instance: the host calls `tilo_fp_alloc`,
writes its input there, then calls an entry point with that region. The
entry point returns a response region packed as `ptr | len << 32`:

- `detect` responds with `tier: u32 LE | score: f32 LE | mime`, where an
  empty MIME means none.
- `import` / `export` respond with `0 | payload` on success, or
  `1 | kind_len: u32 LE | kind | message` for a codec error.

The instance is discarded after the call, so guests never free memory.

The only import a provider may declare is `tilo_fp.fill_random(ptr: i32,
len: i32)`, which fills guest memory with host entropy. A provider that
imports anything else (WASI included) is refused when the host loads it.

## The host

The `tilo-format-host` daemon is a system service — not something a
publisher runs. It:

- Listens on `$XDG_RUNTIME_DIR/tilo/services/format-router.sock`.
- Reads `~/.config/tilo/extensions/format-providers.toml` for the
  installed provider list, hot-reloaded via inotify.
- Loads each provider's `.wasm` under wasmi, checks its exports, imports,
  and ABI version once, and runs every call in a fresh instance with a
  256 MiB linear-memory ceiling and a fuel budget (see
  [Resource limits](#resource-limits)).
- Exposes the Cap'n Proto `FormatRouter` interface:

```capnp
interface FormatRouter {
    detect    @0 (head :Data, hint :Text) -> (detected :Detected);
    importAs  @1 (bytes :Data, providerId :Text) -> (canonical :Data);
    exportAs  @2 (canonical :Data, providerId :Text) -> (bytes :Data);
    list      @3 () -> (providers :List(Text));
}
```

Clients (the editor, the FS daemon, an app that wants to convert a file
outside its native codec) call these.

## The registry file

The host reads its provider list from
`~/.config/tilo/extensions/format-providers.toml`:

```toml
[[provider]]
mime     = "text/markdown"
package  = "so.tilo.qdoc-md"
wasm     = "/home/user/.local/share/tilo-format-providers/so.tilo.qdoc-md/provider.wasm"
version  = "0.1.0"
priority = "installed"
trust    = ["tilo-curated/2026.07"]
```

Fields:

- `mime` — the MIME type this row claims (repeat the block for multi-
  MIME providers).
- `package` — reverse-DNS id; also the `providerId` clients pass to
  `importAs` / `exportAs`.
- `wasm` — absolute path to the provider module.
- `version` — semver-ish; not parsed, shown in diagnostics.
- `priority` — one of `builtin`, `installed`, `ephemeral`. Priority
  breaks ties when multiple providers claim the same MIME:
  `builtin > installed > ephemeral`.
- `trust` — the trust roots the host considers when admitting the
  provider. Free-form strings, stored verbatim.

An optional `[limits]` table overrides the per-call budget for every
provider:

```toml
[limits]
fuel           = 500000000000
max_memory_mib = 256
```

Installers writing new provider `.tapp`s should append a row here (the
`tilo-install` integration for provider packages is scoped to future
work; today registration is a hand-edit of this file).

## The manifest side

Your provider's `manifest.toml`:

```toml
[package]
id      = "so.tilo.qdoc-md"
version = "0.1.0"
kind    = "format-provider"

[publisher]
identity  = "ed25519:<64-hex>"
signature = ""

[sandbox]
namespaces = ["user", "pid", "uts", "ipc", "mount", "net"]
seccomp    = "tilo-default"
landlock   = "strict"

[sandbox.net]
mode = "off"

[providers]
mime = ["text/markdown"]
ext  = ["md", "markdown"]
```

Validation refuses a `kind = "format-provider"` package that:

- has an empty `providers.mime`, or
- ships no `provider.wasm` in the payload.

The manifest's `[sandbox]` block still applies to the package, but a
provider never runs as a process: it runs inside the format host's
interpreter with no imports beyond host entropy, so it has no file,
network, clock, or environment access. The host enforces its own
resource limits (memory, fuel) instead of Landlock.

## Building a provider: Markdown codec walkthrough

The reference in-tree provider is
`crates/doc/tilo-doc-codec-markdown-wasm/`. It delegates to the same Rust
codec (`tilo_doc_codecs::MarkdownCodec`) that the editor uses in-process — one code
path for host and WASM builds.

### Cargo.toml

The crate is **excluded from the workspace** because it must be compiled
for `wasm32-unknown-unknown`, not the host target:

```toml
# Cargo.toml (workspace root)
[workspace]
exclude = ["crates/doc/tilo-doc-codec-markdown-wasm"]

# crates/doc/tilo-doc-codec-markdown-wasm/Cargo.toml
[workspace]                    # empty table = "stop walking up"

[package]
name = "tilo-doc-codec-markdown-wasm"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
tilo-doc = { path = "../qdoc" }
tilo-doc-codecs = { path = "../tilo-doc-codecs", default-features = false, features = ["markdown"] }
format-provider-abi = { path = "../format-provider-abi", features = ["entropy"] }
```

Automerge draws actor ids from `getrandom`, which has no entropy source on
`wasm32-unknown-unknown`. The `entropy` feature plugs in a `getrandom`
backend that calls the host's `tilo_fp.fill_random`; select it in the
crate's `.cargo/config.toml`:

```toml
[target.wasm32-unknown-unknown]
rustflags = ["--cfg", 'getrandom_backend="custom"']
```

Providers that need no randomness leave the feature off and need no
config.

### src/lib.rs

```rust
#[cfg(target_arch = "wasm32")]
mod guest {
    use format_provider_abi::{CodecError, Confidence, Detected, Provider};
    use tilo_doc::Document;
    use tilo_doc_codecs::{FormatProvider, MarkdownCodec};

    struct Markdown;

    impl Provider for Markdown {
        fn detect(head: &[u8]) -> Detected {
            let (tier, score) = match MarkdownCodec.detect(head) {
                tilo_doc_codecs::Confidence::None => (0, 0.0),
                tilo_doc_codecs::Confidence::Low => (0, 0.25),
                tilo_doc_codecs::Confidence::Medium => (1, 0.5),
                tilo_doc_codecs::Confidence::High => (2, 0.9),
            };
            Detected {
                confidence: Confidence { tier, score },
                mime: Some("text/markdown".to_owned()),
            }
        }

        fn import_doc(bytes: &[u8]) -> Result<Vec<u8>, CodecError> {
            let mut doc = Document::new();
            MarkdownCodec
                .import(bytes, &mut doc.as_mut())
                .map_err(|e| CodecError::parse(e.to_string()))?;
            Ok(doc.save())
        }

        fn export_doc(canonical: &[u8]) -> Result<Vec<u8>, CodecError> {
            let doc = Document::load(canonical).map_err(|e| CodecError::parse(e.to_string()))?;
            MarkdownCodec
                .export(doc.as_ref())
                .map_err(|e| CodecError::export(e.to_string()))
        }
    }

    format_provider_abi::export_provider!(Markdown);
}
```

Note the `#[cfg(target_arch = "wasm32")]` gate — building for the host
target gives an empty lib so `cargo check` on the workspace still passes.

### Build

Cargo reads `.cargo/config.toml` from the working directory, so build from
the crate directory:

```bash
crates/doc/tilo-doc-codec-markdown-wasm/build.sh
```

Output at
`crates/doc/tilo-doc-codec-markdown-wasm/target/wasm32-unknown-unknown/release/tilo_doc_codec_markdown_wasm.wasm`.
`crates/doc/format-host/tests/run-provider-integration.sh` builds it and
runs the host's end-to-end tests against it.

### Package

Same `tilo package pack` flow as any other package. The
`provider.wasm` file must sit at the payload root (a top-level file, not
nested).

```
payload/
  provider.wasm      # ← the wasm32-unknown-unknown output, renamed
```

Then:

```bash
tilo package pack \
    --manifest manifest.toml \
    --payload  payload \
    --output   qdoc-md.tapp
```

### Registration

Install via `tilo app install` (registers the package in `registry.toml`),
then append the provider row to
`~/.config/tilo/extensions/format-providers.toml` pointing at the payload:

```toml
[[provider]]
mime     = "text/markdown"
package  = "so.tilo.qdoc-md"
wasm     = "~/.local/share/tilo-apps/pkgs/<package-id>/provider.wasm"
version  = "0.1.0"
priority = "installed"
trust    = []
```

The format-host picks up the change via inotify — no restart needed.

## Canonical bytes

The "canonical CRDT bytes" contract is central. For qdoc-backed formats,
the canonical form is the `automerge::save()` output of a
`tilo_doc::Document`. The host round-trips those bytes opaquely; the editor
and FS reload them via `tilo_doc::Document::load` to get a live CRDT back.
Providers for non-qdoc content models will have their own canonical
form; the ABI represents each canonical form as bytes.

## Resource limits

Each provider call runs in its own wasmi `Store` with:

- a 256 MiB linear-memory ceiling; growth past it traps;
- a fuel budget of 500 billion units. Fuel counts executed instructions,
  so the cap is deterministic: a runaway guest stops after the same amount
  of work on every machine. Importing 1 MB of Markdown with the builtin
  provider uses about 220 billion units.

A guest that exhausts either limit is trapped and the host returns a
`CodecError { kind: "resource_limit", message: … }` to the client; other
traps come back as `kind: "trap"`. The `[limits]` registry table adjusts
both caps; today all providers share the same limits.

Next: [`installation.md`](./installation.md) — `tilo-install` verbs and the
install tree.
