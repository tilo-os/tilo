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

`"onnx_file"` is a single ONNX graph that runs in-process in the service that
uses it. Such a model names no `allowed_runtimes`, has `context_window` 0 (both
may be omitted), declares an in-process capability such as
`"segmentation": true`, and cannot claim `vision` or `face_recognition`. It is
verified and installed like any other object, but never started, and it can
never be the catalog's chat `recommendation`. A catalog containing only in-process
models uses an empty `runtimes` array and `"recommendation": ""`; publishing an
image model does not require publishing a chat runtime. Catalogs containing
runtime-served models still require a runtime-served recommendation.

`"whisper_ggml"` is an explicit whisper.cpp speech graph. Like `onnx_file`, it
runs in-process, names no runtime and has a zero context window. Its signed
format declares the `speech_recognition` capability; it cannot claim
segmentation, chat, vision or face recognition. Installation still verifies
the exact object receipt and license acceptance, and never opens a microphone
or starts a process. The native consumer must check the graph's actual language
capabilities and recheck its receipt before inference. This format support
alone installs no speech release or recognizer, and requires exact protected release inputs; it grants no microphone permission.

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

## U²-Net subject selection release

`dist/local-models/u2net-subject/prepare-release.sh` stages the in-process
salient-object model behind "Select subject": U²-Net's general model
(Apache-2.0, https://github.com/xuebinqin/U-2-Net), as the ONNX conversion
rembg publishes. `model-files.sha256` pins the 175997641-byte `u2net.onnx`
(`8d10d2f3bb75ae3b6d527c77944fc5e7dcd94b29809d47a739a7a728a912b491`) and the
upstream `LICENSE` at commit `ac7e1c817ecab7c7dff5ce6b1abba61cd213ff29`.

The upstream asset sits under a release tag, which can move, so it is only a
source for bytes checked against that digest. The model's revision is the
digest itself, and both files are re-hosted unchanged at
`<publish base>/u2net-subject/<revision>/`, so the artifact and licence URLs
carry the immutable revision as the catalog rules require.

```sh
dist/local-models/u2net-subject/prepare-release.sh \
  --output /secure/build/local-models/u2net-subject \
  --publish-base-url "$TILO_RELEASE_ARTIFACT_BASE_URL"
```

Upload `artifacts/u2net.onnx` and `artifacts/LICENSE` unchanged, verify their
published size and SHA-256, then add the generated
`u2net-subject.model.json` to the `models` of the release's
`local-models.json` (for example
`jq --slurpfile m u2net-subject.model.json '.models += $m'`) before compiling
and signing the catalog as above. `contract-test.sh` checks the recipe offline.

### Production delivery

The production model and license now use the immutable Forgejo package URLs in
`dist/local-models/catalog-source.json`. The signed catalog and public trust key
are image inputs at `os/usr/share/tilo/system-extensions/local-model-{catalog,keys}.json`.
The OS copies this directory unchanged; model weights remain an optional download
through Settings, with explicit license acceptance, outside Canvas's package.

The dedicated key id is `tilo-local-model-release-2026-10`. Its encrypted recovery
record and public identity are documented in the infra repository at
`operations/local-model-release-20261005/README.md`. It is independent of the
development, registry, OS, and runtime signing keys.

`t dispatch publish-model-catalog --ref stable` reproduces and publishes the
reviewed catalog (`tools/release/local-model-catalog.sh`). The compiler is built
by `model-catalog-compiler`, which receives no model signing key.
`sign-model-catalog` fetches the public weights and license, checks their pinned
receipts, signs with `TILO_MODEL_CATALOG_SIGNING_KEY` and compares both outputs
with the checked-in image inputs; `publish-model-catalog`, holding only the
package token, uploads them and verifies anonymous downloads after publication.
It never changes the catalog carried by an already-installed OS; normal image
releases deliver that trust and catalog update.


## Speech candidate review and publication

The protected publisher also accepts `mode=review-speech`. It prepares exactly
U²-Net and multilingual Whisper tiny, base and small-q5_1 from
`catalog-source-speech-candidate.json`, checks upstream receipts and complete MIT
notices, then signs a review artifact with the existing production key. The
resulting public key must equal the checked-in key. The uploaded artifact
contains only catalog/key/source/review hashes; the signing seed and weights
are excluded. This phase neither publishes objects nor activates an image.

After reviewing the signed artifact, commit its exact source and catalog bytes
through the normal protected PR path. Dispatch the default `publish` mode to
reproduce those reviewed image bytes. Speech weights and notices use pinned
immutable Forgejo package URLs; `publish_speech_artifacts.py` verifies anonymous
readback before signing and refuses existing receipt conflicts or unexpected
redirects. Only an actual 404 permits upload, using the existing release token.
The final catalog/key comparison against the image inputs remains mandatory.
U²-Net-only reproduction remains supported until speech image inputs land.

Model installation through Settings requires license acceptance and never
opens a microphone. Dictation's local one-session recording consent is separate
from model publication, installation, or the chosen language.

The speech preparer, publisher extension, format/capability support and pinned
licence/weight receipts are source work; they can be reviewed without release
credentials. The workflow extension must first land on protected main because
both review and publication refuse task refs. The remaining protected operations
use the existing `TILO_MODEL_CATALOG_SIGNING_KEY` and `RELEASE_TOKEN`; no new key,
account or persistent trust grant is needed for in-process Whisper. Approval for
source work or prerecorded qualification does not authorize either dispatch,
image activation, licence acceptance on someone's behalf, or installation.

## Typed runtime execution

Runtime admission and Settings' hardware recommendation use effective RAM
capacity: physical RAM capped by every visible cgroup v2 `memory.max` ancestor
of the supervisor's fixed user `app.slice`. The runtime explicitly retains this
existing slice placement. The caller's own cgroup does not choose the runtime's
budget. Unlimited limits preserve physical capacity; an unavailable, malformed
or redirected observation reports an error rather than a successful fit.

This observes capacity, not current resident headroom. It does not subtract
`memory.current`, count swap as RAM, reserve memory, or establish model buffer
placement or performance. Limits may change after observation; the kernel still
enforces them. In-process models retain their separate installation path.

A runtime source may include signed `execution` settings. Omitting them retains
legacy vLLM serialization/signing bytes. `engine: "vllm"` requires
`max_context_tokens` and `cpu_offload_gib`; `engine: "llama_cpp"` requires
`max_context_tokens` and `gpu_layers`. Its optional `parallel_slots` is a nonzero
integer from 1 through 64 and generates only `--parallel N`. Omission preserves
existing signing bytes and leaves the backend default unchanged. Slot count does
not reduce signed memory requirements or attest resident fit or throughput.
There is no arbitrary command/argv field.
The catalogue compiler and supervisor reject malformed or unbounded settings.
The selected image must have the corresponding server as its pinned entrypoint.

`llama_cpp_gguf` is a separate explicit single-file format. Engine/layout
compatibility is checked during selection, installation and launch. Context is
capped by signed runtime settings and model context; localhost binding and the
served model alias are generated by the service. Before publishing a release,
verify the precise runtime build, artifact conversion, template and tool-use
behavior; generic engine support does not attest a working model release.

## Owned runtime observations

The existing manager `snapshot --format json` reports additive
`runtime_observations` for current owned runs and `retained_run_observations` for
private checkpoints. Empty fields remain omitted. These are content-free host
observations, not signed portable attestations or a resident-fit decision.

Each sample rechecks exact container labels, image and argv digest, unit
invocation, payload PID/start ticks and cgroup directory identity before and
after collection. Wrapper and payload cgroups are reported separately: the
systemd main process is the Podman attachment, and its accounting must not be
substituted for payload accounting. Cgroup counters aggregate descendants;
they do not identify individual model tensor allocations.

Observed memory values use bytes; selected memory-stat/event counters retain
kernel field names, CPU fields retain their microsecond/count names, and I/O
fields preserve device identifiers and kernel byte/request names. `max` is an
observed unlimited limit, not a numeric zero. Missing, malformed, inaccessible
or identity-changing observations explicitly report `unavailable`. Backend
buffer sizes and GPU allocations are not collected. No prompt, generated text,
reasoning, environment or command argument contents are retained.

Ready, failed-start and before-stop checkpoints are exclusive mode-0600 files
under an owned mode-0700 `run-observations` directory. They survive cleanup;
repeat checkpoints never overwrite prior evidence. Stop captures counters before removal and writes its checkpoint after exact
cleanup and the state transaction, so journal I/O cannot delay those steps.
Retention failures emit a content-free diagnostic.
Snapshot reads are bounded to 256 checkpoints/256 KiB; exceeding this returns
an explicit unavailable journal. Live collection is bounded to two runs per
snapshot; additional runs explicitly report the collection bound. Each host
command has a three-second deadline. These observations describe sampling
intervals, not an atomic whole-host snapshot or a reservation of memory.

The existing `runtime-cleanup-qualification` example uses two newly owned fixed
sleep containers, captures their separate wrapper/payload observations, and
verifies exact cleanup while preserving the independent owned control until
its own final cleanup. It loads no model and proves no inference performance.
