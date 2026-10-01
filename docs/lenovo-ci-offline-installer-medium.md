# CI signed-target offline installer media

The normal Lenovo OS is built and signed by the `Release / OS` workflow. The
`Release / imager` workflow can wrap **that exact signed ARM64 child manifest**
in a bootable installer medium for native ARM64 CI and Windows ARM64 WHPX tests.
The Windows laptop does not build or upload an OS image.

Dispatch `release-imager.yml` on `main` to build both proof media in one CI run:

```text
installer_only=true
installer_rootfs=ext4
generic_target=true
lenovo_target_image=code.tilo.so/tilo/tilo@sha256:<CI ARM64 child digest>
published_os_image=registry.tilo.so/tilo/tilo@sha256:<signed generic multiarch index>
published_os_source_revision=<full 40-character source commit SHA shared by both images>
```

`installer_rootfs=ext4` selects a full-size Ext4 live medium for WHPX. Use it
only with a signed OS image whose initramfs includes the Ext4 driver; the
workflow defaults to Btrfs when the input is omitted. The signed target image
and its digest are unchanged by this live-medium choice.

The generic medium gives a full VM install and first-boot proof through the
same offline backend. The workflow resolves the index's unique ARM64
child and independently verifies that child's signature and provenance. The
two media embed different target bytes; compare their signed `live_base_image`
and `build_revision`, `run_id`, and `run_attempt` to establish the common CI live
base and installer recipe. Either target input may be omitted when only one
variant is needed.

The signed generic image boots QEMU `virt` on ARM64 and supplies the live
installer. The separate Lenovo image carries a physical Lenovo DTB, so the
test medium must not boot from it. The job verifies both image sources against
`keys/cosign.pub`, checks the target board profile and source, and uses `cosign save`
to place the target OCI manifest, blobs, signatures, and attestation in one
offline layout. The live installer overlay is a separate CI-built image. While
constructing its disk, `disk-image.sh` stages that layout and a pinned
`target.json` in the medium's writable `/var/lib/tilo-installer/target-oci`.
This does not alter the signed target OCI or its immutable manifest digest.

The release job boots and installs the medium on a native ARM64 KVM runner.
The VM verifies the installed target OCI digest and disk/UKI contents; the
Lenovo target's first boot requires the physical Lenovo firmware and DTB and
cannot be represented by QEMU `virt`.
Only after the respective cycle passes does it upload
`tilo-<profile>-whpx-installer-<source commit>` with a compressed qcow2 and
SHA-256 files. These offline jobs skip the separate raw imager bundle and its
large qcow2-to-raw conversion. On Windows, verify the archive SHA-256,
decompress once, verify
the qcow2 SHA-256, then pass the qcow2 to `tilo-vm scenario install --medium`.
Before using it as an install proof source, verify the accompanying
`*.whpx-source.json` with `cosign verify-blob --offline --key keys/cosign.pub
--bundle <json>.bundle --signature <json>.sig <json>`. This CI KMS signature
binds the compressed and decompressed medium hashes, both signed image refs,
the target profile, CI source revision/run, and native ARM guest install proof
hash. The identity schema is `tilo.whpx-ci-installer-medium.v1`. Each variant
has a separate signed identity, and its `qcow2_sha256` must equal the hash of
the exact medium used in the WHPX test.
The VM must check that the installed target digest equals the signed child;
the booted live medium has a different digest by design.

The qcow2 artifact is a **test installer medium**, not the signed production
OS image. Physical Windows-direct installation consumes the signed normal
Lenovo child, its raw manifest and guest install proof through the reviewed
source/finalizer receipt path. A medium artifact or `target.json` alone is
never accepted as production image authority.
