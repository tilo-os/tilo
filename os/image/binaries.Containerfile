# ---- The app store, without the host's files twice --------------------------
# The env-runtime the store preinstalls (the builder's install-env-runtime.sh)
# is built from the same frozen RPM snapshot as this rootfs, so most of its
# files are byte-identical to the host's /usr; but the store is its own layer,
# which no hard link can reach across, and every pull carried those bytes twice.
# Here, over the rootfs's packages, system and trust, a copy of the store leaves them out
# and names the host file for each in the image's host-sources; package-store
# imports them from there, every byte checked against the image's signed hash.
# The rootfs's final RUN checks each named host file again, after the last
# step that changes one, and puts back any that no longer matches.
FROM trust AS app-store
RUN --network=none \
    --mount=type=bind,from=native-builder-payload,source=/usr/share/tilo-apps,target=/run/tilo-apps-built,ro \
    --mount=type=bind,source=os/scripts/seed-env-runtime.py,target=/run/tilo-seed-env-runtime.py,ro \
    mkdir -p /out/usr/share \
    && cp -a /run/tilo-apps-built /out/usr/share/tilo-apps \
    && python3 /run/tilo-seed-env-runtime.py prune /out/usr/share/tilo-apps /

# ---- The Tilo binaries ------------------------------------------------------
# What the workspace compiles, arranged by os/components.toml, each tree its own
# layer: the rollback, recovery and extension helpers, the desktop binaries
# (with the side builds: the AirPlay sender, joycond, bootc,
# xwayland-satellite), the Windows components, the signed runtime bundle and
# the font catalog's names, the runtimes' publishers, the app store (left out of
# it what the layers below hold already). They are made over nothing, a scratch
# stage, so they don't depend on the layers below them: t builds this stage
# alone and puts its layers over the trust layer as they are, so the same binary
# gives the same layer whatever the variant. One `podman build` of the whole
# image (os/Containerfile) copies the stage into the rootfs whole.
FROM scratch AS binaries-out
COPY --from=component-files /image/rollback-files/ /
COPY --from=component-files /image/recovery-files/ /
COPY --from=component-files /image/extension-files/ /
# Everything above changes only with os/ inputs, packages or the kernel. The
# compiled binaries, the baked runtime and the app store change with almost
# every commit, so they are copied here, after the RPM transactions,
# initramfs and every other slow step, which a new build of the same os/
# inputs then takes from the layer cache. Nothing above runs them; the checks
# that do are here too. (tilo-rollback, which the initramfs carries, arrives
# earlier with rollback-files.)
#
# Compositor, FUSE driver, system apps, settings and model services. The
# enumerated scratch overlay preserves their individual paths and metadata but
# costs this large rootfs only one commit.
# Settings. Everything the compositor's quick-settings popover cannot
# reach lives here — Wi-Fi profiles, audio device routing, display scale,
# keyboard layout, power policy, touchpad — so an image without it is an
# image where those are unreachable on real hardware.
COPY --from=desktop-binaries / /
COPY --from=component-files /image/windows/ /
# ---- Baked signed runtime bundle -------------------------------------------
# The builder stage produced a signed content-addressed bundle from THIS
# image's own rootfs at /usr/lib/tilo-runtime/<release>+native.<digest> — the same
# path it takes here, because `tilo-install` had to find it there while
# packing the app store — and wrote its publisher pubkey to
# /out/runtime-bundle-publisher.hex. Install both here so:
#   * The launcher's `/usr/lib/tilo-runtime/<v>/` lookup succeeds out
#     of the box — no `tilo-install runtime install --from-host` needed
#     after first boot.
#   * The signature check the launcher performs before every mount
#     accepts this bake (the pubkey is the trust root).
# Trust list read by the launcher — one hex pubkey per line, `#` for
# comments. The baked publisher is the only entry by default; operators
# can extend it via `echo <hex> >> /etc/tilo/runtime-bundle-trusted-publishers.txt`.
# The same tree carries the font catalog's two other names, hard links to the
# runtime's fonts, so the bytes are in this one layer (runtime-image-payload),
# and the publishers of the runtimes it carries (runtime-bundle-publisher.hex,
# browser-runtime-publisher.hex), from the same builder result as the bundles.
COPY --from=runtime-image-payload /image/usr/ /usr/
# registry-runtime-publisher.hex is the publisher of the runtimes the Tilo
# registry offers. An app installed or updated from the Store is linked against
# the release runtime build, which is rarely this image's own bake, and the
# Store installs that runtime beside it; the launcher mounts it only when its
# publisher is trusted here.
COPY --from=native-builder-payload \
    /out/runtime-version \
    /out/registry-runtime-publisher.hex \
    /out/app-publisher.hex \
    /usr/share/tilo/
# ---- Preinstalled application store -----------------------------------------
# The registry + unpacked payloads the builder produced with `tilo-app-package pack` and
# `tilo-install install`. The launcher reads this store alongside the per-user
# one (crates/packaging/launcher/src/registry.rs), so the command palette lists
# these apps on a first boot with nobody having installed anything, and a user
# installing their own build of the same id shadows the preinstalled row rather
# than duplicating it.
#
# Read-only, being inside the image. That is the *only* way these packages
# differ from a user's: the sandbox is still built from the manifest, the
# payload is still ro-bound, and the single writable directory still comes from
# the launcher's per-room app dir. Nothing here is trusted for being shipped.
# Which key signed them, for auditing a built image. Not a trust root: package
# trust is the user's `~/.config/tilo/trusted-attestors.toml`, and this key is
# deliberately absent from it — a preinstalled package is self-signed by the
# image build, and claims nothing more.
COPY --from=app-store /out/usr/share/tilo-apps/ /usr/share/tilo-apps/

FROM trust AS binaries
COPY --from=binaries-out / /
