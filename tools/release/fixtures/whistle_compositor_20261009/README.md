# Historical Whistle compositor qualification snapshots

These are exact drivers for the hardware-disabled public-audio runs recorded in
`docs/architecture/dictation-whistle-compositor-evidence.json`. They are evidence,
not an installer or a command to activate a model/runtime on a real OS. Existing
result names intentionally fail. Reuse requires current binary/source receipts,
fresh output names and independent verification of every scope control.

French uses `inner-french-r16.py`; German, English and Spanish use `inner-r17.py`.
The latter retains the identical accuracy assertions after the editor-behavior
journey. German still exits unsuccessfully. Both scripts observe actual worker
command lines before requiring exit; unreadable command lines are errors. The
older Whisper Base executable-link helper could skip unreadable processes and
must not be used as strict worker-exit evidence.

The outer driver projects only pinned Whistle/AOT artifacts into a disposable
read-only image. A namespace keeper represents root-owned image files and UID
65534 separately. Its fixed launch helper retains only existing namespace
SETUID/SETGID authority until it locks no-new-privileges and drops all application
identity privileges before any application, worker or audio starts. The fixture
asserts zero application effective capabilities and preserves the production
root-ownership, exact-digest, Landlock and seccomp checks. All keeper/model owner
processes are reaped and temporary model/trust stores removed on completion.
The original failed setup traces remain in the receipt; no original artifact
permissions or production trust catalogue changed.

The rootless outer container has no network or host devices. PipeWire hardware
and Bluetooth discovery are disabled; public audio plays into an isolated virtual
source. Each run verifies two CPU affinity slots, two CPUs of quota, 3 GiB RAM,
zero swap, 256 tasks and a 240-second timeout. Preview timing measures the first
laid-out nonempty preview rectangle, not its words or correctness. Aggregate
memory includes debug/runtime staging, compositor, browser and page cache.

Public recordings are from Google FLEURS contributors, CC-BY-4.0, pinned at
`70bb2e84b976b7e960aa89f1c648e09c59f894dd`:
https://huggingface.co/datasets/google/fleurs/blob/70bb2e84b976b7e960aa89f1c648e09c59f894dd/README.md
These development fixtures were already consumed during prior qualification;
they are not a frozen holdout or a representative accuracy benchmark.
Whistle weights and runtime source/artifact identities and licences are recorded
in the evidence and the existing AOT review. No binary or model is distributed
by these snapshots.
