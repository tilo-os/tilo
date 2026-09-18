# Reuse the complete native application payload

In `development-compile.yml`, select `native-payload` to compile the native
applications independently of the Windows guest, kernel and hardware profile.
Use `hardware_profile=generic` and the same pinned base/RPM, browser and native
tooling inputs intended for the full image. The browser stays enabled.

The producer retains all `/out`, runtime-bundle and installed application-store
outputs together. This includes their publisher keys and signed packages. A
desktop-only executable collection would omit other outputs and cause the
rootfs build to compile them again.

The CI artifact's `native-builder.json` identifies the immutable payload. For
subsequent `full-build` runs, supply its exact JSON as `native_builder_receipt`
and its file SHA256 as `native_builder_sha256`. The importer verifies the
native image identity, complete payload inventory and exact build inputs, then
overrides the Containerfile's `builder` stage with that immutable local image.
Invalid receipts fail; they never trigger a silent replacement compilation.

The generic ARM VM and Lenovo image can therefore use one native payload when
built from the same committed revision and inputs. Hardware profile, firmware,
kernel, generation and development boot-signing identities are assembly inputs
and do not change this payload. This initial contract requires the exact source
revision and epoch; it does not infer compatibility across later commits.

Payload compilation does not assemble a rootfs, UKI or disk and is not boot
acceptance. Full assembly still requires the proper Windows guest, kernel and
hardware inputs, followed by final signing, sealing and disk/boot verification.
