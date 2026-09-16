# Hardware profiles

Architecture and hardware model are separate build inputs. The default
`generic` image uses the platform firmware's boot description on either
x86-64 or ARM64. Registered profiles add model-specific firmware and an
optional device tree without changing the common build scripts.

## Defining a hardware profile

Add `os/hardware/profiles/<id>.json`, following the validated schema:

```json
{
  "schema_version": 1,
  "id": "example-board",
  "architecture": "aarch64",
  "devicetree": "vendor/example-board.dtb",
  "firmware": {"radio.bin": "vendor/example-board/radio.bin"}
}
```

`architecture` is `aarch64` or `x86_64` and must match the native builder and
installed kernel. `devicetree` names a file relative to that kernel's `dtb/`
directory; it is `null` for ACPI or firmware-provided device descriptions.
Only ARM64 profiles may embed a DTB. `firmware` maps local input basenames
to unique paths under `/usr/lib/firmware/`; an empty mapping needs no local
firmware manifest. Paths cannot be absolute, traverse directories, or contain
shell metacharacters. Unknown fields and unsupported architectures fail.

The reviewed definition is copied into the immutable rootfs beside the
generated profile. The UKI builder checks both files' ownership and permissions,
their matching identity and architecture, and the exact kernel-specific DTB
path and hash. Adding a board does not add a new branch to the UKI builder.

The first registered profile, `lenovo-83ed`, describes the Lenovo Yoga Slim 7x.
Its kernel must supply `dtb/qcom/x1e80100-lenovo-yoga-slim7x.dtb`.

## Local firmware input

For the Lenovo profile, place these six files and `manifest.json` in
`os/hardware/firmware/lenovo-83ed/`. This directory is a local build input;
firmware blobs must not be committed. Preserve the matching Windows driver
package pair when selecting `qccdsp8380.mbn` and `cdsp_dtbs.elf`.

- `qcdxkmsuc8380.mbn`
- `qcvss8380.mbn`
- `qcadsp8380.mbn`
- `adsp_dtbs.elf`
- `qccdsp8380.mbn`
- `cdsp_dtbs.elf`

The manifest has exactly three fields: `schema_version` (integer `1`),
`profile` (`"lenovo-83ed"`), and `files` (an object mapping each of the six
basenames to its lowercase SHA-256). Hashes bind the selected local copies;
they do not prove compatibility or establish a publisher's signature.

## Image build integration

Select the profile when building on native aarch64 Linux, after configuring
the normal pinned build inputs, registry access, generation and owner key:

Set `QOS_TARGET_IMGREF` explicitly to the reviewed update tag or SHA-256 digest
in the signature-required `code.sandbox.example/qos/qos` repository. It must identify
a published channel or artifact that retains this board's firmware and device
tree; this guide does not designate a release that has not been published.
Non-generic builds reject a missing reference before building and bind it into
the immutable hardware metadata. The installer uses that reference, permits an
identical override, and rejects a conflicting override before any disk work.
Generic images retain the official `latest` default and trusted overrides.

```sh
QOS_HARDWARE_PROFILE=lenovo-83ed ./os/build.sh
```

The image builder runs `os/dev/prepare-hardware-profile.py` as root after
installing the selected kernel and before dracut builds the initramfs. It
reads the profile's local firmware directory through a build mount. An
explicit profile cannot be combined with `QOS_SKIP_BUILD=1`; the profile
must be included in a newly built image. Set `KERNEL_RPMS_REF` explicitly
when using a separately built native ARM64 kernel artifact.

The preparer validates every input hash and all paths before writing. It
rejects symlink redirects, a missing or malformed DTB, architecture mismatch,
and incomplete manifests. It writes root-owned files with mode `0644`:

- `/usr/lib/firmware/qcom/x1e80100/LENOVO/83ED/` contains the six firmware files.
- `/etc/dracut.conf.d/41-qos-hardware.conf` includes those exact firmware paths
  in the initramfs, where display and input can need them during disk unlock.
- `/usr/lib/qos/hardware-profile.json` binds the model, aarch64 architecture,
  kernel-specific DTB path, that DTB's SHA-256, and `target_update_ref`. The UKI
  builder validates this immutable image input before embedding the device
  tree and signing.
- `/usr/lib/qos/hardware-profile-definition.json` holds the reviewed definition
  used to validate that generated profile.

Passing `--profile generic` creates no files and needs no local firmware.
These checks prepare build inputs; a signed image, VM installer checks, and a
live boot on the laptop are still needed to establish bootability. Other
firmware dependencies, including Wi-Fi and Bluetooth, must be checked against
the chosen kernel and image packages.

The update-reference binding covers image construction and initial installation.
It does not prove that a later image published at that reference preserves the
profile. A check of staged updates against the installed hardware profile and
physical update/rollback acceptance remain separate work.
