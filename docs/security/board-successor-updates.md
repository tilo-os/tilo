# Verified board successor updates

The ordinary `tilo system update` / Settings update action invokes the existing
`tilo-deployment-update.service`. The service retains one exclusive update lock
through discovery, download, owner verification, rollback-floor publication and
activation. No additional control socket or application permission is involved.

For an official Lenovo 83ED image, the image-owned hardware definition and update
origin select the board namespace in `code.tilo.so/tilo/tilo`. The mutable
`lenovo-83ed-latest` tag is only a discovery hint. Its exact OCI digest must have
the existing pinned-key Cosign signature, SLSA provenance binding the board,
architecture, source, firmware, definition hash and generation, and the existing
short-lived release-freshness attestation. The signed `lenovo-83ed-genN` and
immutable source tag must resolve to that same digest. A lower generation or an
advance exceeding the rollback counter's 64-step bound refuses. A same-generation
different digest also refuses. Generic and development origins retain their
existing update path.

The native Cosign verifier is the same architecture-specific, checksum-pinned
external input used by the offline installer. The verifier uses bootc's existing
root-owned pull identity in `/run/ostree/auth.json`, `/etc/ostree/auth.json`, then
`/usr/lib/ostree/auth.json`; private snapshots are removed after verification.
Pull credentials provide transport access and never replace the release key.

`bootc switch --download-only` receives only the verified immutable digest.
Before activation the staged gate revalidates the actual staged image, matching
UKI composefs digest and unique sealed generation, authenticated NTS time, and
signed freshness expiry. If owner trust is armed, `renew-under-update-lock`
validates the existing endorsement and current revocations, then endorses only
an increasing generation (or the identical already-endorsed digest/generation
when retrying a failed activation) using the same device certificate and still-valid
human-root image-signing grant. It never extends the grant, creates a root grant,
changes the trusted root, or omits revocations. An expired grant requires explicit
owner renewal and fails with an actionable error. The inherited descriptor must
be the already locked private transaction inode; the child closes its duplicate
without unlocking the parent's transaction. Atomic endorsement replacement
precedes the existing `verify-and-record` floor checks and exact downloaded
activation.

Publishing a new tag does **not** update the already installed old helper, which
still upgrades its fixed immutable origin. Delivery of this consumer requires a
canonical `tilo-vm swap user-identity --slot vm --builder remote` prerequisite,
using the source-covered component installer and normal owner/CAS layer
admission. This explicit component carries the three helper scripts, native
image-trust verifier, hardware-profile reader and checksum-pinned native Cosign.
It is not silently added to the nine-member app cohort. Existing service paths
remain unchanged; the bootstrap does not reboot, though the installed daemon
may refresh the owner desktop under its existing component policy. Actual
component delivery and subsequent successor staging remain unproven here;
source tests and new publication are not
proof that an old on-device update button has advanced. Retained userspace cohort
retirement and rollback remain separate deployment responsibilities and cannot
be inferred from a selected digest alone.
