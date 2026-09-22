# Format providers (file-format codecs)

A Tilo format provider is a WASM Component Model binary that implements
the `tilo:format-provider` interface. The `tilo-format-host` daemon loads
providers via wasmtime, exposes a Cap'n Proto socket, and the editor / FS
layer routes `detect / import / export` calls through it.

Same packaging pipeline: your provider is a `.tapp` with
`kind = "format-provider"` and a WASM binary in the payload.

## The WIT interface

Straight from `crates/doc/format-provider-wit/wit/format-provider.wit`:

```wit
package tilo:format-provider@1.0.0;

interface format-provider {
    record confidence { tier: u32, score: f32 }
    record detected   { confidence: confidence, mime: option<string> }
    record codec-error { kind: string, message: string }

    detect: func(head: list<u8>) -> detected;
    import-doc: func(bytes: list<u8>) -> result<list<u8>, codec-error>;
    export-doc: func(canonical: list<u8>) -> result<list<u8>, codec-error>;
}

world provider {
    export format-provider;
}
```

Three functions:

- **`detect`** — given the first N bytes of a file, return a `(tier,
  score)` confidence plus an optional MIME the provider claims. Called on
  disagreement between multiple providers claiming the same extension.
- **`import-doc`** — given the file bytes, return the **canonical CRDT
  bytes** the Tilo document layer speaks. For qdoc that is the
  `automerge::save()` output of a `qdoc::Document`.
- **`export-doc`** — the inverse: canonical CRDT bytes back to the
  provider's on-disk format.

## The host

The `tilo-format-host` daemon is a system service — not something a
publisher runs. It:

- Listens on `$XDG_RUNTIME_DIR/tilo/services/format-router.sock`.
- Reads `~/.config/tilo/extensions/format-providers.toml` for the
  installed provider list, hot-reloaded via inotify.
- Loads each provider's `.wasm` via `wasmtime 46`, in the Component
  Model, with a 256 MiB linear-memory ceiling per call and a 10 s wall-
  clock deadline (epoch-based).
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
wasm     = "/home/user/.local/share/tilo-format-providers/so.tilo.qdoc-md/component.wasm"
version  = "0.1.0"
priority = "installed"
trust    = ["tilo-curated/2026.07"]
```

Fields:

- `mime` — the MIME type this row claims (repeat the block for multi-
  MIME providers).
- `package` — reverse-DNS id; also the `providerId` clients pass to
  `importAs` / `exportAs`.
- `wasm` — absolute path to the Component Model binary.
- `version` — semver-ish; not parsed, shown in diagnostics.
- `priority` — one of `builtin`, `installed`, `ephemeral`. Priority
  breaks ties when multiple providers claim the same MIME:
  `builtin > installed > ephemeral`.
- `trust` — the trust roots the host considers when admitting the
  provider. Free-form strings, stored verbatim.

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
- ships no `component.wasm` in the payload.

The manifest's `[sandbox]` block still applies — providers run in a
sandbox scoped to the format-host's per-call wasmtime `Store`. Landlock
does not apply INSIDE the WASM sandbox; the host provides its own
resource limits (memory, wall-clock) instead.

## Building a provider: Markdown codec walkthrough

The reference in-tree provider is
`crates/doc/qdoc-codec-markdown-wasm/`. It delegates to the same Rust
codec (`qdoc::MarkdownCodec`) that the editor uses in-process — one code
path for host and WASM builds.

### Cargo.toml

The crate is **excluded from the workspace** because it must be compiled
for `wasm32-wasip2`, not the host target:

```toml
# Cargo.toml (workspace root)
[workspace]
exclude = ["crates/doc/qdoc-codec-markdown-wasm"]

# crates/doc/qdoc-codec-markdown-wasm/Cargo.toml
[workspace]                    # empty table = "stop walking up"

[package]
name = "qdoc-codec-markdown-wasm"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
qdoc = { path = "../qdoc" }
wit-bindgen = "0.57"
```

### src/lib.rs

The guest binds itself to the WIT world via `wit_bindgen::generate!` and
delegates to the shared codec:

```rust
#[cfg(target_arch = "wasm32")]
mod guest {
    use qdoc::{Document, FormatProvider, MarkdownCodec};

    wit_bindgen::generate!({
        path: "../format-provider-wit/wit",
        world: "provider",
    });

    use exports::tilo::format_provider::format_provider::{
        Confidence, Detected, CodecError, Guest,
    };

    struct Component;

    impl Guest for Component {
        fn detect(head: Vec<u8>) -> Detected {
            let conf = MarkdownCodec.detect(&head);
            let (tier, score) = match conf {
                qdoc::Confidence::None => (0, 0.0),
                qdoc::Confidence::Low => (0, 0.25),
                qdoc::Confidence::Medium => (1, 0.5),
                qdoc::Confidence::High => (2, 0.9),
            };
            Detected {
                confidence: Confidence { tier, score },
                mime: Some("text/markdown".to_string()),
            }
        }

        fn import_doc(bytes: Vec<u8>) -> Result<Vec<u8>, CodecError> {
            let mut doc = Document::new();
            MarkdownCodec
                .import(&bytes, &mut doc.as_mut())
                .map_err(|e| CodecError { kind: "parse".into(), message: e.to_string() })?;
            Ok(doc.save())
        }

        fn export_doc(canonical: Vec<u8>) -> Result<Vec<u8>, CodecError> {
            let doc = Document::load(&canonical)
                .map_err(|e| CodecError { kind: "parse".into(), message: e.to_string() })?;
            MarkdownCodec.export(doc.as_ref())
                .map_err(|e| CodecError { kind: "export".into(), message: e.to_string() })
        }
    }

    export!(Component);
}
```

Note the `#[cfg(target_arch = "wasm32")]` gate — building for the host
target gives an empty lib so `cargo check` on the workspace still passes.

### Build

```bash
cargo build \
  --manifest-path crates/doc/qdoc-codec-markdown-wasm/Cargo.toml \
  --target wasm32-wasip2 \
  --release \
  --locked
```

Output at `target/wasm32-wasip2/release/qdoc_codec_markdown_wasm.wasm`.

### Package

Same `tilo-app-package pack` flow as any other package. The `component.wasm` file
must sit at the payload root (a top-level file, not nested).

```
payload/
  component.wasm      # ← the wasm32-wasip2 output, renamed
```

Then:

```bash
tilo-app-package pack \
    --manifest manifest.toml \
    --payload  payload \
    --output   qdoc-md.tapp
```

### Registration

Install via `tilo-install install` (registers the package in `registry.toml`),
then append the provider row to
`~/.config/tilo/extensions/format-providers.toml` pointing at the payload:

```toml
[[provider]]
mime     = "text/markdown"
package  = "so.tilo.qdoc-md"
wasm     = "~/.local/share/tilo-apps/payloads/so.tilo.qdoc-md/component.wasm"
version  = "0.1.0"
priority = "installed"
trust    = []
```

The format-host picks up the change via inotify — no restart needed.

## Canonical bytes

The "canonical CRDT bytes" contract is central. For qdoc-backed formats,
the canonical form is the `automerge::save()` output of a
`qdoc::Document`. The host round-trips those bytes opaquely; the editor
and FS reload them via `qdoc::Document::load` to get a live CRDT back.
Providers for non-qdoc content models will have their own canonical
form; the interface represents each canonical form as bytes.

## Resource limits

Each provider call runs in its own wasmtime `Store` with:

- 256 MiB linear-memory ceiling (`ResourceLimiter::memory_growing`).
- 10 s wall-clock deadline enforced by wasmtime's epoch timer.

An infinite-loop guest is trapped and the host returns a
`CodecError { kind: "resource_limit", message: … }` to the client.
Trust tier and per-provider limits are governed by the registry — future
work; today all installed providers get the same limits.

Next: [`installation.md`](./installation.md) — `tilo-install` verbs and the
install tree.
