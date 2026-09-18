# Reusing a native payload for development installation media

The desktop image already contains `/usr/bin/tilo-installer`. Build the live
installer layer from the verified machine-specific image instead of compiling
that application again:

```sh
TILO_IMAGE='REGISTRY/lenovo-development@sha256:VERIFIED_DIGEST' \
TILO_INSTALLER_IMAGE=localhost/tilo-installer:development-intermediate \
  ./os/dev/build-custom-installer.sh --reuse-image-installer --image-only
```

Run this on native ARM Linux using the same container store that holds the
verified base. Use helpers and `os/usr` from the base image's committed source;
changing the overlay source changes the resulting OS. The base must already
contain a UKI: the installer Containerfile modifies that UKI and derives the
installed-target UKI. A raw rootfs without `/boot/EFI/Linux/*.efi` is not an
input to this step.

`--image-only` creates an **intermediate OCI image**, not an installation-ready
artifact. Layering changes the composefs tree and UKI command lines and therefore
invalidates the base image's seal/signature. It intentionally skips disk creation,
the legacy disk-error-driven reseal loop, and media provenance generation.
No Cargo build occurs with `--reuse-image-installer`; a missing executable in the
base fails the Containerfile build. `--stub-ui` and `--no-cargo` are mutually
exclusive alternative binary sources.

## Complete the development medium

1. Verify the selected Lenovo base's immutable digest, native architecture,
   source revision, public trust, board profile, firmware and update origin.
   Retain its build receipt and the new intermediate image ID.
2. Build the layer above. It inherits the base's native applications, kernel,
   browser and compatibility payloads. Its rootfs includes the live installer
   plumbing and an installed greetd configuration. Inspect the resulting public
   trust and initramfs again before signing.
3. Run the development finalization pipeline on this **new** image: compute its
   composefs digest with the pinned bootc version, seal/sign every live UKI and
   `/boot/tilo-target/tilo-target.efi`, produce the target PCR prediction, then
   recompute the digest and require it to stay unchanged. Use the existing
   external signer and public bundle; private keys do not enter the image.
4. Publish/sign the final installer OCI image and independently verify its
   digest, hardware profile, trust, all boot artifacts and target prediction.
5. Use the supervised disposable-disk controller with that verified installer
   image, then verify the actual installed media bytes and boot it in a VM.
   Allocate the live root for both the installed composefs tree and the offline
   container copy: the existing builder uses three times the OCI image size with
   a 40 GiB floor. Disk capacity is logical; use a sparse artifact while testing.
6. Test the installer against a separate disposable target, including preservation
   of a synthetic existing Windows ESP/partition layout. Boot the installed
   target without the installation disk. Record media provenance only after
   final signing/disk conversion, and bind acceptance to those exact bytes.

This is an offline self-install design: `tilo-install` copies the **booted
installer image** with `bootc image copy-to-storage`. It does not download the
base desktop digest during installation. The board's immutable profile supplies
its update origin; that origin is distinct from the immediate installation
source. The target UKI excludes `tilo.install` and selects the normal session.

For source `4583a44c4056c67cfd1ebc4f1e02c7fa4aff3ffc`, native payload run 7489
already builds `tilo-installer` alongside the desktop binaries. These two build
flags can be applied to that source's helper tree without changing its Rust,
kernel, browser or firmware inputs. Keep the source-compatible development
signing helpers together; newer crypto helper APIs may require binaries absent
from that retained native payload.

No physical PC partitions, boot entries or removable media are changed by these
preparation steps. Physical media writing and the eventual Windows-preserving
installation require separate execution after preparation and backup.

Target bundle materialization deliberately refuses an existing `tilo-target` directory on the selected ESP. Reinstalling over a previous Tilo bundle requires a separately reviewed ownership/recovery operation; preparation never overwrites another bundle. Promotion selects the exact `bootc_composefs-<signed target digest>.efi` entry, preserving older entries on a shared ESP.
