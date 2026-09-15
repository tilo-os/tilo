# Hardware profiles

The default `generic` image uses its existing firmware and boot path.
`lenovo-83ed` prepares a Lenovo Yoga Slim 7x ARM64 image using local OEM
firmware. It requires a native aarch64 builder, the matching kernel's
`CONFIG_ARM64=y` configuration, and its
`dtb/qcom/x1e80100-lenovo-yoga-slim7x.dtb`.

## Local firmware input

Place these six files and `manifest.json` in
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
  kernel-specific DTB path, and that DTB's SHA-256. The UKI builder validates
  this immutable image input before embedding the device tree and signing.

Passing `--profile generic` creates no files and needs no local firmware.
These checks prepare build inputs; a signed image, VM installer checks, and a
live boot on the laptop are still needed to establish bootability. Other
firmware dependencies, including Wi-Fi and Bluetooth, must be checked against
the chosen kernel and image packages.
