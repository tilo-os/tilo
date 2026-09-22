# Native x86-64 verifier bundle acceptance

[Producer run 9525](https://code.sandbox.example/qos/qos/actions/runs/9525) (API 23012),
x86 job 84455/task 75605 attempt 1, succeeded at source
`a987e76bac313e27f11c6f67d4c10779b2c3909f`. Its committed workflow builds only
`tilo-app-package-cli` and `runtime-bundle`, checks both CLIs load with `--help`, and compares
two produced archives/descriptors. Builder job 84453 resolved the immutable image
recorded in `builder-observation.json`; the bundle descriptor matches it exactly.

Artifact 38736 was retrieved using the existing authenticated fixed-origin API
with redirects denied and a 16 MiB response bound. Credentials stayed in memory.
The ZIP contains exactly two regular entries: `x86_64.tar.gz` (6,113,401 bytes)
and `x86_64.tar.descriptor.json` (772 bytes). Their 6,114,173-byte sum matches API
artifact accounting; the downloaded ZIP itself is 6,110,774 bytes. These are
different measurements, not a size mismatch. Workflow retention expires at
2026-10-03 01:55:27 UTC; this artifact has not been published to the package registry.

Independent verification used the existing bounded bundle consumer to check the
archive hash/size, exact tar inventory, both ELF64 little-endian x86-64 headers,
both executable hashes and manifest source/builder identities. The frozen adapter
matches its committed Git source and SHA256. The accepted archive digest is
`10ad658a452b21545cf2521fa875536da5d951ee8dd509533251a0a3a0517bde`.
`verification.json` retains the full descriptor and identities.

Independent Windows repackaging of the actual verified binaries produces the exact
same uncompressed tar stream and repeats byte-identically locally. Its gzip bytes
differ from the native producer (compression implementations can differ); preserve
the original downloaded archive and digest. This does not claim cross-toolchain
compiler reproducibility or interchangeable compressed archive identities.

The independent consumer/materialization pass did **not execute** either binary.
Native startup evidence comes from the successful x86 producer, not the local
host. Representative signed package/runtime replay remains a separate semantic
qualification gate. ARM job 84454 was waiting in this captured snapshot; neither
ARM acceptance nor a green whole run is claimed. No redispatch, registry upload,
trust-policy change or channel activation occurred.

`verify.py` reproduces the offline checks from captured API metadata and the saved
ZIP. Use a checkout containing the stated source commit, the existing consumer,
and a fresh output directory:

```sh
python3 -I experiments/ci-forgejo/results/native-verifier-23012/verify.py \
  /trusted/download-directory /fresh/verification-output
```

The downloaded ZIP and extracted bundle are retained locally under
`target/native-verifier-23012/`; `qualified/x86_64.tar.gz` is the original accepted
bundle and `qualified/materialized/` contains the checked executables and adapter.
Large executable artifacts are not committed to Git. The retained JSON observations
are historical evidence, not current API state or centrally approved trust policy.

## Registry publication follow-up

After the offline qualification above, the platform uploaded the original accepted
x86 archive to its exact digest URL and downloaded it again on 2026-09-19.
The readback matched all 6,113,401 bytes and SHA-256; `registry-readback.json`
records that later operation. Existing content was checked before upload, and no
mutable alias was created. The dedicated service identity still needs its own
read-permission test. Native signed-fixture replay, ARM qualification, central
policy approval and application publication remain separate pending gates.
