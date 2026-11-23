# ---- Tilo build outputs, arranged by os/components.toml ---------------------
# The manifest is the one record of which Tilo-built file lands where. This
# stage applies it: every file of every image layer is installed under
# /image/<layer>/, and each overlay below copies its layer as one tree. Dev
# sysext layers are assembled by the same installer from the same entries, so
# the image and a dev layer cannot place a binary differently. Only the compiled
# tree (os/image/compile.sh's OUT, the `compiled` build context) is read, not the
# apps' packing: the programs are installed without waiting for it. The manifest's
# `direct` files (baked runtime, app store) are copied by the rootfs stage from
# the payload, since the packing writes them at their final paths.
# tools/policy/check_component_manifest.py fails when a build output is copied
# into the image outside this path. A layer build (os/image/build-layer.sh) runs the
# same installer on the step's own disk and gives its tree as this stage instead.
FROM ${BASE_REF} AS component-files
ARG SOURCE_DATE_EPOCH=0
COPY os/components.toml os/scripts/install-components.py /tilo-components/
COPY os/usr/libexec/tilo-session os/usr/libexec/tilo-image-trust-staged \
    os/usr/libexec/tilo-verified-update os/usr/libexec/tilo-update-channel \
    /tilo-components/usr/libexec/
COPY os/external-assets/github-assets.tsv /tilo-components/external-assets/
COPY os/usr/lib/systemd/user/tilo-ush.service \
    os/usr/lib/systemd/user/tilo-app-update.service \
    os/usr/lib/systemd/user/tilo-app-update.timer \
    /tilo-components/usr/lib/systemd/user/
COPY os/usr/share/licenses/tilo-speech-worker/LICENSE /tilo-components/usr/share/licenses/tilo-speech-worker/
# The interface declaration the session judges apps against travels with it.
COPY os/usr/share/tilo/system-interfaces.toml /tilo-components/usr/share/tilo/system-interfaces.toml
# Server component metadata travels with the same component in images and updates.
COPY os/usr/lib/tilo/node-server.toml /tilo-components/usr/lib/tilo/node-server.toml
COPY os/usr/lib/sysusers.d/tilo-server.conf /tilo-components/usr/lib/sysusers.d/tilo-server.conf
COPY os/usr/lib/systemd/system/tilo-node-server@.service \
    os/usr/lib/systemd/system/tilo-node-session-key@.service \
    os/usr/lib/systemd/system/tilo-node-session@.service \
    os/usr/lib/systemd/system/tilo-node-session@.path \
    /tilo-components/usr/lib/systemd/system/
# Buildah keys a RUN by its instruction and the layer below, not by what a bind mount from
# another stage holds, so this RUN would be served from an earlier build's cache however the
# compiled components changed. Copying the digest of every file of the tree first keys it.
COPY --from=compiled /sha256sums /tilo-payload-keys/compiled.sha256sums
RUN --network=none \
    --mount=type=bind,from=compiled,source=/,target=/out,ro \
    --mount=type=bind,from=external-assets,source=/,target=/tilo-assets,ro \
    TILO_GITHUB_ASSET_CACHE=/tilo-assets python3 /tilo-components/install-components.py \
      --manifest /tilo-components/components.toml \
      --from /out \
      --dest /image --by-layer --all
