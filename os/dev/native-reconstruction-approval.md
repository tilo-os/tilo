# Offline reconstruction approval

Run `approve-native-reconstruction.py` after obtaining the complete reconstructed
receipt. It does not contact a registry or run image code. Supply independently
reviewed receipt/checkpoint hashes and original RPM/WPE inputs, not values copied
from the unreviewed reconstructed receipt.

```text
python os/dev/approve-native-reconstruction.py
  --source CLEAN_FROZEN_SOURCE_CHECKOUT
  --helper-repo TRUSTED_REPOSITORY
  --helper-revision 5a410e5303f11b4f5ff27aefc4ba9fc26df5c77c
  --receipt RECONSTRUCTED_NATIVE_JSON --receipt-sha256 EXACT_RECEIPT_SHA256
  --checkpoint ORIGINAL_INITIAL_RECEIPT c81b8a9308ef7afb850271b499df49cb7afb162b2df86397e422450df6a084c1
  --checkpoint ORIGINAL_DELTA_RECEIPT c3bb9e84d824190aa7f4c237254c636b991adaf9b40f79ef89e21c1f2581ce44
  --rpms code.podesta.ai/qos/tilo-development-rpms-aarch64@sha256:912255637bab849ee5c3f62a963481f0b994576ef08cec8ff4fe4bc4cfb9e02f
  --wpe-reference code.podesta.ai/qos/tilo-development-wpe-adopted-aarch64@sha256:7a4ce1f1d8985977a18f3b360ff0ff2fda4f53770fee7f6389570c300a4be8ed
  --wpe-receipt-sha256 df41194aae2531d2cd9cd815cbce8009dd9dc254551b64154137228578ef32e2
  --run-id 21517 --attempt 1 --output NEW_APPROVAL_JSON
```

Join these argument lines using the local shell's continuation syntax. The
invocation above describes retry8129 (internal21517, attempt1), dispatched using
workflow2902317188be41e32e8322b1b7e00765bc05404d and the independently pinned
helper5a410. The fresh API observation on 2026-09-18 at04:16:45Z reported running;
no complete artifact or approval is asserted. Failed8077 used internal21461 and
helper6d390; retain that historical evidence and do not reuse those invocation
identities for8129. The finalization archive2d135 is a separate helper role.

The verifier loads original native input semantics from clean source4583 after
comparing executable helper files to their Git blobs. Base, epoch and recipe hash
come from that commit; RPM/WPE inputs come from the explicit arguments. Derivation
code and producer hashes come from exact helper Git blobs, independently of its
working checkout. It reconstructs the checkpoint union, missing target groups,
recipe and driver bytes, then compares the entire reconstruction object and
calculates its hash locally. Recorded absolute paths and seed image ID are public
driver substitutions; no recorded path is executed or opened.

Only a matching receipt yields an exclusive new approval file binding
`approved_reconstruction_sha256` to `receipt_sha256`. Both can then be supplied to
the explicit reconstructed-native consumer. Registry identity, seed contents,
complete artifact inventory and boot are **not** established offline; the normal
full inventory importer and later installation acceptance still apply.
