# Local model catalog releases

`tilo-model-catalog` is the only supported compiler for the catalog consumed by
`tilo-model-manager`. It turns a declarative JSON source plus local release
artifacts into two image inputs:

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
commit hashes and the immutable revision must occur in the HTTPS artifact URL.
Runtime container identities must end in `@sha256:<64 lowercase hex>`. Floating
branches, tags, `latest`, redirects encoded as query strings, symlinks, empty
artifacts, and production `.invalid` URLs are rejected.

Each model must explicitly declare `"artifact_format"`. Production Hugging
Face/vLLM releases use `"hf_snapshot_tar_zst"`: the signed compressed object
is verified first, then extracted into a content-addressed directory only
after rejecting traversal, links, special files, excessive entry counts, and
expansion beyond `installed_size_bytes`. `"vllm_gguf"` is the sole explicit
single-file option; no model is treated as a single file by inference.

The catalog signature covers:

```text
"tilo.local-model.catalog.v1" || 0x00 || compact JSON(payload)
```

The checked-in `tests/fixtures/catalog-source.example.json` is explicitly
`"production": false`; its tiny text artifacts exercise deterministic tooling
and are not model weights, a runtime, or approved upstream release pins.

## SmolVLM2 500M visual analysis release

`dist/local-models/smolvlm2-500m/prepare-release.sh` is the production
recipe for the first local visual-analysis delivery. It stages exactly these
upstream inputs:

- `HuggingFaceTB/SmolVLM2-500M-Video-Instruct` at
  `a3992d502a8714ce2f6207fde3507ccf9503f224`. Its model card at that exact
  revision declares Apache-2.0; the checked-in manifest pins every file used
  by the vLLM snapshot, including `model.safetensors`.
- `docker.io/vllm/vllm-openai@sha256:c62343ee72f094ef574ab3af2717d0c60fec5e0808b238f19ac4c0d22fd152f7`
  (vLLM 0.10.1), saved as a verified OCI archive for `x86_64` NVIDIA CUDA.

It deliberately creates no CPU fallback and no biometric capability. The
model is catalogued with `vision: true` and `face_recognition: false`; it can
serve the bounded Photos caption/label/pet/OCR request, but cannot enable the
separate face clustering route.

The recipe downloads roughly 2 GB of model data plus the OCI runtime and
requires `curl`, `skopeo`, GNU `tar`, and `zstd`. It calculates the final
archive receipts locally, so no placeholder weight hash or mutable upstream
download URL can become a signed catalog.

```sh
dist/local-models/smolvlm2-500m/prepare-release.sh \
  --output /secure/build/local-models/smolvlm2-500m \
  --publish-base-url "$TILO_RELEASE_ARTIFACT_BASE_URL" \
  --base-image-digest "$TILO_BASE_IMAGE_DIGEST"
```

`TILO_RELEASE_ARTIFACT_BASE_URL` must be the permanent HTTPS artifact host;
`TILO_BASE_IMAGE_DIGEST` is the exact, already-approved desktop base-image
digest. Upload the two files in `artifacts/` unchanged, then fetch the actual
published URLs and compare their size and SHA-256 with the generated
`local-models.json`. Only after that evidence is retained may the release key
compile and sign the catalog:

```sh
release_dir=/secure/build/local-models/smolvlm2-500m
cargo run -p local-model-service --bin tilo-model-catalog -- \
  --source "$release_dir/local-models.json" \
  --signing-key /run/secrets/tilo-model-release-key \
  --catalog-out dist/usr/share/tilo/system-extensions/local-model-catalog.json \
  --trust-out dist/usr/share/tilo/system-extensions/local-model-keys.json
```

Before image publication, validate the signed files and retain the command
output with the release record:

```sh
cargo run -p local-model-service --bin tilo-model-catalog -- --help
dist/local-models/smolvlm2-500m/contract-test.sh
```

### Genuine inference acceptance

On an `x86_64` desktop with an NVIDIA driver, at least 16 GiB RAM, 8 GiB VRAM,
rootless Podman, and a working user systemd instance, deploy the signed catalog
and use Settings to install and start **SmolVLM2 500M Video Instruct**. Settings
must show a ready loopback endpoint before proceeding; its normal sync writes
the signed model id, revision, and capability into the local provider profile.

For a developer-only probe, start a separate local gateway listener that reads
that same profile, then send the fixed Photos image request through the real
gateway. The probe checks the actual vLLM response envelope and fixed analysis
schema; it does not accept a mocked response or a face result.

```sh
scratch=$(mktemp -d)
target/release/tilo-model-gateway serve --socket "$scratch/model-gateway.sock" &
gateway_pid=$!
# Replace this with the profile id Settings created for the ready local model.
dist/local-models/smolvlm2-500m/smoke-gateway.py \
  --socket "$scratch/model-gateway.sock" \
  --profile "$TILO_LOCAL_VLLM_PROFILE_ID" \
  --model huggingface/smolvlm2-500m-video-instruct
kill "$gateway_pid"
```

The desktop launcher, rather than this developer socket, supplies the normal
gateway descriptor to Photos. A successful probe is evidence that the signed
model service, vLLM runtime, provider profile, and bounded gateway all carried
out real image inference.
