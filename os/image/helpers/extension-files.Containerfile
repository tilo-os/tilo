# Downloaded Tilo extensions: a helper that, only when the owner asks, downloads
# the artifacts a Tilo-signed manifest pins (for the media codecs: RPM Fusion's
# and Cisco's packages, from their own servers), verifies them and builds local
# system extensions. Which extensions exist, which hosts their bytes may come
# from and which key signs their manifests is decided by the image itself
# (/usr/share/tilo/system-extensions/sysext.d, /usr/share/tilo/extensions.pub). The
# image carries no extension payload. See
# /usr/share/tilo/system-extensions/README.md.
# The files of this tree that are not compiled: scripts, units, keys. Its
# compiled files are a layer of their own (binaries.Containerfile), so a changed
# binary doesn't change the system layer these go into.
FROM scratch AS extension-files
# The only key a manifest may be signed with (KMS alias/tilo-extension-manifests);
# separate from cosign.pub, so the OS update key cannot authorize one.
COPY keys/extensions.pub /usr/share/tilo/extensions.pub
# Starts the deferred install the moment NetworkManager reports internet
# connectivity, for an owner who opted in while offline.
COPY --chmod=0755 os/usr/lib/NetworkManager/dispatcher.d/90-tilo-extension \
    /usr/lib/NetworkManager/dispatcher.d/90-tilo-extension
COPY \
    os/usr/lib/systemd/system/tilo-extension@.service \
    os/usr/lib/systemd/system/tilo-extension-activate.service \
    os/usr/lib/systemd/system/tilo-extension-refresh.timer \
    /usr/lib/systemd/system/
COPY os/usr/lib/systemd/system/bootc-fetch-apply-updates.service.d/40-tilo-extension.conf \
    /usr/lib/systemd/system/bootc-fetch-apply-updates.service.d/40-tilo-extension.conf
COPY os/usr/lib/tmpfiles.d/tilo-extension.conf /usr/lib/tmpfiles.d/tilo-extension.conf
COPY os/etc/polkit-1/rules.d/49-tilo-extension.rules /etc/polkit-1/rules.d/49-tilo-extension.rules
COPY os/usr/share/doc/tilo/codec-pack.md /usr/share/doc/tilo/codec-pack.md
# License information only; Cisco binaries remain separate end-user downloads.
COPY os/usr/share/licenses/tilo-openh264/BINARY_LICENSE.txt /usr/share/licenses/tilo-openh264/BINARY_LICENSE.txt
