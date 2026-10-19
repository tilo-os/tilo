# ---- Windows compatibility system extension ------------------------------
# Everything Windows apps need that is large: the FEX payload and the x86 guest
# root (ARM), Proton, the Steam runtime and the virtio-win media, about 3.1 GB.
# It is packed here as tilo-windows-compat.raw, a sysext overlaying those exact
# /usr paths, and is NOT part of the OS image. build-image.yml exports the
# `windows-compat-sysext` target, publishes the image next to the signed
# manifest tools/extensions/sysext_image.py writes, and a device installs it
# when its owner first enables Windows apps or creates a Windows VM
# (/usr/share/tilo/system-extensions/sysext.d/windows-compat.json).
FROM ${BASE_REF} AS windows-compat-sysext-build
COPY --from=windows-provider /payload/ /tree/
COPY --from=virtio-win-media /out/virtio-win.iso /tree/usr/share/virtio-win/virtio-win.iso
COPY --from=virtio-win-media /out/installer/virtio-win-guest-tools.exe /tree/usr/share/virtio-win/installer/virtio-win-guest-tools.exe
COPY --from=windows-compat-artifacts /out/proton/ /tree/usr/lib/tilo/windows-runners/proton/
COPY --from=windows-compat-artifacts /out/steam-runtime/ /tree/usr/lib/tilo/windows-runners/steam-runtime/
COPY --from=windows-provider /out/usr/share/tilo/windows/provider.json /provider.json
COPY os/dev/windows_provider.py os/dev/build-windows-compat-sysext.sh /tilo/
# The native FEX binaries are checked against the hashes the provider record
# pinned, and their dynamic-linker closure against the base plus this payload.
# /out is made first: the shell opens the descriptor before the packer runs.
RUN --network=none python3 /tilo/windows_provider.py host --manifest /provider.json --root /tree \
    && mkdir -p /out \
    && /tilo/build-windows-compat-sysext.sh /tree /out/tilo-windows-compat.raw > /out/descriptor.json

FROM scratch AS windows-compat-sysext
COPY --from=windows-compat-sysext-build /out/ /
