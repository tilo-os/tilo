# ---- The wallpapers ------------------------------------------------------------
# The pictures the desktop ships and their credits. They are binaries, so they
# are not in Git: tools/wallpapers.py fetches the files
# crates/platform/tilo-wallpapers/catalog.toml pins, verifying each by size
# and SHA-256, and writes the credits beside them; rootfs-base copies /out.
FROM ${BASE_REF} AS os-wallpapers
RUN --mount=type=cache,id=tilo-wallpaper-artifacts,target=/var/cache/tilo-wallpaper-artifacts \
    --mount=type=bind,source=tools/wallpapers.py,target=/tilo-src/tools/wallpapers.py,ro \
    --mount=type=bind,source=crates/platform/tilo-wallpapers/catalog.toml,target=/tilo-src/crates/platform/tilo-wallpapers/catalog.toml,ro \
    python3 /tilo-src/tools/wallpapers.py prepare /out \
        --cache /var/cache/tilo-wallpaper-artifacts
