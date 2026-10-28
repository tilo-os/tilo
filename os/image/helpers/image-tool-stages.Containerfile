# Nothing ships from this stage. It gathers the image's small tool stages, which
# change with neither the Rust build nor the rootfs, into one target that CI
# caches with its intermediate steps: the image build then neither repeats
# their dnf transactions nor pulls the RPM snapshot to run them.
FROM scratch AS image-tool-stages
COPY --from=uki-tools /usr/local/bin/uki.sh /uki-tools/
COPY --from=doubletake-builder /out/ /doubletake/
COPY --from=joycond-builder /out/ /joycond/
COPY --from=bootc-builder /out/ /bootc/
COPY --from=xwayland-satellite-builder /out/ /xwayland-satellite/
