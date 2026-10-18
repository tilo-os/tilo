# ---- Tilo build outputs, arranged by os/components.toml ---------------------
# The manifest is the one record of which Tilo-built file lands where. This
# stage applies it: every file of every image layer is installed under
# /image/<layer>/, and each overlay below copies its layer as one tree. Dev
# sysext layers are assembled by the same installer from the same entries, so
# the image and a dev layer cannot place a binary differently. Only /out is
# read, which a restored native builder payload provides as well. The manifest's
# `direct` files (baked runtime, app store) are copied by the rootfs stage from
# the builder itself, since the build writes them at their final paths.
# tools/policy/check_component_manifest.py fails when a build output is copied
# into the image outside this path.
FROM ${BASE_REF} AS component-files
ARG SOURCE_DATE_EPOCH=0
COPY os/components.toml os/scripts/install-components.py /tilo-components/
COPY os/usr/libexec/tilo-session os/usr/libexec/tilo-image-trust-staged \
    os/usr/libexec/tilo-verified-update \
    /tilo-components/usr/libexec/
COPY os/external-assets/github-assets.tsv /tilo-components/external-assets/
COPY os/usr/lib/systemd/user/tilo-ush.service \
    os/usr/lib/systemd/user/tilo-app-host.service \
    /tilo-components/usr/lib/systemd/user/
COPY os/usr/share/licenses/tilo-speech-worker/LICENSE /tilo-components/usr/share/licenses/tilo-speech-worker/
# Relay component metadata travels with the same component in images and updates.
COPY os/usr/lib/tilo/node-relay.toml /tilo-components/usr/lib/tilo/node-relay.toml
COPY os/usr/lib/sysusers.d/tilo-relay.conf /tilo-components/usr/lib/sysusers.d/tilo-relay.conf
COPY os/usr/lib/systemd/system/tilo-node-relay@.service \
    os/usr/lib/systemd/system/tilo-node-session-key@.service \
    os/usr/lib/systemd/system/tilo-node-session@.service \
    os/usr/lib/systemd/system/tilo-node-session@.path \
    /tilo-components/usr/lib/systemd/system/
RUN --network=none \
    --mount=type=bind,from=native-builder-payload,source=/out,target=/out,ro \
    --mount=type=bind,from=external-assets,source=/,target=/tilo-assets,ro \
    TILO_GITHUB_ASSET_CACHE=/tilo-assets python3 /tilo-components/install-components.py \
      --manifest /tilo-components/components.toml \
      --from /out \
      --dest /image --by-layer --all
