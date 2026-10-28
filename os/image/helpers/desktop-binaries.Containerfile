# The side builds come through image-tool-stages, so a build of the binaries
# given that stage as an image (t's, os/image/layers.toml) doesn't build them again.
FROM scratch AS desktop-binaries
COPY --from=component-files /image/desktop-binaries/ /
COPY --from=image-tool-stages /doubletake/doubletake /usr/libexec/tilo-doubletake
COPY --from=image-tool-stages /doubletake/doubletake-test-receiver /usr/libexec/tilo-doubletake-test-receiver
COPY --from=image-tool-stages /joycond/usr/bin/joycond /usr/bin/joycond
COPY --from=image-tool-stages /joycond/usr/lib/systemd/system/joycond.service /usr/lib/systemd/system/joycond.service
COPY --from=image-tool-stages /joycond/usr/lib/modules-load.d/joycond.conf /usr/lib/modules-load.d/joycond.conf
# bootc that finds this system's ESP on a disk it shares (bootc-builder). It
# replaces the base's binary of the same release; the rootfs's final checks run
# it against the base's libostree.
COPY --from=image-tool-stages /bootc/usr/bin/bootc /usr/bin/bootc
COPY --from=image-tool-stages /bootc/usr/share/licenses/tilo-bootc/ /usr/share/licenses/tilo-bootc/
# The X11 bridge with the in-flight clipboard handoff fix
# (xwayland-satellite-builder); its runtime packages are in the rootfs.
COPY --from=image-tool-stages /xwayland-satellite/usr/bin/xwayland-satellite /usr/bin/xwayland-satellite
COPY --from=image-tool-stages /xwayland-satellite/usr/share/licenses/tilo-xwayland-satellite/ /usr/share/licenses/tilo-xwayland-satellite/
