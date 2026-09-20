# Finalization-only driver

`finalize-development-rootfs.py --helpers /work/helpers --request
/work/finalization-request.json --evidence /evidence` runs in an admitted native
rootful tools worker. It consumes retained artifacts and does not compile the
application workspace, regenerate private keys, install a disk, or boot a VM.

The launcher provides authenticated, read-only source and coherent helper
checkouts and writes `.finalization-source.json` / `.finalization-helpers.json`
with exactly `{"revision":"FULL_COMMIT"}` after checking the corresponding
checkout. The source recipe remains the original full OS Containerfile; the
final target is built with the signed immutable rootfs as a named build-context
override and unused stages skipped. Source and helper commits may differ.

## Request

Schema1 has exactly these fields:

- `source_revision`, `source_dir`, `helpers_revision`: exact committed source,
  canonical source directory, and authenticated coherent helper revision.
- `receipt`, `receipt_sha256`, `public_bundle`, `manifest_sha256`, `target`:
  retained raw receipt and original public trust inputs. The rebind helper
  verifies registry signature, handoff binding, immutable image and actual
  mounted raw contents before transformation.
- `new_public_bundle`, `new_manifest_sha256`, `new_target`, `new_owner`:
  persistent public trust and intended boot origin. No private key fields.
- `architecture`, `generation`, `source_date_epoch`, `systemd_version`:
  native architecture and independently pinned build/measurement identities.
- `base_ref`, `rpms_ref`, `kernel_ref`, `wpe_ref`: immutable original build
  references. Only the base/RPM images needed for final UKI construction are
  pulled; the skipped stages still require their original parser arguments.
- `signer`: root-owned external adapter outside the checkout, installed before
  the launcher's operator-ready gate is released.
- `minimum_free_bytes`: admitted free-space reserve checked on the actual
  container store and evidence filesystem before every stage, including pulls.
  This is a reserve, not a claim to predict unknown expansion. The launcher
  must separately admit the complete raw image and scratch peak to its volume.
- `stage_timeout_seconds`: bounded non-signing command timeout, 60–3600 seconds.
- `signing_response_timeout_seconds`: request-bound deadline for each external
  signing response, 60–7200 seconds. The staged bridge copies this exact value
  into its configuration; it is never supplied by worker-local defaults.
- `prediction_stage_timeout_seconds`: aggregate deadline for all stage-6 work,
  300–8400 seconds, excluding four separately bounded human signing-response
  windows. The complete worker deadline is bounded independently by the
  launcher and workflow.
- `publish_repository`: `code.podesta.ai/qos/tilo-development`, exactly the
  repository portion of `new_target`. The driver never updates the intended
  update tag. It writes `finalization-<canonical-request-sha256>` and records the
  immutable manifest reference. Other repositories are rejected so signatures
  and the installed policy remain bound to the same repository.

Requests that omit either signing deadline are rejected. Supplying a worker
default would make the effective signing authorization differ from the exact
request bytes bound into checkpoints and the operator handshake.

The request itself and all bundle paths are public. Registry authentication
uses the existing worker configuration; its credential file is not copied into
build contexts or public receipts. Local signing authorization remains separate
from this driver; see REMOTE-SIGNING.md.

## Checkpoints and resumption

The evidence directory is root-owned0700. Export only these public files:

1. `01-rebind.json`: authenticated raw import, public trust rebinding and dracut.
2. `02-loader.json`: signed loader verified and added before root sealing.
3. `03-provisional.json`: final split/UKI target assembled from retained rootfs.
4. `04-composefs.json`: independently computed bootc1.16.4 root digest.
5. `05-reseal.json`: immutable boot-only reseal layer.
6. `06-prediction.json`: signed live/target UKIs and authority predictions.
7. `07-verified.json`: unchanged root digest, mounted trust/UKI/prediction checks,
   and bounded all-layer private-PEM audit. Audit scope is explicitly limited;
   this does not claim universal secret absence.
8. `08-published.json`: completed push plus exact registry manifest/config byte
   hashes, architecture and revision checks.
9. `result.json`, `status.json`: public outcome and current checkpoint status.

Completed checkpoints are chained to the exact canonical request and previous
receipt. Resuming with the same surviving store rechecks immutable image IDs
and skips completed signing stages. A `.started-*` marker without an accepted
checkpoint forbids automatic retry. Inspect the actual live process and remote
signing request; a marker alone does not prove the process has stopped. Resolve
pending requests and durable local signer authorizations before explicitly
starting a new job. The driver does not erase reservations or regenerate keys.

Prediction signing currently uses the existing helper's mutable-tag API under
one request-owned local alias; only its verified immutable result is retained
in checkpoints. No existing user tag is used. This preserves the coherent
helper API without duplicating prediction construction.

The CI store is ephemeral. Success therefore requires publication before the
driver returns and the launcher removes its owned worker/store. Interrupted
jobs export available bounded public checkpoints; local signing state/results
remain on the operator machine. A new explicitly approved attempt may repeat
cheap finalization stages from retained artifacts; it never recompiles apps.

## What success means

The returned image is retained and its public boot artifacts are verified.
Registry Cosign signing and fresh signature verification are **still required**
and performed locally against the immutable digest. `installable` stays false;
installed-disk byte checks and VM boot acceptance remain separate. The driver
never reports physical hardware or dual-boot acceptance.

Tests cover real deadline/process-group cleanup, checkpoint/replay rules,
source-reuse command construction, immutable registry-byte verification,
repository binding, and complete stage wiring with delegated helpers mocked.
They do not substitute for running finalization on the real assembled image.
