# CI Lenovo offline installer medium

The normal Lenovo OS is built and signed by the `Release / OS` workflow. The
`Release / imager` workflow can wrap **that exact signed ARM64 child manifest**
in a bootable installer medium for native ARM64 CI and Windows ARM64 WHPX tests.
The Windows laptop does not build or upload an OS image.

Dispatch `release-imager.yml` on `main` with:

```text
installer_only=true
lenovo_target_image=code.sandbox.example/qos/tilo@sha256:<CI ARM64 child digest>
```

The job verifies the Cosign signature and SLSA statement against
`keys/cosign.pub`, checks the board profile and source, and uses `cosign save`
to place the target OCI manifest, blobs, signatures, and attestation in one
offline layout. The live installer overlay is a separate CI-built image. While
constructing its disk, `disk-image.sh` stages that layout and a pinned
`target.json` in the medium's writable `/var/lib/tilo-installer/target-oci`.
This does not alter the signed target OCI or its immutable manifest digest.

The release job boots and installs the medium on a native ARM64 KVM runner.
Only after those cycles pass does it upload
`tilo-lenovo-83ed-whpx-installer-<source commit>` with a compressed qcow2 and
SHA-256 files. On Windows, verify the archive SHA-256, decompress once, verify
the qcow2 SHA-256, then pass the qcow2 to `tilo-vm scenario install --medium`.
The VM must check that the installed target digest equals the signed child;
the booted live medium has a different digest by design.

The qcow2 artifact is a **test installer medium**, not the signed production
OS image. Physical Windows-direct installation consumes the signed normal
Lenovo child, its raw manifest and guest install proof through the reviewed
source/finalizer receipt path. A medium artifact or `target.json` alone is
never accepted as production image authority.
