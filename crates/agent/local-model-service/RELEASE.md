# Local model catalog releases

`tilo-model-catalog` is the only supported compiler for the catalog consumed by
`tilo-model-manager`. It turns a declarative JSON source plus local copies of
the pinned inputs into two image inputs:

- `catalog.json`: model releases, signed runtime-extension manifests, artifact
  receipts, and a domain-separated Ed25519 signature;
- `keys.json`: the public verification key indexed by release key id.

The signing seed is an input only. The compiler requires its file to be mode
`0600` (or stricter), reads exactly 32 bytes encoded as lowercase hexadecimal,
and never writes that seed into either output.

```sh
cargo run -p local-model-service --bin tilo-model-catalog -- \
  --source /secure/build/local-models/local-models.json \
  --signing-key /run/secrets/tilo-model-release-key \
  --catalog-out dist/usr/share/tilo/system-extensions/local-model-catalog.json \
  --trust-out dist/usr/share/tilo/system-extensions/local-model-keys.json
```

The source must point at regular files already fetched by the release job.
Their byte count and SHA-256 are recomputed; declared receipts are comparisons,
not trusted input. Model revisions must be exact 40- or 64-character lowercase
commit hashes and the immutable revision must occur in every model URL.
Floating branches, tags, `latest`, redirects encoded as query strings,
symlinks, empty artifacts, and production `.invalid` URLs are rejected.

Nothing is mirrored or repackaged; every byte is fetched from its vendor:

- **Models** are `"artifact": {"hf_snapshot": {"files": [...]}}`: every file
  of the snapshot, each pinned by SHA-256 and size and fetched from its own
  Hugging Face resolve URL at the pinned revision
  (`https://huggingface.co/<repo>/resolve/<revision>/<path>`). The manager
  verifies each file into the content-addressed store, then links them into a
  private snapshot directory whose paths come only from the signed listing.
  `{"gguf": {...}}` is the single-file alternative; no model is treated as a
  single file by inference.
- **Runtimes** are OCI images pulled from their registry by manifest digest.
  The runtime's `artifact` is the image manifest itself (its registry URL,
  SHA-256 and size), and the compiler refuses a manifest that does not hash to
  the digest `oci_image` names, so the signed extension covers exactly what the
  container engine verifies on pull. `engine` names the server inside
  (`vllm`); the manager builds that engine's argv from signed fields.

A runtime container has no network (`--network none`). It serves its
OpenAI-compatible API on a Unix socket in a private per-model directory, the
only thing mounted writable. Settings records that socket as the profile's
`Supervised` custody, which is what lets the gateway treat the model as unable
to send anything off the device.

The catalog signature covers:

```text
"tilo.local-model.catalog.v1" || 0x00 || compact JSON(payload)
```

The checked-in `tests/fixtures/catalog-source.example.json` is explicitly
`"production": false`; its tiny text artifacts exercise deterministic tooling
and are not model weights, a runtime, or approved upstream release pins.

## Release inputs

`dist/local-models/` holds the pinned inputs, one directory per model:

| Model | Capability | Revision | Notes |
| --- | --- | --- | --- |
| `qwen3-4b-awq` (`Qwen/Qwen3-4B-AWQ`) | `trace_review` | `74d4bd2b…` | Apache-2.0, 2.7 GB, 8K context, 70 % of an 8 GiB card |
| `smolvlm2-500m` (`HuggingFaceTB/SmolVLM2-500M-Video-Instruct`) | `vision` | `a3992d50…` | Apache-2.0; no biometric capability |

Each has `release.json` (catalog metadata) and `model-files.sha256` (every
served file with its SHA-256). `runtimes/` pins the vLLM 0.10.1 CUDA image by
digest. `prepare-release.py` fetches and verifies all of them and writes the
compiler source; `tests/test_prepare_release.py` checks the pins offline.

```sh
dist/local-models/prepare-release.py \
  --output /secure/build/local-models \
  --base-image-digest "$TILO_BASE_IMAGE_DIGEST" \
  --recommendation qwen/qwen3-4b-awq \
  dist/local-models/qwen3-4b-awq dist/local-models/smolvlm2-500m
```

It needs `curl` and `skopeo`. Then compile
`/secure/build/local-models/local-models.json` with the release key as above.

## Developer catalog on this machine

A developer signs with a key of their own; the release key never leaves
release infrastructure. The model manager is a per-user service and accepts
`--catalog`, `--keys` and `--root` overrides only when it is not elevated, so
this is the production install path pointed at a dev-signed catalog. It needs
rootless Podman with the NVIDIA CDI spec (`/etc/cdi/nvidia.yaml`), a working
NVIDIA driver (`nvidia-smi` must succeed), a user systemd instance, and about
23 GB of disk, 20 GB of it the vLLM image.

```sh
# 1. Build the manager, the compiler and the gateway.
cargo build --release -p local-model-service -p tilo-model-gateway

# 2. Fetch and verify the pinned inputs (2.7 GB of weights, the image manifest).
# The compiler refuses symlinked path components (on Fedora Atomic /home is one).
dev=$(realpath -m ~/.local/share/tilo/dev-models)
dist/local-models/prepare-release.py --output "$dev/inputs" \
  --base-image-digest "sha256:$(printf dev | sha256sum | cut -d' ' -f1)" \
  --key-id tilo-dev-local-models --recommendation qwen/qwen3-4b-awq \
  dist/local-models/qwen3-4b-awq

# 3. Sign with a dev key.
(umask 077; od -An -tx1 -N32 /dev/urandom | tr -d ' \n' > "$dev/signing-key")
target/release/tilo-model-catalog --source "$dev/inputs/local-models.json" \
  --signing-key "$dev/signing-key" \
  --catalog-out "$dev/catalog.json" --trust-out "$dev/keys.json"

# 4. Install and start through the manager: it downloads each file from
#    Hugging Face, pulls the image by digest, and starts vLLM without network.
manager() { target/release/tilo-model-manager --catalog "$dev/catalog.json" --keys "$dev/keys.json" "$@"; }
manager install --model qwen/qwen3-4b-awq
manager snapshot   # repeat until the phase is ready with an endpoint socket
```

Settings › Agent models then records the ready model in the local provider
profile (supervised custody, `trace_review` capability, pinned revision); on a
Tilo desktop that is the normal sync. With the gateway on that profile, run a
real review as the desktop session does:

```sh
dist/local-models/qwen3-4b-awq/smoke-review.py \
  --socket "$XDG_RUNTIME_DIR/tilo/services/model-gateway.sock"
```

The answer is the gateway's validated `ReviewAnswer`: `model` names the
reviewer and revision, and `proposals` holds redactions addressed by step,
node and byte span, and step titles. For Photos, `smolvlm2-500m/smoke-gateway.py`
sends the fixed image request the same way with the Photos app principal.
