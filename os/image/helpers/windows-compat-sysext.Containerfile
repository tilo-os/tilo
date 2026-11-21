# ---- Windows compatibility system extension ------------------------------
# Everything Windows apps need that is large: the FEX payload and the x86 guest
# root (ARM), Proton (x86) and the virtio-win media. It is packed here as
# tilo-windows-compat.raw, a sysext overlaying those exact /usr paths, and is
# NOT part of the OS image. The os-windows-compat-<arch> checks build the
# `windows-compat-sysext` target (os/image/layers.toml image `windows-compat`),
# tools/ci/publish/windows-compat-sysext.sh publishes it beside main's dev
# images, extension-manifests pins it in the signed manifest
# tools/extensions/sysext_image.py writes, and a device installs it when its
# owner first enables Windows apps or creates a Windows VM
# (/usr/share/tilo/system-extensions/sysext.d/windows-compat.json).
FROM ${BASE_REF} AS windows-compat-sysext-build
COPY --from=windows-provider /payload/ /tree/
COPY --from=virtio-win-media /out/virtio-win.iso /tree/usr/share/virtio-win/virtio-win.iso
COPY --from=virtio-win-media /out/installer/virtio-win-guest-tools.exe /tree/usr/share/virtio-win/installer/virtio-win-guest-tools.exe
COPY --from=windows-provider /out/usr/share/tilo/windows/provider.json /provider.json
COPY os/dev/windows_provider.py os/dev/proton_libraries.py os/dev/build-windows-compat-sysext.sh os/dev/sysext-image.py /tilo/
# The native FEX binaries are checked against the hashes the provider record
# pinned, and their dynamic-linker closure against the base plus this payload.
# On x86 a capsule starts Proton on the system's libraries, so every library it
# needs or loads by name must be in the image's packages (the `packages` layer,
# mounted at /os: the base alone has no desktop libraries) or this payload. An
# ARM computer plans no Proton route, so its extension carries none and has no
# libraries to check. /out is made before the packer: the shell opens the
# descriptor it prints.
RUN --network=none --mount=type=bind,from=packages,source=/,target=/os,ro \
    --mount=type=bind,from=windows-compat-artifacts,source=/out,target=/artifacts,ro \
    mkdir -p /tree/usr/lib/tilo/windows-runners \
    && if [ "$(uname -m)" = x86_64 ]; then \
         mkdir -p /tree/usr/lib/tilo/windows-runners/proton \
         && cp -a /artifacts/proton/. /tree/usr/lib/tilo/windows-runners/proton/; \
       else \
         test "$(uname -m)" = aarch64; \
       fi \
    && python3 /tilo/windows_provider.py host --manifest /provider.json --root /tree \
    && if [ "$(uname -m)" = x86_64 ]; then \
         python3 /tilo/proton_libraries.py /tree/usr/lib/tilo/windows-runners/proton/UMU-Proton-10.0-4 --tree /tree --base /os; \
       fi \
    && mkdir -p /out \
    && /tilo/build-windows-compat-sysext.sh /tree /out/tilo-windows-compat.raw > /out/descriptor.json

FROM scratch AS windows-compat-sysext
COPY --from=windows-compat-sysext-build /out/ /
