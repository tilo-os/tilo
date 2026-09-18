# Offline reconstruction approval

Run `approve-native-reconstruction.py` after obtaining the complete reconstructed
receipt. It does not contact a registry or run image code. Supply independently
reviewed receipt/checkpoint hashes and original RPM/WPE inputs, not values copied
from the unreviewed reconstructed receipt.

```text
python os/dev/approve-native-reconstruction.py
  --source CLEAN_FROZEN_SOURCE_CHECKOUT
  --helper-repo TRUSTED_REPOSITORY
  --helper-revision REVIEWED_PRODUCER_HELPER_COMMIT
  --receipt RECONSTRUCTED_NATIVE_JSON --receipt-sha256 EXACT_RECEIPT_SHA256
  --checkpoint ORIGINAL_INITIAL_RECEIPT c81b8a9308ef7afb850271b499df49cb7afb162b2df86397e422450df6a084c1
  --checkpoint ORIGINAL_DELTA_RECEIPT c3bb9e84d824190aa7f4c237254c636b991adaf9b40f79ef89e21c1f2581ce44
  --rpms code.sandbox.example/qos/tilo-development-rpms-aarch64@sha256:912255637bab849ee5c3f62a963481f0b994576ef08cec8ff4fe4bc4cfb9e02f
  --wpe-reference code.sandbox.example/qos/tilo-development-wpe-adopted-aarch64@sha256:7a4ce1f1d8985977a18f3b360ff0ff2fda4f53770fee7f6389570c300a4be8ed
  --wpe-receipt-sha256 df41194aae2531d2cd9cd815cbce8009dd9dc254551b64154137228578ef32e2
  --run-id VERIFIED_INTERNAL_RUN_ID --attempt VERIFIED_ATTEMPT --output NEW_APPROVAL_JSON
```

Join these argument lines using the local shell's continuation syntax. Resolve
the producer helper commit from the reviewed dispatch inputs, and independently
confirm the internal run ID and attempt through Forgejo. The visible run number
is not the internal run ID. Retain these observations with the receipt and
approval; never take their expected values from the receipt being approved.
Use the helper commit that actually produced the completed artifact, not a newer
checkout or the separate finalization helper archive. Failed attempts do not
provide a completed artifact to approve.

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
# Cache-assisted producer admission

The existing assembly consumer can import expanded reconstruction provenance:
it checks the independently approved reconstruction hash and the complete
mounted payload inventory. Keep the frozen application source and its existing
consumer contract. The offline approval tool must reproduce the additional
provenance before supplying that hash.

For the cache-assisted producer, select its exact helper commit (currently
`2d09fac4a71547f0c0a9c30be757ffac8a642930`), all four original checkpoint
receipt/path hash pairs, and the actual successful run/attempt. In addition to
the existing arguments, supply:

```
--partial-admission PATH SHA256
--partial-metadata RECONSTRUCTED_RELEASE_RECEIPT_SHA256 RELEASE_METADATA_PATH
--partial-metadata RECONSTRUCTED_WINDOWS_RECEIPT_SHA256 WINDOWS_METADATA_PATH
--thin-cache-admission PATH SHA256
--thin-cache-receipt PATH SHA256
--source-metadata PATH SHA256
```

Each pair is independently pinned. Partial metadata bytes are checked against
the corresponding pinned receipt's metadata hash, then passed through the
pinned producer's depth-one derivation validator. Cache runtime evidence is
recreated from the admitted cache counts, source timestamp count, RPM hash,
reviewed restore-helper hash and approval identities. The exact runtime JSON
serialization is hashed independently. A self-consistent remote reconstruction
hash cannot approve changed runtime counts or unsupported freshness claims.

Run this offline tool with `python -X utf8` on Windows. Two temporary files
contain the exact Git source blobs for the unchanged producer validator; the
tool checks their decoded text before invoking it and removes them afterward.

The tool does not import an image or verify its ELF inventory. Successful offline
approval still requires the assembly consumer's registry and complete-inventory
verification. No future artifact is approved by preparing this invocation.
