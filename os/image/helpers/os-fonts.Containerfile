# ---- One copy of the font catalog -------------------------------------------
# The catalog's faces have three readers, each at its own path: the signed
# runtime bundle's lib/fonts (mounted into app sandboxes as /run/tilo-fonts),
# /usr/share/fonts/tilo (fontconfig, the image's own processes, sandboxes that
# bind /usr/share/fonts, Flatpak's /run/host/fonts) and
# /usr/share/tilo/font-catalogs/<manifest sha256> (the catalog a binary from a
# newer userspace looks up by its own manifest). Copied separately they put
# the same ~55 MB of fonts into three OCI layers. Here the two later
# names are hard links to the runtime's files in the one tree the rootfs
# copies in one COPY, so every pull carries the bytes once; symlinks would
# dangle under Flatpak and Landlock. The OS catalog's one face the product
# catalog leaves out (NewCM10-Regular, GPL) comes from os-fonts.
FROM ${BASE_REF} AS os-fonts
RUN --mount=type=cache,id=tilo-font-artifacts,target=/var/cache/tilo-font-artifacts \
    --mount=type=bind,source=tools/font-catalog.py,target=/tilo-src/tools/font-catalog.py,ro \
    --mount=type=bind,source=crates/sdk/ui-core/assets/fonts,target=/tilo-src/crates/sdk/ui-core/assets/fonts,ro \
    python3 /tilo-src/tools/font-catalog.py prepare \
        /tilo-src/crates/sdk/ui-core/assets/fonts /out/fonts --scope os \
        --cache /var/cache/tilo-font-artifacts
