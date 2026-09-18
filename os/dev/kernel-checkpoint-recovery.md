# Contingency: recover the interrupted ARM kernel

This manual workflow is specific to source `33e83f64cde9df4ee65ff47a708f125d932256da`.
It is a contingency after the original producer has stopped, not authorization
to duplicate a live build. No checkpoint is admitted by this document.

Before dispatch, independently review the capture, writer mapping, dependency
invalidations, excluded selftests, exact toolchain inventory and restore helper.
An approval JSON must contain the fields required by `kernel_checkpoint_restore.py`
plus `checkpoint_reference`, binding the immutable partial checkpoint image.
Supply its exact bytes as `checkpoint_approval` and its independently calculated
SHA256 as `approval_sha256`. No approval template with true assertions is supplied.
Pin `helper_revision` to the reviewed committed helper. Reuse the original immutable
base/RPM inputs. Never use a mutable checkpoint tag.

The publisher layout is `/checkpoint/{tree.tar,scan.json,capture-receipt.json,
toolchain-receipt.json,metadata.json}`. Staging renames the public toolchain receipt
to `toolchain.json`, verifies every consumed file hash, and mounts only the staged
context read-only into the original recipe at its sole `make all` seam.
The build uses four jobs and checks the exact installed toolchain before restoring.
It regenerates the embedded module certificate with a new ephemeral identity.
The original packaging, module signing/verification and key deletion tail is unchanged.
An appended `reconstructed-publish` stage copies only public runtime evidence
and the invalidation manifest alongside the original `/repo`. Runtime evidence
records the actual certificate DER SHA256 and successful regeneration and embedding
checks. The workflow validates and binds this evidence to the output receipt.

Output is in `tilo-development-reconstructed-kernel-aarch64`, with receipt purpose
`tilo-development-reconstructed-kernel`. Its `reconstruction` object identifies
the original and derived recipes, checkpoint digest, approval hash and helper.
It also binds archive, scan, capture and toolchain receipt hashes, and explicitly
states `original_recipe_executed_verbatim=false` and `installable=false`.
The successful build plus unchanged RPM/config audit are required; preparation
metadata alone does not establish successful restoration, boot or installation.

## Assembly handoff

`compile-development-os.py` already accepts an immutable `kernel_candidate` and
passes it as `KERNEL_RPMS_REF`; it does not enforce the old receipt purpose or old
repository name. Thus no producer-format compatibility lie is necessary and no
assembly code extension is currently required. The operator admission must instead
verify the new receipt purpose, actual recovery run/attempt, checkpoint and approval,
original/derived recipe hashes, package/config audit and immutable registry config
readback before putting that reference into the assembly plan. Preserve this full
reconstruction receipt with the assembly evidence.

The old local `verify-kernel7864-evidence.py` is pinned to the original producer and
must not be reused for this output. A recovery-specific independent admission
check is still required before assembly; this contingency workflow does not itself
claim that independent admission or actual boot acceptance.
