# Enumerated artifact overlays keep the expensive rootfs stage shallow without
# bulk-copying os/usr (which would accidentally ship development/test-only
# files). Their internal COPY layers are cheap scratch-tree commits; each is
# applied to the multi-gigabyte bootc rootfs once, at the same logical boundary
# where its individual files were previously copied.
FROM scratch AS native-builder-payload
COPY --from=apps-payload /out/ /out/
COPY --from=apps-payload /usr/share/tilo-apps/ /usr/share/tilo-apps/
