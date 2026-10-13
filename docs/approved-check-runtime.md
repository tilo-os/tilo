# Host-approved project checks

The project host registers fixed checks with `ush::approved_checks::ApprovedChecks`.
Each registration binds an ID to an exact workspace, host, canonical record and
resource, a source checkout, immutable tool images, fixed argv/environment and
execution limits. A caller selects the ID; it cannot supply a command or change
these limits. The registry itself grants no resource authority and schedules no
agents. Native action transport is a separate integration.

The host supplies an authority callback to `run`. It must validate the original
invocation custody before work and throughout execution. Failed validation
cancels the owned service. The implementation observes the aggregate cgroup's
CPU, memory, swap and process bounds before admitting the guest, then uses the
existing ush confinement with fully enforced Landlock. It observes retirement
of the owned service and its descendants before reporting cleanup.

Source, build and private scratch are separate canonical directories. The
source's empty `target` mount point receives the dedicated writable build
volume. Tool-image verification and read-only materialization remain the Images
provider's responsibility; a SHA-shaped identity alone does not verify an
image. Registration rejects duplicate IDs, mismatched bindings, source links,
unsafe scratch, overlapping directories, loader/environment injection and
invalid bounds.

A `CheckReceipt` records the exact project binding, configuration and runtime
hashes, observed cgroup limits, termination, bounded output hashes and cleanup.
An exited command is execution evidence. It supplies no coding score, test
oracle, model qualification or hardware result by itself. Full output belongs
in the private host receipt; an app's public result surface should disclose only
the bounded information its resource contract permits.

The developer example exercises the same supervision path:

```sh
cargo run --locked -p ush --example approved-check-qualification -- PRIVATE_CONFIG NEW_RECEIPT
```

The input is an owner-private JSON serialization of `ApprovedCheck`; the output
path must be new. An optional third argument requests cancellation after that
many milliseconds. The example returns success only for exit zero with observed
cleanup. Preserve failure receipts when testing timeout, cancellation, output
limits or failing commands. The internal `__approved-check-supervise` and
`__approved-check-init` entry points support the owned runtime; they are not an
app command gateway.

For informational phase qualification, configure the optional host-reviewed
`reported_phases` vocabulary and add `--reports` to the existing developer
example. It prints closed report events as they arrive through the production
owned guest stderr capture. This developer probe proves supervision and capture;
it does not prove signed application delivery or establish a hardware result.
The native action transport and reporting contract are described in
[approved project checks](architecture/project-approved-checks.md#reported-phases-on-the-original-job).
